use std::{
    collections::HashMap,
    fs,
    net::IpAddr,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

const KEYRING_SERVICE: &str = "com.yierbubu.desktop-pet";
const STATE_FILE: &str = "private-binding.json";
const PAIRING_LIFETIME: Duration = Duration::from_secs(120);
const REQUEST_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const SPAKE_MAC_ID: &[u8] = b"yier-bubu/mac/v1";
const SPAKE_WINDOWS_ID: &[u8] = b"yier-bubu/windows/v1";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEnrollment {
    pub role: String,
    pub pet_name: String,
    pub public_key: String,
    pub tailscale_stable_id: String,
    pub tailscale_host: String,
    pub machine_fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BindingCore {
    pub version: u8,
    pub binding_id: String,
    pub created_at_ms: u64,
    pub mac: DeviceEnrollment,
    pub windows: DeviceEnrollment,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BindingRecord {
    pub core: BindingCore,
    pub mac_signature: String,
    pub windows_signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UnbindCore {
    pub request_id: String,
    pub binding_id: String,
    pub requested_by: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SignedUnbindRequest {
    pub core: UnbindCore,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UnbindApproval {
    pub request: SignedUnbindRequest,
    pub approved_by: String,
    pub approved_at_ms: u64,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UnbindAck {
    pub request_id: String,
    pub binding_id: String,
    pub acknowledged_by: String,
    pub acknowledged_at_ms: u64,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PersistedBinding {
    record: Option<BindingRecord>,
    revoked: bool,
    incoming_unbind: Option<SignedUnbindRequest>,
    outgoing_unbind: Option<SignedUnbindRequest>,
    approved_unbind: Option<UnbindApproval>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingStatus {
    pub state: String,
    pub pet_name: String,
    pub partner_name: String,
    pub binding_id: Option<String>,
    pub partner_host: Option<String>,
    pub partner_machine_code: Option<String>,
    pub created_at_ms: Option<u64>,
    pub incoming_unbind: bool,
    pub outgoing_unbind: bool,
    pub approval_pending: bool,
    pub requested_by_name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingResult {
    pub state: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairStartRequest {
    pub binding_id: String,
    pub created_at_ms: u64,
    pub a_message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairStartResponse {
    pub session_id: String,
    pub b_message: String,
    pub server_info: DeviceEnrollment,
    pub server_mac: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairFinishRequest {
    pub session_id: String,
    pub client_info: DeviceEnrollment,
    pub client_mac: String,
    pub client_signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairFinishResponse {
    pub record: BindingRecord,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenAuth {
    pub message_type: String,
    pub binding_id: String,
    pub role: String,
    pub public_key: String,
    pub nonce: String,
    pub timestamp_ms: u64,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenServerAuth {
    pub message_type: String,
    pub binding_id: String,
    pub client_nonce: String,
    pub server_nonce: String,
    pub timestamp_ms: u64,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenConnectionInfo {
    pub partner_host: String,
    pub auth: ScreenAuth,
}

#[derive(Clone, Debug)]
struct TailNode {
    stable_id: String,
    host: String,
    ips: Vec<String>,
    os: String,
    online: bool,
}

#[derive(Clone, Debug)]
struct TailStatus {
    local: TailNode,
    peers: Vec<TailNode>,
}

struct PendingPair {
    binding_id: String,
    created_at_ms: u64,
    key: Vec<u8>,
    server_info: DeviceEnrollment,
    expires_at_ms: u64,
}

#[derive(Default)]
struct PairingRuntime {
    password: Option<String>,
    expires_at_ms: u64,
    pending: HashMap<String, PendingPair>,
    failures: u8,
    locked_until_ms: u64,
}

#[derive(Clone)]
pub struct BindingManager {
    app: AppHandle,
    state_path: PathBuf,
    state: Arc<RwLock<PersistedBinding>>,
    pairing: Arc<Mutex<PairingRuntime>>,
    seen_screen_nonces: Arc<Mutex<HashMap<String, u64>>>,
    signing_key: Arc<SigningKey>,
}

impl BindingManager {
    pub fn new(app: AppHandle, app_data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&app_data_dir).map_err(|error| error.to_string())?;
        let state_path = app_data_dir.join(STATE_FILE);
        let state = if state_path.exists() {
            serde_json::from_slice(&fs::read(&state_path).map_err(|error| error.to_string())?)
                .map_err(|error| format!("绑定信息损坏：{error}"))?
        } else {
            PersistedBinding::default()
        };
        let signing_key = load_or_create_signing_key()?;
        Ok(Self {
            app,
            state_path,
            state: Arc::new(RwLock::new(state)),
            pairing: Arc::new(Mutex::new(PairingRuntime::default())),
            seen_screen_nonces: Arc::new(Mutex::new(HashMap::new())),
            signing_key: Arc::new(signing_key),
        })
    }

    pub fn role(&self) -> &'static str {
        platform_role()
    }

    pub async fn status(&self) -> BindingStatus {
        let state = self.state.read().await;
        let record = state.record.as_ref();
        let partner = record.map(|value| partner_enrollment(&value.core));
        let lifecycle = if state.approved_unbind.is_some() && !state.revoked {
            "revoking"
        } else if state.revoked {
            "revoked"
        } else if record.is_some() {
            "bound"
        } else {
            "unbound"
        };
        BindingStatus {
            state: lifecycle.to_string(),
            pet_name: local_pet_name().to_string(),
            partner_name: partner_pet_name().to_string(),
            binding_id: record.map(|value| value.core.binding_id.clone()),
            partner_host: partner.map(|value| value.tailscale_host.clone()),
            partner_machine_code: partner.map(|value| short_code(&value.machine_fingerprint)),
            created_at_ms: record.map(|value| value.core.created_at_ms),
            incoming_unbind: state
                .incoming_unbind
                .as_ref()
                .is_some_and(|request| request.core.expires_at_ms >= now_ms()),
            outgoing_unbind: state
                .outgoing_unbind
                .as_ref()
                .is_some_and(|request| request.core.expires_at_ms >= now_ms()),
            approval_pending: state.approved_unbind.is_some() && !state.revoked,
            requested_by_name: state.incoming_unbind.as_ref().and_then(|request| {
                (request.core.expires_at_ms >= now_ms())
                    .then(|| pet_name_for_role(&request.core.requested_by).to_string())
            }),
        }
    }

    pub async fn pair(&self, passphrase: String) -> Result<PairingResult, String> {
        let passphrase = passphrase.trim().to_string();
        if passphrase.chars().count() < 8 {
            return Err("绑定口令至少需要 8 个字符".into());
        }
        {
            let state = self.state.read().await;
            if state.record.is_some() && !state.revoked {
                return Err("这台电脑已经绑定，不能再次绑定".into());
            }
            if state.approved_unbind.is_some() && !state.revoked {
                return Err("双向解绑尚未完成，暂时不能重新绑定".into());
            }
        }

        if self.role() == "bubu" {
            self.wait_for_mac_pairing(passphrase).await
        } else {
            self.connect_to_windows_for_pairing(passphrase).await
        }
    }

    async fn wait_for_mac_pairing(&self, passphrase: String) -> Result<PairingResult, String> {
        {
            let mut runtime = self.pairing.lock().await;
            runtime.password = Some(passphrase);
            runtime.expires_at_ms = now_ms() + PAIRING_LIFETIME.as_millis() as u64;
            runtime.pending.clear();
            runtime.failures = 0;
            runtime.locked_until_ms = 0;
        }
        let deadline = now_ms() + PAIRING_LIFETIME.as_millis() as u64;
        while now_ms() < deadline {
            if self.state.read().await.record.is_some() && !self.state.read().await.revoked {
                self.clear_pairing_runtime().await;
                return Ok(PairingResult {
                    state: "bound".into(),
                    message: "已成功绑定我的布布".into(),
                });
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        self.clear_pairing_runtime().await;
        Err("等待一二超时，请确认两台电脑都在线并重新输入相同口令".into())
    }

    async fn connect_to_windows_for_pairing(
        &self,
        passphrase: String,
    ) -> Result<PairingResult, String> {
        let deadline = now_ms() + PAIRING_LIFETIME.as_millis() as u64;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|error| error.to_string())?;
        let mut last_error = "尚未发现等待绑定的布布电脑".to_string();

        while now_ms() < deadline {
            let status = tailscale_status()
                .await
                .map_err(|error| format!("无法读取 Tailscale 设备：{error}"))?;
            let candidates: Vec<TailNode> = status
                .peers
                .into_iter()
                .filter(|peer| peer.online && peer.os.to_ascii_lowercase().contains("windows"))
                .collect();
            for candidate in candidates {
                match self
                    .try_pair_candidate(&client, &candidate, &passphrase)
                    .await
                {
                    Ok(()) => {
                        return Ok(PairingResult {
                            state: "bound".into(),
                            message: "已成功绑定我的一二".into(),
                        });
                    }
                    Err(error) => last_error = error,
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(format!("绑定超时：{last_error}"))
    }

    async fn try_pair_candidate(
        &self,
        client: &reqwest::Client,
        candidate: &TailNode,
        passphrase: &str,
    ) -> Result<(), String> {
        let binding_id = Uuid::new_v4().to_string();
        let (spake, a_message) = Spake2::<Ed25519Group>::start_a(
            &Password::new(passphrase.as_bytes()),
            &Identity::new(SPAKE_MAC_ID),
            &Identity::new(SPAKE_WINDOWS_ID),
        );
        let start = PairStartRequest {
            binding_id: binding_id.clone(),
            created_at_ms: now_ms(),
            a_message: BASE64.encode(a_message),
        };
        let base = format!("http://{}:39821", candidate.host);
        let response = client
            .post(format!("{base}/pair/start"))
            .json(&start)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(response
                .text()
                .await
                .unwrap_or_else(|_| "对方尚未打开绑定窗口".into()));
        }
        let response: PairStartResponse =
            response.json().await.map_err(|error| error.to_string())?;
        if response.server_info.role != "bubu"
            || response.server_info.tailscale_stable_id != candidate.stable_id
        {
            return Err("对方 Tailscale 设备身份与绑定响应不一致".into());
        }
        let b_message = BASE64
            .decode(&response.b_message)
            .map_err(|_| "绑定握手数据无效".to_string())?;
        let key = spake
            .finish(&b_message)
            .map_err(|_| "绑定口令握手失败".to_string())?;
        verify_payload_mac(
            &key,
            b"server-info",
            &response.server_info,
            &response.server_mac,
        )
        .map_err(|_| "双方口令不一致".to_string())?;

        let client_info = self.local_enrollment(&binding_id).await?;
        let core = BindingCore {
            version: 1,
            binding_id,
            created_at_ms: start.created_at_ms,
            mac: client_info.clone(),
            windows: response.server_info,
        };
        let client_signature = self.sign_serialized(&core)?;
        let finish = PairFinishRequest {
            session_id: response.session_id,
            client_mac: payload_mac(&key, b"client-info", &client_info)?,
            client_info,
            client_signature,
        };
        let response = client
            .post(format!("{base}/pair/finish"))
            .json(&finish)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(response
                .text()
                .await
                .unwrap_or_else(|_| "对方拒绝完成绑定".into()));
        }
        let response: PairFinishResponse =
            response.json().await.map_err(|error| error.to_string())?;
        self.verify_complete_record(&response.record)?;
        if response.record.core != core {
            return Err("双方生成的绑定记录不一致".into());
        }
        self.replace_with_record(response.record).await?;
        Ok(())
    }

    pub async fn receive_pair_start(
        &self,
        remote_ip: IpAddr,
        request: PairStartRequest,
    ) -> Result<PairStartResponse, String> {
        if self.role() != "bubu" {
            return Err("只有布布电脑可以响应首次绑定".into());
        }
        let source = peer_for_ip(remote_ip).await?;
        if !source.os.to_ascii_lowercase().contains("mac") {
            return Err("首次绑定请求不是来自 Tailscale 中的 Mac".into());
        }
        let password = {
            let runtime = self.pairing.lock().await;
            if now_ms() < runtime.locked_until_ms {
                return Err("口令尝试次数过多，请稍后再试".into());
            }
            if now_ms() > runtime.expires_at_ms {
                return Err("请先在布布电脑输入绑定口令".into());
            }
            runtime
                .password
                .clone()
                .ok_or_else(|| "请先在布布电脑输入绑定口令".to_string())?
        };
        if now_ms().abs_diff(request.created_at_ms) > PAIRING_LIFETIME.as_millis() as u64 {
            return Err("绑定请求已过期".into());
        }
        let a_message = BASE64
            .decode(&request.a_message)
            .map_err(|_| "绑定握手数据无效".to_string())?;
        let (spake, b_message) = Spake2::<Ed25519Group>::start_b(
            &Password::new(password.as_bytes()),
            &Identity::new(SPAKE_MAC_ID),
            &Identity::new(SPAKE_WINDOWS_ID),
        );
        let key = spake
            .finish(&a_message)
            .map_err(|_| "绑定握手数据无效".to_string())?;
        let server_info = self.local_enrollment(&request.binding_id).await?;
        let server_mac = payload_mac(&key, b"server-info", &server_info)?;
        let session_id = Uuid::new_v4().to_string();
        let pending = PendingPair {
            binding_id: request.binding_id,
            created_at_ms: request.created_at_ms,
            key,
            server_info: server_info.clone(),
            expires_at_ms: now_ms() + 30_000,
        };
        let mut runtime = self.pairing.lock().await;
        runtime
            .pending
            .retain(|_, value| value.expires_at_ms >= now_ms());
        if runtime.pending.len() >= 8 {
            return Err("等待中的绑定握手过多，请稍后重试".into());
        }
        runtime.pending.insert(session_id.clone(), pending);
        Ok(PairStartResponse {
            session_id,
            b_message: BASE64.encode(b_message),
            server_info,
            server_mac,
        })
    }

    pub async fn receive_pair_finish(
        &self,
        remote_ip: IpAddr,
        request: PairFinishRequest,
    ) -> Result<PairFinishResponse, String> {
        let pending = self
            .pairing
            .lock()
            .await
            .pending
            .remove(&request.session_id)
            .ok_or_else(|| "绑定会话不存在或已过期".to_string())?;
        if now_ms() > pending.expires_at_ms {
            return Err("绑定会话已过期".into());
        }
        if verify_payload_mac(
            &pending.key,
            b"client-info",
            &request.client_info,
            &request.client_mac,
        )
        .is_err()
        {
            self.record_pairing_failure().await;
            return Err("双方口令不一致".into());
        }
        let source = peer_for_ip(remote_ip).await?;
        if request.client_info.role != "yier"
            || request.client_info.tailscale_stable_id != source.stable_id
        {
            return Err("一二电脑的 Tailscale 身份不一致".into());
        }
        let core = BindingCore {
            version: 1,
            binding_id: pending.binding_id,
            created_at_ms: pending.created_at_ms,
            mac: request.client_info,
            windows: pending.server_info,
        };
        verify_serialized(&core.mac.public_key, &core, &request.client_signature)?;
        let record = BindingRecord {
            windows_signature: self.sign_serialized(&core)?,
            mac_signature: request.client_signature,
            core,
        };
        self.verify_complete_record(&record)?;
        self.replace_with_record(record.clone()).await?;
        self.clear_pairing_runtime().await;
        Ok(PairFinishResponse { record })
    }

    pub async fn screen_connection_info(&self) -> Result<ScreenConnectionInfo, String> {
        let record = self.active_record().await?;
        self.ensure_local_machine(&record).await?;
        let partner = partner_enrollment(&record.core);
        let partner_host = resolve_peer_host(&partner.tailscale_stable_id)
            .await
            .unwrap_or_else(|_| partner.tailscale_host.clone());
        let nonce = Uuid::new_v4().to_string();
        let timestamp_ms = now_ms();
        let public_key = BASE64.encode(self.signing_key.verifying_key().as_bytes());
        let signature = self.sign_bytes(&screen_client_bytes(
            &record.core.binding_id,
            self.role(),
            &public_key,
            &nonce,
            timestamp_ms,
        ));
        Ok(ScreenConnectionInfo {
            partner_host,
            auth: ScreenAuth {
                message_type: "screenAuth".into(),
                binding_id: record.core.binding_id,
                role: self.role().into(),
                public_key,
                nonce,
                timestamp_ms,
                signature,
            },
        })
    }

    pub async fn authorize_screen_client(
        &self,
        remote_ip: IpAddr,
        auth: &ScreenAuth,
    ) -> Result<ScreenServerAuth, String> {
        let record = self.active_record().await?;
        self.ensure_local_machine(&record).await?;
        let partner = partner_enrollment(&record.core);
        let source = peer_for_ip(remote_ip).await?;
        if source.stable_id != partner.tailscale_stable_id {
            return Err("连接不是来自已绑定的 Tailscale 设备".into());
        }
        if auth.message_type != "screenAuth"
            || auth.binding_id != record.core.binding_id
            || auth.role != partner.role
            || auth.public_key != partner.public_key
            || now_ms().abs_diff(auth.timestamp_ms) > 60_000
        {
            return Err("设备绑定身份不匹配".into());
        }
        {
            let mut seen = self.seen_screen_nonces.lock().await;
            seen.retain(|_, timestamp| now_ms().saturating_sub(*timestamp) <= 120_000);
            if seen.insert(auth.nonce.clone(), auth.timestamp_ms).is_some() {
                return Err("检测到重复的设备认证请求".into());
            }
        }
        verify_bytes(
            &partner.public_key,
            &screen_client_bytes(
                &auth.binding_id,
                &auth.role,
                &auth.public_key,
                &auth.nonce,
                auth.timestamp_ms,
            ),
            &auth.signature,
        )?;
        let server_nonce = Uuid::new_v4().to_string();
        let timestamp_ms = now_ms();
        let bytes = screen_server_bytes(
            &record.core.binding_id,
            &auth.nonce,
            &server_nonce,
            timestamp_ms,
        );
        Ok(ScreenServerAuth {
            message_type: "screenAuthOk".into(),
            binding_id: record.core.binding_id,
            client_nonce: auth.nonce.clone(),
            server_nonce,
            timestamp_ms,
            signature: self.sign_bytes(&bytes),
        })
    }

    pub async fn verify_screen_server_auth(&self, auth: ScreenServerAuth) -> Result<bool, String> {
        let record = self.active_record().await?;
        if auth.message_type != "screenAuthOk"
            || auth.binding_id != record.core.binding_id
            || now_ms().abs_diff(auth.timestamp_ms) > 60_000
        {
            return Ok(false);
        }
        let partner = partner_enrollment(&record.core);
        verify_bytes(
            &partner.public_key,
            &screen_server_bytes(
                &auth.binding_id,
                &auth.client_nonce,
                &auth.server_nonce,
                auth.timestamp_ms,
            ),
            &auth.signature,
        )?;
        Ok(true)
    }

    pub async fn request_unbind(&self) -> Result<String, String> {
        let record = self.active_record().await?;
        let core = UnbindCore {
            request_id: Uuid::new_v4().to_string(),
            binding_id: record.core.binding_id.clone(),
            requested_by: self.role().into(),
            created_at_ms: now_ms(),
            expires_at_ms: now_ms() + REQUEST_LIFETIME.as_millis() as u64,
        };
        let request = SignedUnbindRequest {
            signature: self.sign_bytes(&unbind_request_bytes(&core)),
            core,
        };
        let partner = partner_enrollment(&record.core);
        let host = resolve_peer_host(&partner.tailscale_stable_id)
            .await
            .unwrap_or_else(|_| partner.tailscale_host.clone());
        let response = short_http_client()?
            .post(format!("http://{host}:39821/unbind/request"))
            .json(&request)
            .send()
            .await
            .map_err(|_| "对方当前不在线，解绑请求尚未发送".to_string())?;
        if !response.status().is_success() {
            return Err(response
                .text()
                .await
                .unwrap_or_else(|_| "对方拒绝接收解绑请求".into()));
        }
        self.mutate_state(|state| state.outgoing_unbind = Some(request))
            .await?;
        Ok("解绑请求已发送，只有对方同意后才会生效".into())
    }

    pub async fn receive_unbind_request(
        &self,
        remote_ip: IpAddr,
        request: SignedUnbindRequest,
    ) -> Result<(), String> {
        let record = self.active_record().await?;
        let partner = partner_enrollment(&record.core);
        self.verify_remote_peer(remote_ip, partner).await?;
        if request.core.binding_id != record.core.binding_id
            || request.core.requested_by != partner.role
            || request.core.expires_at_ms < now_ms()
        {
            return Err("解绑请求与当前绑定不匹配".into());
        }
        verify_bytes(
            &partner.public_key,
            &unbind_request_bytes(&request.core),
            &request.signature,
        )?;
        self.mutate_state(|state| state.incoming_unbind = Some(request))
            .await?;
        let _ = self.app.emit("unbind-request-received", ());
        let _ = self.app.emit("binding-changed", ());
        Ok(())
    }

    pub async fn respond_unbind(&self, approve: bool) -> Result<String, String> {
        let state = self.state.read().await.clone();
        let record = state.record.ok_or_else(|| "当前没有绑定".to_string())?;
        let request = state
            .incoming_unbind
            .or_else(|| {
                state
                    .approved_unbind
                    .as_ref()
                    .map(|value| value.request.clone())
            })
            .ok_or_else(|| "当前没有等待处理的解绑请求".to_string())?;
        let partner = partner_enrollment(&record.core);
        let host = resolve_peer_host(&partner.tailscale_stable_id)
            .await
            .unwrap_or_else(|_| partner.tailscale_host.clone());
        let client = short_http_client()?;

        if !approve {
            if state.approved_unbind.is_some() {
                return Err("已经签署同意，不能再改为拒绝".into());
            }
            let acknowledged_at_ms = now_ms();
            let rejection = UnbindAck {
                request_id: request.core.request_id.clone(),
                binding_id: request.core.binding_id.clone(),
                acknowledged_by: self.role().into(),
                acknowledged_at_ms,
                signature: self.sign_bytes(&unbind_reject_bytes(
                    &request.core.request_id,
                    &request.core.binding_id,
                    self.role(),
                    acknowledged_at_ms,
                )),
            };
            let response = client
                .post(format!("http://{host}:39821/unbind/reject"))
                .json(&rejection)
                .send()
                .await
                .map_err(|_| "对方当前不在线，拒绝结果尚未送达".to_string())?;
            if !response.status().is_success() {
                return Err("对方未能确认拒绝结果".into());
            }
            self.mutate_state(|state| state.incoming_unbind = None)
                .await?;
            return Ok("已拒绝解绑，原绑定继续有效".into());
        }

        let approval = state.approved_unbind.unwrap_or_else(|| {
            let approved_at_ms = now_ms();
            UnbindApproval {
                signature: self.sign_bytes(&unbind_approval_bytes(
                    &request,
                    self.role(),
                    approved_at_ms,
                )),
                request: request.clone(),
                approved_by: self.role().into(),
                approved_at_ms,
            }
        });
        self.mutate_state(|state| state.approved_unbind = Some(approval.clone()))
            .await?;
        let response = client
            .post(format!("http://{host}:39821/unbind/approve"))
            .json(&approval)
            .send()
            .await
            .map_err(|_| "已记录你的同意；对方离线，稍后请重试完成解绑".to_string())?;
        if !response.status().is_success() {
            return Err(response
                .text()
                .await
                .unwrap_or_else(|_| "对方未能完成解绑".into()));
        }
        let ack: UnbindAck = response.json().await.map_err(|error| error.to_string())?;
        verify_bytes(
            &partner.public_key,
            &unbind_ack_bytes(
                &ack.request_id,
                &ack.binding_id,
                &ack.acknowledged_by,
                ack.acknowledged_at_ms,
            ),
            &ack.signature,
        )?;
        if ack.request_id != request.core.request_id || ack.binding_id != record.core.binding_id {
            return Err("对方的解绑确认不匹配".into());
        }
        self.mutate_state(|state| {
            state.revoked = true;
            state.incoming_unbind = None;
        })
        .await?;
        let _ = self.app.emit("binding-changed", ());
        Ok("双方已经同意，绑定已解除".into())
    }

    pub async fn receive_unbind_approval(
        &self,
        remote_ip: IpAddr,
        approval: UnbindApproval,
    ) -> Result<UnbindAck, String> {
        let state = self.state.read().await.clone();
        let record = state.record.ok_or_else(|| "当前没有绑定记录".to_string())?;
        let partner = partner_enrollment(&record.core);
        self.verify_remote_peer(remote_ip, partner).await?;
        let expected = state
            .outgoing_unbind
            .ok_or_else(|| "本机没有发出对应解绑请求".to_string())?;
        if approval.request != expected
            || approval.approved_by != partner.role
            || approval.request.core.expires_at_ms < now_ms()
        {
            return Err("解绑同意与本机请求不匹配".into());
        }
        verify_bytes(
            &partner.public_key,
            &unbind_approval_bytes(
                &approval.request,
                &approval.approved_by,
                approval.approved_at_ms,
            ),
            &approval.signature,
        )?;
        self.mutate_state(|state| {
            state.approved_unbind = Some(approval.clone());
            state.revoked = true;
            state.outgoing_unbind = None;
        })
        .await?;
        let acknowledged_at_ms = now_ms();
        let ack = UnbindAck {
            request_id: approval.request.core.request_id,
            binding_id: record.core.binding_id,
            acknowledged_by: self.role().into(),
            acknowledged_at_ms,
            signature: String::new(),
        };
        let ack = UnbindAck {
            signature: self.sign_bytes(&unbind_ack_bytes(
                &ack.request_id,
                &ack.binding_id,
                &ack.acknowledged_by,
                ack.acknowledged_at_ms,
            )),
            ..ack
        };
        let _ = self.app.emit("binding-changed", ());
        Ok(ack)
    }

    pub async fn receive_unbind_rejection(
        &self,
        remote_ip: IpAddr,
        rejection: UnbindAck,
    ) -> Result<(), String> {
        let state = self.state.read().await.clone();
        let record = state.record.ok_or_else(|| "当前没有绑定记录".to_string())?;
        let partner = partner_enrollment(&record.core);
        self.verify_remote_peer(remote_ip, partner).await?;
        let outgoing = state
            .outgoing_unbind
            .ok_or_else(|| "本机没有等待中的解绑请求".to_string())?;
        if rejection.request_id != outgoing.core.request_id
            || rejection.binding_id != record.core.binding_id
            || rejection.acknowledged_by != partner.role
        {
            return Err("解绑拒绝与本机请求不匹配".into());
        }
        verify_bytes(
            &partner.public_key,
            &unbind_reject_bytes(
                &rejection.request_id,
                &rejection.binding_id,
                &rejection.acknowledged_by,
                rejection.acknowledged_at_ms,
            ),
            &rejection.signature,
        )?;
        self.mutate_state(|state| state.outgoing_unbind = None)
            .await?;
        let _ = self.app.emit("binding-changed", ());
        Ok(())
    }

    pub async fn local_tailscale_ip(&self) -> Result<String, String> {
        let status = tailscale_status().await?;
        status
            .local
            .ips
            .into_iter()
            .find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())
            .ok_or_else(|| "Tailscale 尚未分配 IPv4 地址".to_string())
    }

    async fn local_enrollment(&self, binding_id: &str) -> Result<DeviceEnrollment, String> {
        let status = tailscale_status().await?;
        Ok(DeviceEnrollment {
            role: self.role().into(),
            pet_name: local_pet_name().into(),
            public_key: BASE64.encode(self.signing_key.verifying_key().as_bytes()),
            tailscale_stable_id: status.local.stable_id,
            tailscale_host: status.local.host,
            machine_fingerprint: machine_fingerprint(binding_id)?,
        })
    }

    async fn ensure_local_machine(&self, record: &BindingRecord) -> Result<(), String> {
        let local = local_enrollment(&record.core);
        if machine_fingerprint(&record.core.binding_id)? != local.machine_fingerprint {
            return Err("本机机器特征与绑定记录不一致，已拒绝连接".into());
        }
        let status = tailscale_status().await?;
        if status.local.stable_id != local.tailscale_stable_id {
            return Err("本机 Tailscale 设备身份与绑定记录不一致".into());
        }
        Ok(())
    }

    async fn verify_remote_peer(
        &self,
        remote_ip: IpAddr,
        expected: &DeviceEnrollment,
    ) -> Result<(), String> {
        let source = peer_for_ip(remote_ip).await?;
        if source.stable_id != expected.tailscale_stable_id {
            return Err("请求不是来自已绑定的对方电脑".into());
        }
        Ok(())
    }

    async fn active_record(&self) -> Result<BindingRecord, String> {
        let state = self.state.read().await;
        if state.revoked || state.approved_unbind.is_some() {
            return Err("绑定已解除或正在完成双向解绑".into());
        }
        state
            .record
            .clone()
            .ok_or_else(|| "请先完成首次绑定".into())
    }

    fn verify_complete_record(&self, record: &BindingRecord) -> Result<(), String> {
        if record.core.version != 1
            || record.core.mac.role != "yier"
            || record.core.windows.role != "bubu"
        {
            return Err("绑定记录的设备角色无效".into());
        }
        verify_serialized(
            &record.core.mac.public_key,
            &record.core,
            &record.mac_signature,
        )?;
        verify_serialized(
            &record.core.windows.public_key,
            &record.core,
            &record.windows_signature,
        )?;
        Ok(())
    }

    async fn replace_with_record(&self, record: BindingRecord) -> Result<(), String> {
        self.mutate_state(|state| {
            *state = PersistedBinding {
                record: Some(record),
                ..PersistedBinding::default()
            };
        })
        .await?;
        let _ = self.app.emit("binding-changed", ());
        Ok(())
    }

    async fn mutate_state(&self, change: impl FnOnce(&mut PersistedBinding)) -> Result<(), String> {
        let mut state = self.state.write().await;
        change(&mut state);
        let bytes = serde_json::to_vec_pretty(&*state).map_err(|error| error.to_string())?;
        fs::write(&self.state_path, bytes).map_err(|error| error.to_string())
    }

    async fn record_pairing_failure(&self) {
        let mut runtime = self.pairing.lock().await;
        runtime.failures = runtime.failures.saturating_add(1);
        if runtime.failures >= 5 {
            runtime.locked_until_ms = now_ms() + 60_000;
            runtime.failures = 0;
        }
    }

    async fn clear_pairing_runtime(&self) {
        *self.pairing.lock().await = PairingRuntime::default();
    }

    fn sign_serialized<T: Serialize>(&self, value: &T) -> Result<String, String> {
        let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        Ok(self.sign_bytes(&bytes))
    }

    fn sign_bytes(&self, bytes: &[u8]) -> String {
        BASE64.encode(self.signing_key.sign(bytes).to_bytes())
    }
}

fn load_or_create_signing_key() -> Result<SigningKey, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, platform_role())
        .map_err(|error| format!("无法访问系统安全存储：{error}"))?;
    match entry.get_password() {
        Ok(encoded) => {
            let bytes = BASE64
                .decode(encoded)
                .map_err(|_| "系统安全存储中的设备密钥损坏".to_string())?;
            let bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| "系统安全存储中的设备密钥长度无效".to_string())?;
            return Ok(SigningKey::from_bytes(&bytes));
        }
        Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(format!("无法读取系统安全存储中的设备密钥：{error}")),
    }
    let key = SigningKey::generate(&mut OsRng);
    entry
        .set_password(&BASE64.encode(key.to_bytes()))
        .map_err(|error| format!("无法保存设备密钥到系统安全存储：{error}"))?;
    Ok(key)
}

fn verify_serialized<T: Serialize>(
    public_key: &str,
    value: &T,
    signature: &str,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    verify_bytes(public_key, &bytes, signature)
}

fn verify_bytes(public_key: &str, bytes: &[u8], signature: &str) -> Result<(), String> {
    let public = BASE64
        .decode(public_key)
        .map_err(|_| "设备公钥无效".to_string())?;
    let public: [u8; 32] = public
        .try_into()
        .map_err(|_| "设备公钥长度无效".to_string())?;
    let key = VerifyingKey::from_bytes(&public).map_err(|_| "设备公钥无法验证".to_string())?;
    let signature = BASE64
        .decode(signature)
        .map_err(|_| "设备签名无效".to_string())?;
    let signature =
        Signature::from_slice(&signature).map_err(|_| "设备签名长度无效".to_string())?;
    key.verify(bytes, &signature)
        .map_err(|_| "设备签名校验失败".to_string())
}

fn payload_mac<T: Serialize>(key: &[u8], label: &[u8], value: &T) -> Result<String, String> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| error.to_string())?;
    mac.update(label);
    mac.update(&serde_json::to_vec(value).map_err(|error| error.to_string())?);
    Ok(BASE64.encode(mac.finalize().into_bytes()))
}

fn verify_payload_mac<T: Serialize>(
    key: &[u8],
    label: &[u8],
    value: &T,
    expected: &str,
) -> Result<(), String> {
    let expected = BASE64
        .decode(expected)
        .map_err(|_| "绑定校验码无效".to_string())?;
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| error.to_string())?;
    mac.update(label);
    mac.update(&serde_json::to_vec(value).map_err(|error| error.to_string())?);
    mac.verify_slice(&expected)
        .map_err(|_| "绑定校验失败".to_string())
}

fn platform_role() -> &'static str {
    if cfg!(target_os = "windows") {
        "bubu"
    } else {
        "yier"
    }
}

fn local_pet_name() -> &'static str {
    if platform_role() == "bubu" {
        "布布"
    } else {
        "一二"
    }
}

fn partner_pet_name() -> &'static str {
    if platform_role() == "bubu" {
        "一二"
    } else {
        "布布"
    }
}

fn pet_name_for_role(role: &str) -> &'static str {
    if role == "bubu" {
        "布布"
    } else {
        "一二"
    }
}

fn local_enrollment(core: &BindingCore) -> &DeviceEnrollment {
    if platform_role() == "bubu" {
        &core.windows
    } else {
        &core.mac
    }
}

fn partner_enrollment(core: &BindingCore) -> &DeviceEnrollment {
    if platform_role() == "bubu" {
        &core.mac
    } else {
        &core.windows
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn short_code(value: &str) -> String {
    value.chars().take(12).collect::<String>().to_uppercase()
}

fn machine_fingerprint(binding_id: &str) -> Result<String, String> {
    let raw = platform_machine_id()
        .ok_or_else(|| "无法读取本机机器特征码，已停止绑定以避免弱身份校验".to_string())?;
    let mut digest = Sha256::new();
    digest.update(b"yier-bubu-machine-v1\0");
    digest.update(binding_id.as_bytes());
    digest.update(b"\0");
    digest.update(raw.as_bytes());
    Ok(hex_lower(&digest.finalize()))
}

#[cfg(target_os = "macos")]
fn platform_machine_id() -> Option<String> {
    let output = Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().find_map(|line| {
        if !line.contains("IOPlatformUUID") {
            return None;
        }
        line.split('=')
            .nth(1)
            .map(|value| value.trim().trim_matches('"').to_string())
    })
}

#[cfg(target_os = "windows")]
fn platform_machine_id() -> Option<String> {
    let output = Command::new("reg.exe")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().find_map(|line| {
        if !line.contains("MachineGuid") {
            return None;
        }
        line.split_whitespace().last().map(str::to_string)
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_machine_id() -> Option<String> {
    None
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn tailscale_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("tailscale")];
    #[cfg(target_os = "macos")]
    {
        candidates.extend([
            PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale"),
            PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/tailscale"),
            PathBuf::from("/usr/local/bin/tailscale"),
            PathBuf::from("/opt/homebrew/bin/tailscale"),
        ]);
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home)
                    .join("Applications")
                    .join("Tailscale.app")
                    .join("Contents")
                    .join("MacOS")
                    .join("Tailscale"),
            );
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            candidates.push(
                PathBuf::from(program_files)
                    .join("Tailscale")
                    .join("tailscale.exe"),
            );
        }
        candidates.push(PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe"));
    }
    candidates
}

async fn tailscale_status() -> Result<TailStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut last_error = String::new();
        let mut command_found = false;
        for binary in tailscale_candidates() {
            let mut command = Command::new(&binary);
            command.args(["status", "--json"]);
            #[cfg(target_os = "macos")]
            command.env("TAILSCALE_BE_CLI", "1");
            match command.output() {
                Ok(output) if output.status.success() => {
                    return parse_tailscale_status(&output.stdout);
                }
                Ok(output) => {
                    command_found = true;
                    last_error = String::from_utf8_lossy(&output.stderr).trim().to_string()
                }
                Err(error) => {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        command_found = true;
                    }
                    last_error = error.to_string();
                }
            }
        }
        if command_found {
            Err(format!(
                "Tailscale 已安装，但尚未正常连接。请打开 Tailscale、登录并确认状态为已连接后重试。详细信息：{last_error}"
            ))
        } else {
            Err("未检测到 Tailscale。请先在 Mac 和 Windows 安装 Tailscale，登录同一个账号并保持连接，再返回重试绑定。".to_string())
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

fn parse_tailscale_status(bytes: &[u8]) -> Result<TailStatus, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let local = parse_tail_node(value.get("Self").ok_or("Tailscale 状态缺少本机信息")?)?;
    let peers = value
        .get("Peer")
        .and_then(|value| value.as_object())
        .map(|values| {
            values
                .values()
                .filter_map(|value| parse_tail_node(value).ok())
                .collect()
        })
        .unwrap_or_default();
    Ok(TailStatus { local, peers })
}

fn parse_tail_node(value: &serde_json::Value) -> Result<TailNode, String> {
    let stable_id = value
        .get("StableID")
        .or_else(|| value.get("ID"))
        .and_then(|value| value.as_str())
        .ok_or("Tailscale 设备缺少稳定身份")?
        .to_string();
    let ips: Vec<String> = value
        .get("TailscaleIPs")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let dns = value
        .get("DNSName")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim_end_matches('.');
    let host = ips
        .iter()
        .find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())
        .cloned()
        .or_else(|| (!dns.is_empty()).then(|| dns.to_string()))
        .ok_or("Tailscale 设备没有可用地址")?;
    Ok(TailNode {
        stable_id,
        host,
        ips,
        os: value
            .get("OS")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        online: value
            .get("Online")
            .and_then(|value| value.as_bool())
            .unwrap_or(true),
    })
}

async fn peer_for_ip(ip: IpAddr) -> Result<TailNode, String> {
    let needle = ip.to_string();
    tailscale_status()
        .await?
        .peers
        .into_iter()
        .find(|peer| peer.ips.iter().any(|value| value == &needle))
        .ok_or_else(|| "请求来源不是当前 Tailscale 网络中的设备".to_string())
}

async fn resolve_peer_host(stable_id: &str) -> Result<String, String> {
    tailscale_status()
        .await?
        .peers
        .into_iter()
        .find(|peer| peer.stable_id == stable_id && peer.online)
        .map(|peer| peer.host)
        .ok_or_else(|| "已绑定的对方电脑当前不在线".to_string())
}

fn short_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())
}

fn screen_client_bytes(
    binding_id: &str,
    role: &str,
    public_key: &str,
    nonce: &str,
    timestamp_ms: u64,
) -> Vec<u8> {
    format!("screen-client-v1|{binding_id}|{role}|{public_key}|{nonce}|{timestamp_ms}").into_bytes()
}

fn screen_server_bytes(
    binding_id: &str,
    client_nonce: &str,
    server_nonce: &str,
    timestamp_ms: u64,
) -> Vec<u8> {
    format!("screen-server-v1|{binding_id}|{client_nonce}|{server_nonce}|{timestamp_ms}")
        .into_bytes()
}

fn unbind_request_bytes(core: &UnbindCore) -> Vec<u8> {
    format!(
        "unbind-request-v1|{}|{}|{}|{}|{}",
        core.request_id, core.binding_id, core.requested_by, core.created_at_ms, core.expires_at_ms
    )
    .into_bytes()
}

fn unbind_approval_bytes(
    request: &SignedUnbindRequest,
    approved_by: &str,
    approved_at_ms: u64,
) -> Vec<u8> {
    format!(
        "unbind-approve-v1|{}|{}|{}|{}",
        request.core.request_id, request.core.binding_id, approved_by, approved_at_ms
    )
    .into_bytes()
}

fn unbind_ack_bytes(
    request_id: &str,
    binding_id: &str,
    acknowledged_by: &str,
    acknowledged_at_ms: u64,
) -> Vec<u8> {
    format!("unbind-ack-v1|{request_id}|{binding_id}|{acknowledged_by}|{acknowledged_at_ms}")
        .into_bytes()
}

fn unbind_reject_bytes(
    request_id: &str,
    binding_id: &str,
    acknowledged_by: &str,
    acknowledged_at_ms: u64,
) -> Vec<u8> {
    format!("unbind-reject-v1|{request_id}|{binding_id}|{acknowledged_by}|{acknowledged_at_ms}")
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spake_pair_derives_the_same_key() {
        let password = Password::new(b"same private phrase");
        let (mac, mac_message) = Spake2::<Ed25519Group>::start_a(
            &password,
            &Identity::new(SPAKE_MAC_ID),
            &Identity::new(SPAKE_WINDOWS_ID),
        );
        let (windows, windows_message) = Spake2::<Ed25519Group>::start_b(
            &password,
            &Identity::new(SPAKE_MAC_ID),
            &Identity::new(SPAKE_WINDOWS_ID),
        );
        assert_eq!(
            mac.finish(&windows_message).unwrap(),
            windows.finish(&mac_message).unwrap()
        );
    }
}
