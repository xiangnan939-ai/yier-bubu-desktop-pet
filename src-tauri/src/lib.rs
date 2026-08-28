use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use image::codecs::jpeg::JpegEncoder;
use serde::Serialize;
use serde_json::Value;
use tauri::{ipc::Response, Emitter, Manager};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
#[cfg(not(target_os = "macos"))]
use xcap::Monitor;

#[cfg(target_os = "macos")]
#[allow(deprecated)]
use objc2_core_graphics::{
    CGDataProvider, CGDisplayCreateImage, CGImage, CGMainDisplayID, CGPreflightScreenCaptureAccess,
    CGRequestScreenCaptureAccess,
};

mod cloud_binding;
mod updates;

use cloud_binding::{
    BindingManager, BindingStatus, PairingResult, SignalProcessResult, SignedSignal,
};

const MAX_STREAM_WIDTH: u32 = 1_280;

#[derive(Clone, Default)]
struct ScreenServerState {
    active_viewers: Arc<AtomicUsize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppProfile {
    role: &'static str,
    pet_name: &'static str,
    partner_name: &'static str,
    remote_menu_label: &'static str,
    platform: &'static str,
}

#[tauri::command]
fn app_profile() -> AppProfile {
    #[cfg(target_os = "macos")]
    return AppProfile {
        role: "yier",
        pet_name: "一二",
        partner_name: "布布",
        remote_menu_label: "看看TA在干嘛",
        platform: "macos",
    };

    #[cfg(target_os = "windows")]
    return AppProfile {
        role: "bubu",
        pet_name: "布布",
        partner_name: "一二",
        remote_menu_label: "看看TA在干嘛",
        platform: "windows",
    };

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    AppProfile {
        role: "yier",
        pet_name: "一二",
        partner_name: "布布",
        remote_menu_label: "看看TA在干嘛",
        platform: "unsupported",
    }
}

#[tauri::command]
fn screen_share_active(state: tauri::State<'_, ScreenServerState>) -> bool {
    state.active_viewers.load(Ordering::Relaxed) > 0
}

#[tauri::command]
async fn binding_status(
    manager: tauri::State<'_, BindingManager>,
) -> Result<BindingStatus, String> {
    Ok(manager.status().await)
}

#[tauri::command]
async fn pair_device(
    passphrase: String,
    manager: tauri::State<'_, BindingManager>,
) -> Result<PairingResult, String> {
    manager.pair(passphrase).await
}

#[tauri::command]
async fn make_realtime_signal(
    message_type: String,
    payload: Value,
    manager: tauri::State<'_, BindingManager>,
) -> Result<SignedSignal, String> {
    manager.make_signal(message_type, payload).await
}

#[tauri::command]
async fn process_realtime_signal(
    signal: SignedSignal,
    manager: tauri::State<'_, BindingManager>,
) -> Result<SignalProcessResult, String> {
    manager.process_signal(signal).await
}

#[tauri::command]
async fn request_unbind(manager: tauri::State<'_, BindingManager>) -> Result<SignedSignal, String> {
    manager.request_unbind().await
}

#[tauri::command]
async fn respond_unbind(
    approve: bool,
    manager: tauri::State<'_, BindingManager>,
) -> Result<SignedSignal, String> {
    manager.respond_unbind(approve).await
}

#[tauri::command]
fn set_screen_share_active(active: bool, state: tauri::State<'_, ScreenServerState>) {
    state
        .active_viewers
        .store(usize::from(active), Ordering::Relaxed);
}

#[tauri::command]
fn capture_screen_frame() -> Result<Response, String> {
    capture_primary_monitor().map(Response::new)
}

#[tauri::command]
fn ensure_screen_capture_permission() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess() {
            return Ok(());
        }
        return Err(
            "Mac 尚未允许桌宠录制屏幕，请在系统设置的隐私与安全性中允许后重新启动桌宠".to_string(),
        );
    }
    #[cfg(not(target_os = "macos"))]
    Ok(())
}

#[tauri::command]
fn close_viewer_window(app: tauri::AppHandle, _session: Option<Value>) -> Result<(), String> {
    // Destroy immediately. The main window observes `tauri://destroyed` and
    // sends the stop signal without intercepting the native close request.
    if let Some(viewer) = app.get_webview_window("viewer") {
        viewer.destroy().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn close_pet_menu_windows(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("pet-menu") {
        let _ = window.destroy();
    }
}

#[tauri::command]
fn trigger_pet_menu_action(app: tauri::AppHandle, action: String) -> Result<(), String> {
    match action.as_str() {
        "viewer" | "settings" => app
            .emit_to("main", "pet-menu-action", action)
            .map_err(|error| error.to_string()),
        _ => Err("未知的菜单操作".into()),
    }
}

#[tauri::command]
fn set_pet_status(app: tauri::AppHandle, status: String) -> Result<(), String> {
    const ALLOWED: [&str; 8] = [
        "free", "happy", "angry", "dance", "eat", "drink", "sleep", "work",
    ];
    if !ALLOWED.contains(&status.as_str()) {
        return Err("未知的桌宠状态".into());
    }
    // Emit before returning to the submenu. This guarantees the main window
    // receives the selection before the submenu asks the native process to
    // destroy both menu windows.
    app.emit_to("main", "pet-status-selected", status)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn pet_menu_has_focus(app: tauri::AppHandle) -> bool {
    app.get_webview_window("pet-menu")
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(false)
}

#[tauri::command]
fn pet_menu_pointer_inside(app: tauri::AppHandle) -> bool {
    let Ok(cursor) = app.cursor_position() else {
        return false;
    };

    let Some(window) = app.get_webview_window("pet-menu") else {
        return false;
    };
    let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return false;
    };
    let padding = 8.0;
    let left = position.x as f64 - padding;
    let top = position.y as f64 - padding;
    let right = position.x as f64 + size.width as f64 + padding;
    let bottom = position.y as f64 + size.height as f64 + padding;
    cursor.x >= left && cursor.x <= right && cursor.y >= top && cursor.y <= bottom
}

#[tauri::command]
async fn wait_for_primary_mouse_release() {
    #[cfg(target_os = "windows")]
    {
        use std::time::{Duration, Instant};
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};

        let is_pressed = || unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 };

        // The WebView loses pointer ownership as soon as Tauri enters the
        // native window-moving loop. Observe the physical button instead of
        // relying on pointerup or on startDragging()'s Promise lifetime.
        let pressed_deadline = Instant::now() + Duration::from_millis(300);
        while !is_pressed() && Instant::now() < pressed_deadline {
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
        if !is_pressed() {
            return;
        }

        let release_deadline = Instant::now() + Duration::from_secs(60);
        while is_pressed() && Instant::now() < release_deadline {
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_audio_playing() -> bool {
    use objc2_core_audio::{
        kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
        AudioObjectGetPropertyData, AudioObjectPropertyAddress,
    };
    use std::{ffi::c_void, mem::size_of, ptr::NonNull};

    unsafe {
        let default_output_address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut output_device = 0_u32;
        let mut output_device_size = size_of::<u32>() as u32;
        let output_status = AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            NonNull::from(&default_output_address),
            0,
            std::ptr::null(),
            NonNull::from(&mut output_device_size),
            NonNull::new_unchecked((&mut output_device as *mut u32).cast::<c_void>()),
        );
        if output_status != 0 || output_device == 0 {
            return false;
        }

        let running_address = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut running = 0_u32;
        let mut running_size = size_of::<u32>() as u32;
        AudioObjectGetPropertyData(
            output_device,
            NonNull::from(&running_address),
            0,
            std::ptr::null(),
            NonNull::from(&mut running_size),
            NonNull::new_unchecked((&mut running as *mut u32).cast::<c_void>()),
        ) == 0
            && running != 0
    }
}

#[cfg(target_os = "windows")]
fn platform_audio_playing() -> bool {
    use windows::Win32::{
        Media::Audio::{
            eMultimedia, eRender, Endpoints::IAudioMeterInformation, IMMDeviceEnumerator,
            MMDeviceEnumerator,
        },
        System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED},
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(device_enumerator): windows::core::Result<IMMDeviceEnumerator> =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return false;
        };
        let Ok(device) = device_enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) else {
            return false;
        };
        let Ok(meter): windows::core::Result<IAudioMeterInformation> =
            device.Activate(CLSCTX_ALL, None)
        else {
            return false;
        };
        // An AudioSession can remain Active after music has stopped. The output
        // endpoint peak reflects audible samples instead, while the frontend's
        // three-sample debounce filters normal brief gaps between notes/tracks.
        meter.GetPeakValue().is_ok_and(|peak| peak > 0.000_5)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_audio_playing() -> bool {
    false
}

#[tauri::command]
fn system_audio_playing() -> bool {
    platform_audio_playing()
}

#[cfg(target_os = "macos")]
fn platform_idle_seconds() -> u64 {
    use objc2_core_graphics::{CGEventSource, CGEventSourceStateID, CGEventType};

    CGEventSource::seconds_since_last_event_type(
        CGEventSourceStateID::CombinedSessionState,
        CGEventType(u32::MAX),
    )
    .max(0.0) as u64
}

#[cfg(target_os = "windows")]
fn platform_idle_seconds() -> u64 {
    use std::mem::size_of;
    use windows::Win32::{
        System::SystemInformation::GetTickCount64,
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    };

    unsafe {
        let mut last_input = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if !GetLastInputInfo(&mut last_input).as_bool() {
            return 0;
        }
        let current_tick = GetTickCount64() as u32;
        u64::from(current_tick.wrapping_sub(last_input.dwTime)) / 1_000
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_idle_seconds() -> u64 {
    0
}

#[tauri::command]
fn system_idle_seconds() -> u64 {
    platform_idle_seconds()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceStatus {
    battery_percentage: Option<u8>,
    charging: bool,
    hot: bool,
}

#[cfg(target_os = "macos")]
fn platform_hot() -> bool {
    use objc2_foundation::{NSProcessInfo, NSProcessInfoThermalState};

    matches!(
        NSProcessInfo::processInfo().thermalState(),
        NSProcessInfoThermalState::Serious | NSProcessInfoThermalState::Critical
    )
}

#[cfg(target_os = "windows")]
fn platform_hot() -> bool {
    use std::sync::Mutex;
    use windows::Win32::{Foundation::FILETIME, System::Threading::GetSystemTimes};

    static PREVIOUS_TIMES: Mutex<Option<(u64, u64, u64)>> = Mutex::new(None);

    fn as_u64(value: FILETIME) -> u64 {
        (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
    }

    unsafe {
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_err() {
            return false;
        }
        let current = (as_u64(idle), as_u64(kernel), as_u64(user));
        let Ok(mut previous) = PREVIOUS_TIMES.lock() else {
            return false;
        };
        let Some(old) = previous.replace(current) else {
            return false;
        };
        let idle_delta = current.0.saturating_sub(old.0);
        let total_delta = current
            .1
            .saturating_sub(old.1)
            .saturating_add(current.2.saturating_sub(old.2));
        total_delta > 0
            && (total_delta.saturating_sub(idle_delta)) as f64 / total_delta as f64 >= 0.85
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_hot() -> bool {
    false
}

#[tauri::command]
fn device_status() -> DeviceStatus {
    use battery::{units::ratio::percent, State};

    let battery = battery::Manager::new().ok().and_then(|manager| {
        manager
            .batteries()
            .ok()?
            .filter_map(Result::ok)
            .next()
            .map(|value| {
                let percentage = value.state_of_charge().get::<percent>().round();
                (
                    Some(percentage.clamp(0.0, 100.0) as u8),
                    value.state() == State::Charging,
                )
            })
    });
    let (battery_percentage, charging) = battery.unwrap_or((None, false));
    DeviceStatus {
        battery_percentage,
        charging,
        hot: platform_hot(),
    }
}

#[cfg(target_os = "macos")]
fn capture_macos_display() -> Result<image::RgbaImage, String> {
    // xcap's macOS snapshot implementation is based on CGWindowListCreateImage.
    // Recent macOS versions may return only this app's transparent pet window.
    // Capturing the physical main display directly produces the composited screen.
    #[allow(deprecated)]
    let captured = CGDisplayCreateImage(CGMainDisplayID())
        .ok_or_else(|| "Mac 无法读取主显示器画面，请重新开启录屏权限并重启桌宠".to_string())?;
    let width = CGImage::width(Some(&captured));
    let height = CGImage::height(Some(&captured));
    let bytes_per_row = CGImage::bytes_per_row(Some(&captured));
    if width == 0 || height == 0 || CGImage::bits_per_pixel(Some(&captured)) != 32 {
        return Err("Mac 返回了不受支持的屏幕画面格式".to_string());
    }
    let provider = CGImage::data_provider(Some(&captured))
        .ok_or_else(|| "Mac 屏幕画面没有可读取的数据".to_string())?;
    let data = CGDataProvider::data(Some(&provider))
        .ok_or_else(|| "Mac 屏幕画面数据读取失败".to_string())?
        .to_vec();
    if bytes_per_row < width * 4 || data.len() < bytes_per_row * height {
        return Err("Mac 屏幕画面数据不完整".to_string());
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in data.chunks_exact(bytes_per_row).take(height) {
        rgba.extend_from_slice(&row[..width * 4]);
    }
    // CGDisplayCreateImage supplies 32-bit little-endian BGRA pixels.
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    image::RgbaImage::from_raw(width as u32, height as u32, rgba)
        .ok_or_else(|| "Mac 屏幕画面转换失败".to_string())
}

fn capture_primary_monitor() -> Result<Vec<u8>, String> {
    #[cfg(target_os = "macos")]
    let captured = {
        if !CGPreflightScreenCaptureAccess() {
            return Err(
                "Mac 尚未允许桌宠录制屏幕，请在系统设置的隐私与安全性中允许后重新启动桌宠"
                    .to_string(),
            );
        }
        capture_macos_display()?
    };
    #[cfg(not(target_os = "macos"))]
    let captured = {
        let monitors = Monitor::all().map_err(|error| error.to_string())?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .ok_or_else(|| "没有找到可捕获的显示器".to_string())?;
        monitor.capture_image().map_err(|error| error.to_string())?
    };
    let image = if captured.width() > MAX_STREAM_WIDTH {
        let height = captured.height() * MAX_STREAM_WIDTH / captured.width();
        image::imageops::resize(
            &captured,
            MAX_STREAM_WIDTH,
            height,
            image::imageops::FilterType::Triangle,
        )
    } else {
        captured
    };
    let mut bytes = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut bytes, 68)
        .encode_image(&image)
        .map_err(|error| error.to_string())?;
    Ok(bytes.into_inner())
}

#[cfg(any(target_os = "windows", test))]
fn direct_webview_network_arguments(current: &str) -> String {
    if current
        .split_whitespace()
        .any(|argument| argument == "--no-proxy-server")
    {
        current.trim().to_string()
    } else if current.trim().is_empty() {
        "--no-proxy-server".into()
    } else {
        format!("{} --no-proxy-server", current.trim())
    }
}

#[cfg(target_os = "windows")]
fn configure_webview_network() {
    // WebView2 must reach the two embedded MQTT signaling routes directly.
    // A stale loopback proxy would otherwise leave the pet bound but offline.
    const KEY: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";
    let current = std::env::var(KEY).unwrap_or_default();
    std::env::set_var(KEY, direct_webview_network_arguments(&current));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "windows")]
    configure_webview_network();

    let screen_state = ScreenServerState::default();
    let mut builder = tauri::Builder::default()
        .manage(screen_state)
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            activate_app(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                window.set_background_color(Some(tauri::utils::config::Color(0, 0, 0, 0)))?;
            }
            let skip_autostart = std::env::var_os("YIER_BUBU_SKIP_AUTOSTART").is_some();
            if !cfg!(debug_assertions) && !skip_autostart {
                let autostart = app.autolaunch();
                if !autostart.is_enabled().unwrap_or(false) {
                    autostart.enable()?;
                }
            }
            let binding_manager =
                BindingManager::new(app.handle().clone(), app.path().app_data_dir()?)
                    .map_err(std::io::Error::other)?;
            app.manage(binding_manager);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_profile,
            binding_status,
            pair_device,
            make_realtime_signal,
            process_realtime_signal,
            request_unbind,
            respond_unbind,
            screen_share_active,
            set_screen_share_active,
            capture_screen_frame,
            ensure_screen_capture_permission,
            close_viewer_window,
            close_pet_menu_windows,
            trigger_pet_menu_action,
            pet_menu_has_focus,
            pet_menu_pointer_inside,
            set_pet_status,
            wait_for_primary_mouse_release,
            system_audio_playing,
            system_idle_seconds,
            device_status,
            updates::update_configuration,
            updates::check_app_update,
            updates::install_app_update
        ]);

    if let (Some(endpoint), Some(public_key)) = (
        updates::APP_UPDATE_ENDPOINT.filter(|value| !value.trim().is_empty()),
        updates::APP_UPDATE_PUBLIC_KEY.filter(|value| !value.trim().is_empty()),
    ) {
        match endpoint.parse::<tauri::Url>() {
            Ok(_) => {
                let updater = tauri_plugin_updater::Builder::new().pubkey(public_key);
                builder = builder.plugin(updater.build());
            }
            Err(error) => eprintln!("程序更新地址无效，已停用自动更新：{error}"),
        }
    }

    let app = builder
        .build(tauri::generate_context!())
        .expect("构建一二布布私人桌宠失败");
    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            activate_app(app);
        }
    });
}

fn activate_app(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("binding") {
        let _ = window.show();
        let _ = window.set_focus();
    } else if let Some(window) = app.get_webview_window("main") {
        let _ = window.emit("activate-app", ());
    }
}

#[cfg(test)]
mod tests {
    use super::direct_webview_network_arguments;

    #[test]
    fn webview_network_bypasses_stale_system_proxy_once() {
        assert_eq!(direct_webview_network_arguments(""), "--no-proxy-server");
        assert_eq!(
            direct_webview_network_arguments("--remote-debugging-port=9222"),
            "--remote-debugging-port=9222 --no-proxy-server"
        );
        assert_eq!(
            direct_webview_network_arguments("--no-proxy-server"),
            "--no-proxy-server"
        );
    }
}
