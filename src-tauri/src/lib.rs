use std::{
    io::Cursor,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use image::codecs::jpeg::JpegEncoder;
use serde::Serialize;
use tauri::Manager;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use xcap::Monitor;

mod binding;
mod updates;

use binding::{
    BindingManager, BindingStatus, PairFinishRequest, PairFinishResponse, PairStartRequest,
    PairStartResponse, PairingResult, ScreenConnectionInfo, ScreenServerAuth, SignedUnbindRequest,
    UnbindAck, UnbindApproval,
};

const SCREEN_PORT: u16 = 39_821;
const FRAME_INTERVAL: Duration = Duration::from_millis(180);
const MAX_STREAM_WIDTH: u32 = 1_600;

#[derive(Clone, Default)]
struct ScreenServerState {
    active_viewers: Arc<AtomicUsize>,
    binding: Arc<RwLock<Option<BindingManager>>>,
}

struct ViewerGuard(Arc<AtomicUsize>);

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
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
async fn screen_connection_info(
    manager: tauri::State<'_, BindingManager>,
) -> Result<ScreenConnectionInfo, String> {
    manager.screen_connection_info().await
}

#[tauri::command]
async fn verify_screen_server_auth(
    auth: ScreenServerAuth,
    manager: tauri::State<'_, BindingManager>,
) -> Result<bool, String> {
    manager.verify_screen_server_auth(auth).await
}

#[tauri::command]
async fn request_unbind(manager: tauri::State<'_, BindingManager>) -> Result<String, String> {
    manager.request_unbind().await
}

#[tauri::command]
async fn respond_unbind(
    approve: bool,
    manager: tauri::State<'_, BindingManager>,
) -> Result<String, String> {
    manager.respond_unbind(approve).await
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

async fn screen_route(
    State(state): State<ScreenServerState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let manager = match screen_binding_manager(&state) {
        Ok(manager) => manager,
        Err(error) => return (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
    };
    upgrade
        .on_upgrade(move |socket| stream_screen(socket, state, manager, remote))
        .into_response()
}

async fn stream_screen(
    mut socket: WebSocket,
    state: ScreenServerState,
    manager: BindingManager,
    remote: SocketAddr,
) {
    let auth_message = tokio::time::timeout(Duration::from_secs(8), socket.recv()).await;
    let auth = match auth_message {
        Ok(Some(Ok(Message::Text(value)))) => serde_json::from_str(value.as_str()),
        _ => {
            let _ = socket.send(Message::Text("设备认证超时".into())).await;
            return;
        }
    };
    let auth = match auth {
        Ok(auth) => auth,
        Err(_) => {
            let _ = socket.send(Message::Text("设备认证数据无效".into())).await;
            return;
        }
    };
    let server_auth = match manager.authorize_screen_client(remote.ip(), &auth).await {
        Ok(auth) => auth,
        Err(error) => {
            let _ = socket.send(Message::Text(error.into())).await;
            return;
        }
    };
    let Ok(server_auth) = serde_json::to_string(&server_auth) else {
        return;
    };
    if socket
        .send(Message::Text(server_auth.into()))
        .await
        .is_err()
    {
        return;
    }
    state.active_viewers.fetch_add(1, Ordering::Relaxed);
    let _viewer_guard = ViewerGuard(state.active_viewers);
    loop {
        let frame = tauri::async_runtime::spawn_blocking(capture_primary_monitor).await;
        let Ok(Ok(frame)) = frame else {
            let _ = socket
                .send(Message::Text("暂时无法捕获屏幕，请检查系统权限".into()))
                .await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        if socket.send(Message::Binary(frame.into())).await.is_err() {
            break;
        }
        tokio::time::sleep(FRAME_INTERVAL).await;
    }
}

async fn pair_start_route(
    State(state): State<ScreenServerState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Json(request): Json<PairStartRequest>,
) -> Result<Json<PairStartResponse>, (StatusCode, String)> {
    screen_binding_manager(&state)
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error))?
        .receive_pair_start(remote.ip(), request)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::UNAUTHORIZED, error))
}

async fn pair_finish_route(
    State(state): State<ScreenServerState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Json(request): Json<PairFinishRequest>,
) -> Result<Json<PairFinishResponse>, (StatusCode, String)> {
    screen_binding_manager(&state)
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error))?
        .receive_pair_finish(remote.ip(), request)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::UNAUTHORIZED, error))
}

async fn unbind_request_route(
    State(state): State<ScreenServerState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Json(request): Json<SignedUnbindRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    screen_binding_manager(&state)
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error))?
        .receive_unbind_request(remote.ip(), request)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|error| (StatusCode::UNAUTHORIZED, error))
}

async fn unbind_approve_route(
    State(state): State<ScreenServerState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Json(approval): Json<UnbindApproval>,
) -> Result<Json<UnbindAck>, (StatusCode, String)> {
    screen_binding_manager(&state)
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error))?
        .receive_unbind_approval(remote.ip(), approval)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::UNAUTHORIZED, error))
}

async fn unbind_reject_route(
    State(state): State<ScreenServerState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Json(rejection): Json<UnbindAck>,
) -> Result<StatusCode, (StatusCode, String)> {
    screen_binding_manager(&state)
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error))?
        .receive_unbind_rejection(remote.ip(), rejection)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|error| (StatusCode::UNAUTHORIZED, error))
}

fn screen_binding_manager(state: &ScreenServerState) -> Result<BindingManager, String> {
    state
        .binding
        .read()
        .map_err(|_| "绑定服务状态不可用".to_string())?
        .clone()
        .ok_or_else(|| "绑定服务尚未初始化".to_string())
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

async fn run_screen_server(state: ScreenServerState) {
    let app = Router::new()
        .route("/screen", get(screen_route))
        .route("/pair/start", post(pair_start_route))
        .route("/pair/finish", post(pair_finish_route))
        .route("/unbind/request", post(unbind_request_route))
        .route("/unbind/approve", post(unbind_approve_route))
        .route("/unbind/reject", post(unbind_reject_route))
        .with_state(state.clone());
    loop {
        let manager = match screen_binding_manager(&state) {
            Ok(manager) => manager,
            Err(error) => {
                eprintln!("绑定服务尚未就绪：{error}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let ip = match manager.local_tailscale_ip().await {
            Ok(ip) => ip,
            Err(error) => {
                eprintln!("等待 Tailscale 网络：{error}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let address = format!("{ip}:{SCREEN_PORT}");
        match tokio::net::TcpListener::bind(&address).await {
            Ok(listener) => {
                if let Err(error) = axum::serve(
                    listener,
                    app.clone()
                        .into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                {
                    eprintln!("屏幕服务退出：{error}");
                }
            }
            Err(error) => eprintln!("无法监听屏幕服务 {address}：{error}"),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let screen_state = ScreenServerState::default();
    let server_state = screen_state.clone();
    let mut builder = tauri::Builder::default()
        .manage(screen_state)
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
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
            *server_state
                .binding
                .write()
                .map_err(|_| std::io::Error::other("无法初始化绑定服务"))? =
                Some(binding_manager.clone());
            app.manage(binding_manager);
            tauri::async_runtime::spawn(run_screen_server(server_state));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_profile,
            binding_status,
            pair_device,
            screen_connection_info,
            verify_screen_server_auth,
            request_unbind,
            respond_unbind,
            screen_share_active,
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

    builder
        .run(tauri::generate_context!())
        .expect("启动一二布布私人桌宠失败");
}
