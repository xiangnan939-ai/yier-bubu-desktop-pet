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
use tauri::{ipc::Response, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use xcap::Monitor;

mod cloud_binding;
mod updates;

use cloud_binding::{
    BindingManager, BindingStatus, PairingResult, RealtimeCredentials, SignalProcessResult,
    SignedSignal,
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
async fn sync_binding_recovery(
    manager: tauri::State<'_, BindingManager>,
) -> Result<PairingResult, String> {
    manager.sync_binding_recovery().await
}

#[tauri::command]
async fn realtime_credentials(
    force_refresh: bool,
    manager: tauri::State<'_, BindingManager>,
) -> Result<RealtimeCredentials, String> {
    manager.credentials(force_refresh).await
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
fn open_viewer_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window("viewer") {
        existing.destroy().map_err(|error| error.to_string())?;
    }
    WebviewWindowBuilder::new(&app, "viewer", WebviewUrl::App("/?mode=viewer".into()))
        .title("看看TA在干嘛")
        .inner_size(1_100.0, 720.0)
        .center()
        .decorations(true)
        .transparent(false)
        .build()
        .map(|_| ())
        .map_err(|error| format!("无法创建远程画面窗口：{error}"))
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
            eMultimedia, eRender, AudioSessionStateActive, IAudioSessionManager2,
            IMMDeviceEnumerator, MMDeviceEnumerator,
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
        let Ok(manager): windows::core::Result<IAudioSessionManager2> =
            device.Activate(CLSCTX_ALL, None)
        else {
            return false;
        };
        let Ok(sessions) = manager.GetSessionEnumerator() else {
            return false;
        };
        let Ok(count) = sessions.GetCount() else {
            return false;
        };
        (0..count).any(|index| {
            sessions
                .GetSession(index)
                .and_then(|session| session.GetState())
                .is_ok_and(|state| state == AudioSessionStateActive)
        })
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

fn capture_primary_monitor() -> Result<Vec<u8>, String> {
    let monitors = Monitor::all().map_err(|error| error.to_string())?;
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| "没有找到可捕获的显示器".to_string())?;
    let captured = monitor.capture_image().map_err(|error| error.to_string())?;
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
    // The realtime renderer only talks to Tencent Chat/TRTC. A stale Windows
    // loopback proxy would otherwise keep the pet bound but permanently
    // offline (WebView2 reports ERR_PROXY_CONNECTION_FAILED). Rust-side API
    // and updater traffic are unaffected by this WebView-only argument.
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
            sync_binding_recovery,
            realtime_credentials,
            make_realtime_signal,
            process_realtime_signal,
            request_unbind,
            respond_unbind,
            screen_share_active,
            set_screen_share_active,
            capture_screen_frame,
            open_viewer_window,
            system_audio_playing,
            system_idle_seconds,
            device_status,
            updates::update_configuration,
            updates::check_app_update,
            updates::install_app_update,
            updates::installed_asset_pack,
            updates::check_and_install_asset_update
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
