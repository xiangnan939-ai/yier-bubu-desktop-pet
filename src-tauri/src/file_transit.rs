use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Window};
use uuid::Uuid;

const TRANSIT_DIR_NAME: &str = "file-transit";
const TRANSIT_METADATA_NAME: &str = "holding.json";
const DRAG_PREVIEW_PNG: &[u8] = include_bytes!("../icons/32x32.png");

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTransitStatus {
    holding: bool,
    file_name: Option<String>,
    original_path: Option<String>,
    stored_path: Option<String>,
    size_bytes: Option<u64>,
    placed_at_ms: Option<u128>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTransitDragResult {
    dropped: bool,
    message: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransitMetadata {
    file_name: String,
    original_path: PathBuf,
    stored_path: PathBuf,
    size_bytes: u64,
    placed_at_ms: u128,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn empty_status() -> FileTransitStatus {
    FileTransitStatus {
        holding: false,
        file_name: None,
        original_path: None,
        stored_path: None,
        size_bytes: None,
        placed_at_ms: None,
    }
}

fn transit_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|error| error.to_string())
        .map(|path| path.join(TRANSIT_DIR_NAME))
}

fn metadata_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(transit_dir(app)?.join(TRANSIT_METADATA_NAME))
}

fn metadata_to_status(metadata: &TransitMetadata) -> FileTransitStatus {
    FileTransitStatus {
        holding: true,
        file_name: Some(metadata.file_name.clone()),
        original_path: Some(metadata.original_path.to_string_lossy().into_owned()),
        stored_path: Some(metadata.stored_path.to_string_lossy().into_owned()),
        size_bytes: Some(metadata.size_bytes),
        placed_at_ms: Some(metadata.placed_at_ms),
    }
}

fn read_metadata(app: &AppHandle) -> Result<Option<TransitMetadata>, String> {
    let path = metadata_path(app)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map(Some).map_err(|error| {
        let _ = fs::remove_file(&path);
        format!("文件中转站记录损坏，已清理：{error}")
    })
}

fn write_metadata(app: &AppHandle, metadata: &TransitMetadata) -> Result<(), String> {
    let directory = transit_dir(app)?;
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let payload = serde_json::to_vec_pretty(metadata).map_err(|error| error.to_string())?;
    fs::write(metadata_path(app)?, payload).map_err(|error| error.to_string())
}

fn status_from_disk(app: &AppHandle) -> Result<FileTransitStatus, String> {
    let Some(metadata) = read_metadata(app)? else {
        return Ok(empty_status());
    };
    if metadata.stored_path.is_file() {
        Ok(metadata_to_status(&metadata))
    } else {
        let _ = fs::remove_file(metadata_path(app)?);
        Ok(empty_status())
    }
}

fn emit_status(app: &AppHandle) {
    if let Ok(status) = status_from_disk(app) {
        let _ = app.emit_to("main", "file-transit-updated", status);
    }
}

fn source_filename(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| "无法识别文件名".to_string())
}

fn unique_stored_path(directory: &Path, original_name: &str) -> PathBuf {
    directory.join(format!("{}-{}-{}", now_ms(), Uuid::new_v4(), original_name))
}

fn move_file_cut_semantics(source: &Path, destination: &Path) -> Result<(), String> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(rename_error) => {
            fs::copy(source, destination).map_err(|copy_error| {
                format!(
                    "无法剪切文件：移动失败（{rename_error}），跨磁盘复制也失败（{copy_error}）"
                )
            })?;
            let source_len = fs::metadata(source)
                .map_err(|error| error.to_string())?
                .len();
            let destination_len = fs::metadata(destination)
                .map_err(|error| error.to_string())?
                .len();
            if source_len != destination_len {
                let _ = fs::remove_file(destination);
                return Err("跨磁盘剪切校验失败，原文件已保留".to_string());
            }
            if let Err(remove_error) = fs::remove_file(source) {
                let _ = fs::remove_file(destination);
                return Err(format!(
                    "无法从原位置移除文件，已回滚中转文件：{remove_error}"
                ));
            }
            Ok(())
        }
    }
}

fn restore_moved_file_best_effort(stored_path: &Path, original_path: &Path) {
    if !stored_path.exists() || original_path.exists() {
        return;
    }
    if let Some(parent) = original_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::rename(stored_path, original_path).is_ok() {
        return;
    }
    if fs::copy(stored_path, original_path).is_ok() {
        let _ = fs::remove_file(stored_path);
    }
}

fn clear_after_success(app: &AppHandle) -> Result<(), String> {
    if let Some(metadata) = read_metadata(app)? {
        if metadata.stored_path.exists() {
            fs::remove_file(&metadata.stored_path)
                .map_err(|error| format!("文件已经投放，但中转站源文件清理失败：{error}"))?;
        }
    }
    let path = metadata_path(app)?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    emit_status(app);
    Ok(())
}

#[tauri::command]
pub fn file_transit_status(app: AppHandle) -> Result<FileTransitStatus, String> {
    status_from_disk(&app)
}

#[tauri::command]
pub fn file_transit_store(paths: Vec<String>, app: AppHandle) -> Result<FileTransitStatus, String> {
    if paths.len() != 1 {
        return Err("一次只能暂存一个文件".to_string());
    }
    if status_from_disk(&app)?.holding {
        return Err("桌宠这里已经有一个文件了，先取出来再放新的吧".to_string());
    }

    let source = PathBuf::from(&paths[0]);
    let source =
        fs::canonicalize(&source).map_err(|error| format!("无法读取拖入的文件：{error}"))?;
    let source_metadata = fs::metadata(&source).map_err(|error| error.to_string())?;
    if !source_metadata.is_file() {
        return Err("文件中转站暂时只支持单个文件，不支持文件夹".to_string());
    }

    let directory = transit_dir(&app)?;
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let directory = fs::canonicalize(&directory).unwrap_or(directory);
    if source.starts_with(&directory) {
        return Err("这个文件已经在中转站里了".to_string());
    }

    let file_name = source_filename(&source)?;
    let stored_path = unique_stored_path(&directory, &file_name);
    move_file_cut_semantics(&source, &stored_path)?;
    let metadata = TransitMetadata {
        file_name,
        original_path: source,
        stored_path,
        size_bytes: source_metadata.len(),
        placed_at_ms: now_ms(),
    };
    if let Err(error) = write_metadata(&app, &metadata) {
        restore_moved_file_best_effort(&metadata.stored_path, &metadata.original_path);
        return Err(format!("暂存记录写入失败，已取消本次剪切：{error}"));
    }
    let status = metadata_to_status(&metadata);
    let _ = app.emit_to("main", "file-transit-updated", status.clone());
    Ok(status)
}

#[tauri::command]
pub fn file_transit_start_drag(window: Window, app: AppHandle) -> Result<(), String> {
    let metadata = read_metadata(&app)?.ok_or_else(|| "桌宠这里暂时没有文件".to_string())?;
    if !metadata.stored_path.is_file() {
        let _ = fs::remove_file(metadata_path(&app)?);
        emit_status(&app);
        return Err("中转站里的文件已经不存在了".to_string());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let drag_window = window.clone();
        let app_for_callback = app.clone();
        let app_for_start_error = app.clone();
        let stored_path = metadata.stored_path.clone();
        window
            .run_on_main_thread(move || {
                let result = drag::start_drag(
                    &drag_window,
                    drag::DragItem::Files(vec![stored_path]),
                    drag::Image::Raw(DRAG_PREVIEW_PNG.to_vec()),
                    move |result, _cursor_position| {
                        let dropped = matches!(result, drag::DragResult::Dropped);
                        let message = if dropped {
                            match clear_after_success(&app_for_callback) {
                                Ok(()) => "文件已取出".to_string(),
                                Err(error) => error,
                            }
                        } else {
                            "已取消取出，文件还在桌宠这里".to_string()
                        };
                        let _ = app_for_callback.emit_to(
                            "main",
                            "file-transit-drag-finished",
                            FileTransitDragResult { dropped, message },
                        );
                    },
                    drag::Options {
                        skip_animatation_on_cancel_or_failure: true,
                        mode: drag::DragMode::Move,
                    },
                );
                if let Err(error) = result {
                    let _ = app_for_start_error.emit_to(
                        "main",
                        "file-transit-drag-finished",
                        FileTransitDragResult {
                            dropped: false,
                            message: format!("文件拖出失败：{error}"),
                        },
                    );
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前系统暂不支持从桌宠拖出文件".to_string())
    }
}
