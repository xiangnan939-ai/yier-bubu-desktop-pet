use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};

const DEFAULT_APP_UPDATE_ENDPOINT: &str =
    "https://github.com/xiangnan939-ai/yier-bubu-desktop-pet/releases/latest/download/latest.json";
const DEFAULT_APP_UPDATE_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEI4MTQ3OTVCREZFNjEyRjUKUldUMUV1YmZXM2tVdU0rYWhPVEljQ1dwNENPeEk3bm01NnRHL1pRY081RzNZZDdKb3pqMWxBWVgK";

// These values are public. Built-in defaults keep local signed builds capable
// of updating even when CI-only environment variables are absent.
pub const APP_UPDATE_ENDPOINT: Option<&str> = match option_env!("YIER_BUBU_APP_UPDATE_ENDPOINT") {
    Some(value) => Some(value),
    None => Some(DEFAULT_APP_UPDATE_ENDPOINT),
};
pub const APP_UPDATE_PUBLIC_KEY: Option<&str> = match option_env!("YIER_BUBU_APP_UPDATE_PUBLIC_KEY")
{
    Some(value) => Some(value),
    None => Some(DEFAULT_APP_UPDATE_PUBLIC_KEY),
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConfiguration {
    current_version: &'static str,
    app_update_enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateCheck {
    available: bool,
    current_version: String,
    version: Option<String>,
    notes: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProgress {
    update_type: &'static str,
    phase: &'static str,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}

fn configured(value: Option<&str>) -> bool {
    value.is_some_and(|item| !item.trim().is_empty())
}

fn emit_progress(
    app: &AppHandle,
    phase: &'static str,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
) {
    let _ = app.emit(
        "update-download-progress",
        UpdateProgress {
            update_type: "app",
            phase,
            downloaded_bytes,
            total_bytes,
        },
    );
}

#[tauri::command]
pub fn update_configuration() -> UpdateConfiguration {
    UpdateConfiguration {
        current_version: env!("CARGO_PKG_VERSION"),
        app_update_enabled: configured(APP_UPDATE_ENDPOINT) && configured(APP_UPDATE_PUBLIC_KEY),
    }
}

fn updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let endpoint = APP_UPDATE_ENDPOINT
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "程序发布地址尚未配置".to_string())?;
    let url = endpoint
        .parse()
        .map_err(|_| "程序更新地址无效".to_string())?;
    app.updater_builder()
        .timeout(Duration::from_secs(15))
        .endpoints(vec![url])
        .map_err(|error| format!("程序更新地址无效：{error}"))?
        .build()
        .map_err(|error| format!("初始化程序更新器失败：{error}"))
}

async fn checked_update(app: &AppHandle) -> Result<Option<Update>, String> {
    let mut update = updater(app)?
        .check()
        .await
        .map_err(|error| error.to_string())?;
    if let Some(value) = update.as_mut() {
        value.timeout = Some(Duration::from_secs(5 * 60));
    }
    Ok(update)
}

#[tauri::command]
pub async fn check_app_update(app: AppHandle) -> Result<AppUpdateCheck, String> {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let update = checked_update(&app)
        .await
        .map_err(|error| format!("检查程序更新失败：{error}"))?;
    Ok(match update {
        Some(value) => AppUpdateCheck {
            available: true,
            current_version,
            version: Some(value.version.to_string()),
            notes: value.body,
        },
        None => AppUpdateCheck {
            available: false,
            current_version,
            version: None,
            notes: None,
        },
    })
}

#[tauri::command]
pub async fn install_app_update(app: AppHandle) -> Result<(), String> {
    let update = checked_update(&app)
        .await
        .map_err(|error| format!("检查程序更新失败：{error}"))?
        .ok_or_else(|| "当前程序已经是最新版".to_string())?;
    let progress_app = app.clone();
    let finish_app = app.clone();
    let mut downloaded_bytes = 0_u64;
    update
        .download_and_install(
            move |chunk_length, total_bytes| {
                downloaded_bytes = downloaded_bytes.saturating_add(chunk_length as u64);
                emit_progress(&progress_app, "downloading", downloaded_bytes, total_bytes);
            },
            move || emit_progress(&finish_app, "installing", 0, None),
        )
        .await
        .map_err(|error| format!("下载或安装程序更新失败：{error}"))?;
    emit_progress(&app, "complete", 1, Some(1));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_build_keeps_program_update_enabled() {
        assert!(configured(APP_UPDATE_ENDPOINT));
        assert!(configured(APP_UPDATE_PUBLIC_KEY));
    }
}
