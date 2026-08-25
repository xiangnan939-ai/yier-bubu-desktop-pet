use std::{
    fs,
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::StreamExt;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;
use zip::ZipArchive;

const MAX_PACK_BYTES: usize = 256 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ARCHIVE_FILES: usize = 512;

pub const APP_UPDATE_ENDPOINT: Option<&str> = option_env!("YIER_BUBU_APP_UPDATE_ENDPOINT");
pub const APP_UPDATE_PUBLIC_KEY: Option<&str> = option_env!("YIER_BUBU_APP_UPDATE_PUBLIC_KEY");
const ASSET_MANIFEST_URL: Option<&str> = option_env!("YIER_BUBU_ASSET_MANIFEST_URL");
const ASSET_PUBLIC_KEY: Option<&str> = option_env!("YIER_BUBU_ASSET_PUBLIC_KEY");

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConfiguration {
    current_version: &'static str,
    app_update_enabled: bool,
    asset_update_enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateCheck {
    available: bool,
    current_version: String,
    version: Option<String>,
    notes: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssetManifest {
    schema_version: u8,
    version: String,
    min_app_version: String,
    pack_url: String,
    sha256: String,
    signature: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetUpdateResult {
    status: &'static str,
    version: Option<String>,
    message: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProgress {
    update_type: &'static str,
    phase: &'static str,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}

fn emit_update_progress(
    app: &AppHandle,
    update_type: &'static str,
    phase: &'static str,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
) {
    let _ = app.emit(
        "update-download-progress",
        UpdateProgress {
            update_type,
            phase,
            downloaded_bytes,
            total_bytes,
        },
    );
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotAsset {
    action: String,
    path: String,
    source_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledAssetPack {
    version: Option<String>,
    assets: Vec<HotAsset>,
    rules: Option<serde_json::Value>,
}

fn configured(value: Option<&str>) -> bool {
    value.is_some_and(|item| !item.trim().is_empty())
}

#[tauri::command]
pub fn update_configuration() -> UpdateConfiguration {
    UpdateConfiguration {
        current_version: env!("CARGO_PKG_VERSION"),
        app_update_enabled: configured(APP_UPDATE_ENDPOINT) && configured(APP_UPDATE_PUBLIC_KEY),
        asset_update_enabled: configured(ASSET_MANIFEST_URL) && configured(ASSET_PUBLIC_KEY),
    }
}

fn app_updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let endpoint = APP_UPDATE_ENDPOINT
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "程序发布地址尚未配置".to_string())?;
    require_safe_remote_url(endpoint)?;
    let url = endpoint
        .parse()
        .map_err(|_| "程序更新地址格式无效".to_string())?;
    app.updater_builder()
        .endpoints(vec![url])
        .map_err(|error| format!("程序更新地址无效：{error}"))?
        .build()
        .map_err(|error| format!("初始化程序更新器失败：{error}"))
}

#[tauri::command]
pub async fn check_app_update(app: AppHandle) -> Result<AppUpdateCheck, String> {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let update = app_updater(&app)?
        .check()
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
    let update = app_updater(&app)?
        .check()
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
                emit_update_progress(
                    &progress_app,
                    "app",
                    "downloading",
                    downloaded_bytes,
                    total_bytes,
                );
            },
            move || {
                emit_update_progress(&finish_app, "app", "installing", 0, None);
            },
        )
        .await
        .map_err(|error| format!("安装程序更新失败：{error}"))?;
    emit_update_progress(&app, "app", "complete", 0, None);
    Ok(())
}

fn asset_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("asset-packs"))
        .map_err(|error| format!("无法确定素材目录：{error}"))
}

fn manifest_payload(manifest: &AssetManifest) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        manifest.schema_version,
        manifest.version,
        manifest.min_app_version,
        manifest.pack_url,
        manifest.sha256.to_ascii_lowercase()
    )
}

fn verify_manifest(manifest: &AssetManifest, public_key: &str) -> Result<(), String> {
    if manifest.schema_version != 1 {
        return Err(format!("不支持素材清单版本 {}", manifest.schema_version));
    }
    Version::parse(&manifest.version).map_err(|_| "素材版本号不是合法的 SemVer".to_string())?;
    Version::parse(&manifest.min_app_version)
        .map_err(|_| "最低程序版本号不是合法的 SemVer".to_string())?;
    if manifest.sha256.len() != 64
        || !manifest
            .sha256
            .chars()
            .all(|value| value.is_ascii_hexdigit())
    {
        return Err("素材包 SHA-256 格式无效".to_string());
    }
    require_safe_remote_url(&manifest.pack_url)?;

    let key_bytes = BASE64
        .decode(public_key.trim())
        .map_err(|_| "素材更新公钥不是合法的 Base64".to_string())?;
    let key_array: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| "素材更新公钥必须是 32 字节 Ed25519 公钥".to_string())?;
    let verifying_key =
        VerifyingKey::from_bytes(&key_array).map_err(|_| "素材更新公钥无效".to_string())?;
    let signature_bytes = BASE64
        .decode(manifest.signature.trim())
        .map_err(|_| "素材清单签名不是合法的 Base64".to_string())?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| "素材清单签名长度无效".to_string())?;
    verifying_key
        .verify_strict(manifest_payload(manifest).as_bytes(), &signature)
        .map_err(|_| "素材清单签名验证失败，已拒绝更新".to_string())
}

fn require_safe_remote_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "更新地址格式无效".to_string())?;
    if url.scheme() == "https" {
        return Ok(());
    }
    #[cfg(debug_assertions)]
    if url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost")) {
        return Ok(());
    }
    Err("正式版本只接受 HTTPS 更新地址".to_string())
}

fn read_manifest(path: &Path) -> Option<AssetManifest> {
    serde_json::from_slice(&fs::read(path.join(".manifest.json")).ok()?).ok()
}

fn valid_pack_directory(path: &Path) -> bool {
    if !path.is_dir() || read_manifest(path).is_none() {
        return false;
    }
    ["一二", "布布"].iter().all(|role| {
        fs::read_dir(path.join(role)).ok().is_some_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                let filename = entry.file_name().to_string_lossy().to_string();
                entry.path().is_file()
                    && filename.to_ascii_lowercase().ends_with(".gif")
                    && action_from_filename(&filename) == "idle"
            })
        })
    })
}

fn recover_interrupted_activation(root: &Path) -> Result<(), String> {
    let current = root.join("current");
    let previous = root.join("previous");
    if !valid_pack_directory(&current) && valid_pack_directory(&previous) {
        if current.exists() {
            fs::remove_dir_all(&current).map_err(|error| format!("清理损坏素材失败：{error}"))?;
        }
        fs::rename(&previous, &current).map_err(|error| format!("恢复上一版素材失败：{error}"))?;
    }
    Ok(())
}

fn action_from_filename(filename: &str) -> String {
    let stem = filename
        .strip_suffix(".gif")
        .or_else(|| filename.strip_suffix(".GIF"))
        .unwrap_or(filename);
    let raw = stem.split('（').next().unwrap_or("idle");
    let action = raw.trim_end_matches(|value: char| value.is_ascii_digit());
    if action == "idel" { "idle" } else { action }.to_string()
}

#[tauri::command]
pub fn installed_asset_pack(app: AppHandle, role: String) -> Result<InstalledAssetPack, String> {
    let root = asset_root(&app)?;
    fs::create_dir_all(&root).map_err(|error| format!("创建素材目录失败：{error}"))?;
    recover_interrupted_activation(&root)?;
    let current = root.join("current");
    let manifest = read_manifest(&current);
    if !valid_pack_directory(&current) {
        return Ok(InstalledAssetPack {
            version: None,
            assets: Vec::new(),
            rules: None,
        });
    }
    // Program and character actions are released as one version. Never let a
    // leftover hot pack from an older program override the assets bundled in
    // the newly installed application.
    if manifest
        .as_ref()
        .is_some_and(|value| value.version != env!("CARGO_PKG_VERSION"))
    {
        return Ok(InstalledAssetPack {
            version: None,
            assets: Vec::new(),
            rules: None,
        });
    }

    let role_folder = match role.as_str() {
        "yier" => "一二",
        "bubu" => "布布",
        _ => return Err("未知桌宠角色".to_string()),
    };
    let mut assets = Vec::new();
    let entries = fs::read_dir(current.join(role_folder))
        .map_err(|error| format!("读取热更新素材失败：{error}"))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let filename = entry.file_name().to_string_lossy().to_string();
        if !path.is_file() || !filename.to_ascii_lowercase().ends_with(".gif") {
            continue;
        }
        assets.push(HotAsset {
            action: action_from_filename(&filename),
            path: path.to_string_lossy().to_string(),
            source_path: format!("hot://{role_folder}/{filename}"),
        });
    }
    assets.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    let rules = fs::read(current.join("rules.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Ok(InstalledAssetPack {
        version: manifest.map(|value| value.version),
        assets,
        rules,
    })
}

fn safe_archive_path(path: &Path) -> bool {
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return false;
    }
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(root)) if root == "一二" || root == "布布" => true,
        Some(Component::Normal(root)) if root == "rules.json" => components.next().is_none(),
        _ => false,
    }
}

fn unpack_and_validate(
    bytes: &[u8],
    staging: &Path,
    manifest: &AssetManifest,
) -> Result<(), String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("素材包不是有效的 ZIP：{error}"))?;
    if archive.len() > MAX_ARCHIVE_FILES {
        return Err("素材包文件数量超过安全限制".to_string());
    }
    fs::create_dir_all(staging).map_err(|error| format!("创建临时素材目录失败：{error}"))?;
    let mut unpacked = 0_u64;
    for index in 0..archive.len() {
        let mut item = archive
            .by_index(index)
            .map_err(|error| format!("读取素材包失败：{error}"))?;
        let enclosed = item
            .enclosed_name()
            .ok_or_else(|| "素材包包含不安全路径".to_string())?;
        if !safe_archive_path(&enclosed) {
            return Err(format!("素材包包含不允许的文件：{}", enclosed.display()));
        }
        if item.is_dir() {
            if enclosed == Path::new("rules.json") {
                return Err("rules.json 不能是目录".to_string());
            }
            fs::create_dir_all(staging.join(enclosed))
                .map_err(|error| format!("创建素材子目录失败：{error}"))?;
            continue;
        }
        let is_rules = enclosed == Path::new("rules.json");
        let is_gif = enclosed
            .extension()
            .is_some_and(|value| value.eq_ignore_ascii_case("gif"));
        if !is_rules && !is_gif {
            return Err(format!(
                "素材包只允许 GIF 和 rules.json：{}",
                enclosed.display()
            ));
        }
        if item.size() > 64 * 1024 * 1024 {
            return Err(format!("单个素材文件超过安全限制：{}", enclosed.display()));
        }
        unpacked = unpacked.saturating_add(item.size());
        if unpacked > MAX_UNPACKED_BYTES {
            return Err("素材包解压后大小超过安全限制".to_string());
        }
        let output_path = staging.join(&enclosed);
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建素材目录失败：{error}"))?;
        }
        let mut output =
            fs::File::create(&output_path).map_err(|error| format!("写入素材失败：{error}"))?;
        std::io::copy(&mut item, &mut output).map_err(|error| format!("解压素材失败：{error}"))?;
        output
            .flush()
            .map_err(|error| format!("保存素材失败：{error}"))?;
        if is_gif {
            let mut header = [0_u8; 6];
            fs::File::open(&output_path)
                .and_then(|mut file| file.read_exact(&mut header))
                .map_err(|_| format!("GIF 素材无法读取：{}", enclosed.display()))?;
            if &header != b"GIF87a" && &header != b"GIF89a" {
                return Err(format!("文件不是有效 GIF：{}", enclosed.display()));
            }
        }
    }
    fs::write(
        staging.join(".manifest.json"),
        serde_json::to_vec_pretty(manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("保存素材清单失败：{error}"))?;
    if !valid_pack_directory(staging) {
        return Err("素材包必须同时包含一二和布布的 idle GIF".to_string());
    }
    Ok(())
}

fn activate_pack(root: &Path, staging: &Path) -> Result<(), String> {
    let current = root.join("current");
    let previous = root.join("previous");
    if previous.exists() {
        fs::remove_dir_all(&previous).map_err(|error| format!("清理上一版素材失败：{error}"))?;
    }
    if current.exists() {
        fs::rename(&current, &previous).map_err(|error| format!("备份当前素材失败：{error}"))?;
    }
    if let Err(error) = fs::rename(staging, &current) {
        if previous.exists() && !current.exists() {
            let _ = fs::rename(&previous, &current);
        }
        return Err(format!("启用新素材失败：{error}"));
    }
    Ok(())
}

async fn download_bytes(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
    progress: Option<(&AppHandle, &'static str)>,
) -> Result<Vec<u8>, String> {
    require_safe_remote_url(url)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("下载更新失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("更新服务器返回错误：{error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("更新文件超过安全大小限制".to_string());
    }
    let total_bytes = response.content_length();
    let mut downloaded_bytes = 0_u64;
    let mut bytes = Vec::with_capacity(
        total_bytes
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or_default()
            .min(limit),
    );
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取更新文件失败：{error}"))?;
        downloaded_bytes = downloaded_bytes.saturating_add(chunk.len() as u64);
        if downloaded_bytes > limit as u64 {
            return Err("更新文件超过安全大小限制".to_string());
        }
        bytes.extend_from_slice(&chunk);
        if let Some((app, update_type)) = progress {
            emit_update_progress(
                app,
                update_type,
                "downloading",
                downloaded_bytes,
                total_bytes,
            );
        }
    }
    Ok(bytes)
}

#[tauri::command]
pub async fn check_and_install_asset_update(app: AppHandle) -> Result<AssetUpdateResult, String> {
    let (Some(manifest_url), Some(public_key)) = (ASSET_MANIFEST_URL, ASSET_PUBLIC_KEY) else {
        return Ok(AssetUpdateResult {
            status: "unconfigured",
            version: None,
            message: "素材发布地址尚未配置".to_string(),
        });
    };
    if manifest_url.trim().is_empty() || public_key.trim().is_empty() {
        return Ok(AssetUpdateResult {
            status: "unconfigured",
            version: None,
            message: "素材发布地址尚未配置".to_string(),
        });
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .user_agent(format!(
            "yier-bubu-desktop-pet/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(|error| format!("创建更新客户端失败：{error}"))?;
    let manifest_bytes = download_bytes(&client, manifest_url, 256 * 1024, None).await?;
    let manifest: AssetManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("素材清单格式错误：{error}"))?;
    verify_manifest(&manifest, public_key)?;

    let app_version =
        Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| error.to_string())?;
    let min_version =
        Version::parse(&manifest.min_app_version).map_err(|error| error.to_string())?;
    if app_version < min_version {
        return Ok(AssetUpdateResult {
            status: "requiresAppUpdate",
            version: Some(manifest.version),
            message: format!("新素材需要先把程序更新到 {}", manifest.min_app_version),
        });
    }

    let root = asset_root(&app)?;
    fs::create_dir_all(&root).map_err(|error| format!("创建素材目录失败：{error}"))?;
    recover_interrupted_activation(&root)?;
    if let Some(installed) = read_manifest(&root.join("current")) {
        let installed_version = Version::parse(&installed.version).ok();
        let remote_version =
            Version::parse(&manifest.version).map_err(|error| error.to_string())?;
        if installed_version.is_some_and(|current| current >= remote_version) {
            return Ok(AssetUpdateResult {
                status: "upToDate",
                version: Some(installed.version),
                message: "动作素材已是最新版".to_string(),
            });
        }
    }

    let pack_bytes = download_bytes(
        &client,
        &manifest.pack_url,
        MAX_PACK_BYTES,
        Some((&app, "assets")),
    )
    .await?;
    emit_update_progress(&app, "assets", "installing", 0, None);
    let actual_hash = format!("{:x}", Sha256::digest(&pack_bytes));
    if !actual_hash.eq_ignore_ascii_case(&manifest.sha256) {
        return Err("素材包 SHA-256 不一致，已拒绝安装".to_string());
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let staging = root.join(format!(".staging-{}-{nonce}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|error| format!("清理临时目录失败：{error}"))?;
    }
    if let Err(error) = unpack_and_validate(&pack_bytes, &staging, &manifest) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = activate_pack(&root, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    emit_update_progress(&app, "assets", "complete", 0, None);
    Ok(AssetUpdateResult {
        status: "updated",
        version: Some(manifest.version),
        message: "动作素材已更新，当前动作播完后生效".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn signed_asset_manifest_is_verified_and_tampering_is_rejected() {
        let signing_key = SigningKey::from_bytes(&[23_u8; 32]);
        let public_key = BASE64.encode(signing_key.verifying_key().as_bytes());
        let mut manifest = AssetManifest {
            schema_version: 1,
            version: "2026.8.1".to_string(),
            min_app_version: "0.2.2".to_string(),
            pack_url: "https://example.com/character-assets-2026.8.1.zip".to_string(),
            sha256: "ab".repeat(32),
            signature: String::new(),
        };
        manifest.signature = BASE64.encode(
            signing_key
                .sign(manifest_payload(&manifest).as_bytes())
                .to_bytes(),
        );
        assert!(verify_manifest(&manifest, &public_key).is_ok());

        manifest.pack_url = "https://example.com/tampered.zip".to_string();
        assert!(verify_manifest(&manifest, &public_key).is_err());
    }

    #[test]
    fn archive_paths_cannot_escape_or_hide_files_under_rules_json() {
        assert!(safe_archive_path(Path::new("一二/idel.gif")));
        assert!(safe_archive_path(Path::new("布布/walk.gif")));
        assert!(safe_archive_path(Path::new("rules.json")));
        assert!(!safe_archive_path(Path::new("../secret.gif")));
        assert!(!safe_archive_path(Path::new("rules.json/hidden.gif")));
        assert!(!safe_archive_path(Path::new("其他/idle.gif")));
    }
}
