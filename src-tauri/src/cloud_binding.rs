use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use rand_core::OsRng;
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS, Transport};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

const KEYRING_SERVICE: &str = "com.yierbubu.desktop-pet";
const STATE_FILE: &str = "private-binding.json";
const SIGNING_KEY_FILE: &str = "device-signing-key-v1";
const PAIRING_LIFETIME: Duration = Duration::from_secs(180);
const REQUEST_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const SIGNAL_LIFETIME: Duration = Duration::from_secs(120);
const SPAKE_MAC_ID: &[u8] = b"yier-bubu/mac/cloud-v2";
const SPAKE_WINDOWS_ID: &[u8] = b"yier-bubu/windows/cloud-v2";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEnrollment {
    pub role: String,
    pub pet_name: String,
    pub public_key: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub signaling_user_id: String,
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
    pub signaling_route: Option<String>,
    pub partner_user_id: Option<String>,
    pub partner_machine_code: Option<String>,
    pub created_at_ms: Option<u64>,
    pub incoming_unbind: bool,
    pub outgoing_unbind: bool,
    pub approval_pending: bool,
    pub requested_by_name: Option<String>,
    pub realtime_configured: bool,
    pub local_public_key: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingResult {
    pub state: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairHello {
    spake_message: String,
    binding_id: Option<String>,
    created_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairEnrollment {
    info: DeviceEnrollment,
    mac: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairExchangeEnvelope<T> {
    payload: T,
    timestamp_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalCore {
    pub version: u8,
    pub message_type: String,
    pub binding_id: String,
    pub sender_role: String,
    pub recipient_role: String,
    pub nonce: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedSignal {
    pub core: SignalCore,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalProcessResult {
    pub accepted: bool,
    pub event: String,
    pub reply: Option<SignedSignal>,
}

#[derive(Clone)]
pub struct BindingManager {
    app: AppHandle,
    state_path: PathBuf,
    state: Arc<RwLock<PersistedBinding>>,
    seen_signal_nonces: Arc<Mutex<HashMap<String, u64>>>,
    signing_key: Arc<SigningKey>,
}

impl BindingManager {
    pub fn new(app: AppHandle, app_data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&app_data_dir).map_err(|error| error.to_string())?;
        let signing_key = load_or_create_signing_key(&app_data_dir)?;
        let state_path = app_data_dir.join(STATE_FILE);
        let state = if state_path.exists() {
            serde_json::from_slice(&fs::read(&state_path).map_err(|error| error.to_string())?)
                .map_err(|error| format!("绑定信息损坏：{error}"))?
        } else {
            PersistedBinding::default()
        };
        Ok(Self {
            app,
            state_path,
            state: Arc::new(RwLock::new(state)),
            seen_signal_nonces: Arc::new(Mutex::new(HashMap::new())),
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
            state: lifecycle.into(),
            pet_name: local_pet_name().into(),
            partner_name: partner_pet_name().into(),
            binding_id: record.map(|value| value.core.binding_id.clone()),
            signaling_route: record.map(|value| {
                short_hash(&format!("yier-bubu-mqtt-v1|{}", value.core.binding_id), 64)
            }),
            partner_user_id: record.map(|value| signaling_user_id(&value.core, partner_role())),
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
            realtime_configured: true,
            local_public_key: self.public_key(),
        }
    }

    pub async fn pair(&self, passphrase: String) -> Result<PairingResult, String> {
        let passphrase = normalize_pairing_code(&passphrase);
        if passphrase.len() != 16 {
            return Err("请输入 Mac 生成的完整 16 位配对码".into());
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

        let channel = pair_channel(&passphrase);
        let is_mac = self.role() == "yier";
        let (spake, own_message) = if is_mac {
            Spake2::<Ed25519Group>::start_a(
                &Password::new(passphrase.as_bytes()),
                &Identity::new(SPAKE_MAC_ID),
                &Identity::new(SPAKE_WINDOWS_ID),
            )
        } else {
            Spake2::<Ed25519Group>::start_b(
                &Password::new(passphrase.as_bytes()),
                &Identity::new(SPAKE_MAC_ID),
                &Identity::new(SPAKE_WINDOWS_ID),
            )
        };
        let hello = PairHello {
            spake_message: BASE64.encode(own_message),
            binding_id: is_mac.then(|| Uuid::new_v4().to_string()),
            created_at_ms: is_mac.then(now_ms),
        };
        let peer_hello: PairHello = self
            .exchange_pair(&channel, "hello", &hello, "正在寻找对方电脑")
            .await?;
        let binding_id = hello
            .binding_id
            .clone()
            .or(peer_hello.binding_id.clone())
            .ok_or_else(|| "绑定会话缺少一二生成的绑定编号".to_string())?;
        let created_at_ms = hello
            .created_at_ms
            .or(peer_hello.created_at_ms)
            .ok_or_else(|| "绑定会话缺少创建时间".to_string())?;
        let peer_message = BASE64
            .decode(peer_hello.spake_message)
            .map_err(|_| "绑定握手数据无效".to_string())?;
        let key = spake
            .finish(&peer_message)
            .map_err(|_| "绑定口令握手失败".to_string())?;

        let local_info = self.local_enrollment(&binding_id)?;
        let enrollment = PairEnrollment {
            mac: payload_mac(&key, b"enrollment-v2", &local_info)?,
            info: local_info.clone(),
        };
        let peer_enrollment: PairEnrollment = self
            .exchange_pair(&channel, "enrollment", &enrollment, "正在核对设备身份")
            .await?;
        verify_payload_mac(
            &key,
            b"enrollment-v2",
            &peer_enrollment.info,
            &peer_enrollment.mac,
        )
        .map_err(|_| "双方绑定口令不一致".to_string())?;
        if peer_enrollment.info.role != partner_role() {
            return Err("联网绑定遇到了角色相同的电脑".into());
        }

        let (mac, windows) = if is_mac {
            (local_info, peer_enrollment.info)
        } else {
            (peer_enrollment.info, local_info)
        };
        let core = BindingCore {
            version: 2,
            binding_id,
            created_at_ms,
            mac,
            windows,
        };
        let own_signature = self.sign_serialized(&core)?;
        let peer_signature: String = self
            .exchange_pair(&channel, "signature", &own_signature, "正在签署绑定记录")
            .await?;
        let record = if is_mac {
            BindingRecord {
                core,
                mac_signature: own_signature,
                windows_signature: peer_signature,
            }
        } else {
            BindingRecord {
                core,
                mac_signature: peer_signature,
                windows_signature: own_signature,
            }
        };
        self.verify_complete_record(&record)?;
        self.replace_with_record(record).await?;
        Ok(PairingResult {
            state: "bound".into(),
            message: format!("已成功绑定我的{}，无需安装其他软件", local_pet_name()),
        })
    }

    async fn exchange_pair<T>(
        &self,
        channel: &str,
        phase: &str,
        payload: &T,
        waiting: &str,
    ) -> Result<T, String>
    where
        T: Serialize + for<'de> Deserialize<'de> + Clone + Send + 'static,
    {
        let deadline = now_ms() + PAIRING_LIFETIME.as_millis() as u64;
        let own_role = self.role().to_string();
        let peer_role = partner_role().to_string();
        let envelope = PairExchangeEnvelope {
            payload: payload.clone(),
            timestamp_ms: now_ms(),
        };
        let outbound = format!("yier-bubu/pair/v3/{channel}/{phase}/{own_role}");
        let inbound = format!("yier-bubu/pair/v3/{channel}/{phase}/{peer_role}");
        let encoded = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
        let primary = mqtt_pair_exchange::<T>(
            "broker.emqx.io",
            8883,
            outbound.clone(),
            inbound.clone(),
            encoded.clone(),
            deadline,
        );
        let fallback = mqtt_pair_exchange::<T>(
            "broker.hivemq.com",
            8883,
            outbound,
            inbound,
            encoded,
            deadline,
        );
        tokio::pin!(primary, fallback);
        tokio::select! {
            result = &mut primary => match result {
                Ok(value) => Ok(value),
                Err(first) => fallback.await.map_err(|second| format!("绑定超时（{waiting}）：{first}；{second}")),
            },
            result = &mut fallback => match result {
                Ok(value) => Ok(value),
                Err(first) => primary.await.map_err(|second| format!("绑定超时（{waiting}）：{first}；{second}")),
            },
        }
    }

    pub async fn make_signal(
        &self,
        message_type: String,
        payload: Value,
    ) -> Result<SignedSignal, String> {
        let record = self.communication_record().await?;
        self.ensure_local_machine(&record)?;
        self.make_signal_for_record(&record, message_type, payload)
    }

    fn make_signal_for_record(
        &self,
        record: &BindingRecord,
        message_type: String,
        payload: Value,
    ) -> Result<SignedSignal, String> {
        let created_at_ms = now_ms();
        let lifetime = if message_type.starts_with("unbind") {
            REQUEST_LIFETIME
        } else {
            SIGNAL_LIFETIME
        };
        let core = SignalCore {
            version: 1,
            message_type,
            binding_id: record.core.binding_id.clone(),
            sender_role: self.role().into(),
            recipient_role: partner_role().into(),
            nonce: Uuid::new_v4().to_string(),
            created_at_ms,
            expires_at_ms: created_at_ms + lifetime.as_millis() as u64,
            payload,
        };
        Ok(SignedSignal {
            signature: self.sign_serialized(&core)?,
            core,
        })
    }

    pub async fn process_signal(
        &self,
        signal: SignedSignal,
    ) -> Result<SignalProcessResult, String> {
        let record = self.communication_record().await?;
        let partner = partner_enrollment(&record.core);
        if signal.core.version != 1
            || signal.core.binding_id != record.core.binding_id
            || signal.core.sender_role != partner.role
            || signal.core.recipient_role != self.role()
            || signal.core.expires_at_ms < now_ms()
            || signal.core.created_at_ms > now_ms() + 60_000
        {
            return Err("收到的联网消息与当前绑定不匹配".into());
        }
        verify_serialized(&partner.public_key, &signal.core, &signal.signature)?;
        {
            let mut seen = self.seen_signal_nonces.lock().await;
            seen.retain(|_, timestamp| now_ms().saturating_sub(*timestamp) <= 10 * 60_000);
            if seen
                .insert(signal.core.nonce.clone(), signal.core.created_at_ms)
                .is_some()
            {
                return Err("检测到重复的联网消息".into());
            }
        }

        let mut reply = None;
        let event = match signal.core.message_type.as_str() {
            "viewRequest" => "view-request",
            "viewAnswer" => "view-answer",
            "viewIce" => "view-ice",
            "viewStop" => "view-stop",
            "viewError" => "view-error",
            "unbindRequest" => {
                let request: SignedUnbindRequest = serde_json::from_value(signal.core.payload)
                    .map_err(|error| error.to_string())?;
                self.receive_unbind_request(request).await?;
                "unbind-request"
            }
            "unbindApproval" => {
                let approval: UnbindApproval = serde_json::from_value(signal.core.payload)
                    .map_err(|error| error.to_string())?;
                let ack = self.receive_unbind_approval(approval).await?;
                reply = Some(self.make_signal_for_record(
                    &record,
                    "unbindAck".into(),
                    serde_json::to_value(ack).map_err(|error| error.to_string())?,
                )?);
                "unbind-approved"
            }
            "unbindReject" => {
                let rejection: UnbindAck = serde_json::from_value(signal.core.payload)
                    .map_err(|error| error.to_string())?;
                self.receive_unbind_rejection(rejection).await?;
                "unbind-rejected"
            }
            "unbindAck" => {
                let ack: UnbindAck = serde_json::from_value(signal.core.payload)
                    .map_err(|error| error.to_string())?;
                self.receive_unbind_ack(ack).await?;
                "unbind-complete"
            }
            _ => return Err("收到未知类型的联网消息".into()),
        };
        Ok(SignalProcessResult {
            accepted: true,
            event: event.into(),
            reply,
        })
    }

    pub async fn request_unbind(&self) -> Result<SignedSignal, String> {
        let record = self.active_record().await?;
        let created_at_ms = now_ms();
        let core = UnbindCore {
            request_id: Uuid::new_v4().to_string(),
            binding_id: record.core.binding_id.clone(),
            requested_by: self.role().into(),
            created_at_ms,
            expires_at_ms: created_at_ms + REQUEST_LIFETIME.as_millis() as u64,
        };
        let request = SignedUnbindRequest {
            signature: self.sign_bytes(&unbind_request_bytes(&core)),
            core,
        };
        self.mutate_state(|state| state.outgoing_unbind = Some(request.clone()))
            .await?;
        self.make_signal(
            "unbindRequest".into(),
            serde_json::to_value(request).map_err(|error| error.to_string())?,
        )
        .await
    }

    pub async fn respond_unbind(&self, approve: bool) -> Result<SignedSignal, String> {
        let record = self.communication_record().await?;
        let state = self.state.read().await.clone();
        let request = state
            .incoming_unbind
            .clone()
            .or_else(|| {
                state
                    .approved_unbind
                    .as_ref()
                    .map(|value| value.request.clone())
            })
            .ok_or_else(|| "当前没有等待处理的解绑请求".to_string())?;
        if !approve {
            let rejected_at = now_ms();
            let rejection = UnbindAck {
                request_id: request.core.request_id.clone(),
                binding_id: record.core.binding_id,
                acknowledged_by: self.role().into(),
                acknowledged_at_ms: rejected_at,
                signature: self.sign_bytes(&unbind_reject_bytes(
                    &request.core.request_id,
                    &request.core.binding_id,
                    self.role(),
                    rejected_at,
                )),
            };
            self.mutate_state(|state| state.incoming_unbind = None)
                .await?;
            return self
                .make_signal(
                    "unbindReject".into(),
                    serde_json::to_value(rejection).map_err(|error| error.to_string())?,
                )
                .await;
        }

        let approval = state.approved_unbind.unwrap_or_else(|| {
            let approved_at_ms = now_ms();
            UnbindApproval {
                signature: self.sign_bytes(&unbind_approval_bytes(
                    &request,
                    self.role(),
                    approved_at_ms,
                )),
                request,
                approved_by: self.role().into(),
                approved_at_ms,
            }
        });
        self.mutate_state(|state| state.approved_unbind = Some(approval.clone()))
            .await?;
        self.make_signal(
            "unbindApproval".into(),
            serde_json::to_value(approval).map_err(|error| error.to_string())?,
        )
        .await
    }

    async fn receive_unbind_request(&self, request: SignedUnbindRequest) -> Result<(), String> {
        let record = self.active_record().await?;
        let partner = partner_enrollment(&record.core);
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

    async fn receive_unbind_approval(&self, approval: UnbindApproval) -> Result<UnbindAck, String> {
        let record = self.active_record().await?;
        let partner = partner_enrollment(&record.core);
        let outgoing = self
            .state
            .read()
            .await
            .outgoing_unbind
            .clone()
            .ok_or_else(|| "本机没有对应的解绑请求".to_string())?;
        if approval.request != outgoing || approval.approved_by != partner.role {
            return Err("解绑批准与本机请求不匹配".into());
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
        let acknowledged_at_ms = now_ms();
        let ack = UnbindAck {
            request_id: approval.request.core.request_id.clone(),
            binding_id: record.core.binding_id.clone(),
            acknowledged_by: self.role().into(),
            acknowledged_at_ms,
            signature: self.sign_bytes(&unbind_ack_bytes(
                &approval.request.core.request_id,
                &approval.request.core.binding_id,
                self.role(),
                acknowledged_at_ms,
            )),
        };
        self.mutate_state(|state| {
            state.approved_unbind = Some(approval);
            state.outgoing_unbind = None;
            state.revoked = true;
        })
        .await?;
        Ok(ack)
    }

    async fn receive_unbind_ack(&self, ack: UnbindAck) -> Result<(), String> {
        let record = self.communication_record().await?;
        let partner = partner_enrollment(&record.core);
        let approval = self
            .state
            .read()
            .await
            .approved_unbind
            .clone()
            .ok_or_else(|| "本机没有等待确认的解绑批准".to_string())?;
        if ack.request_id != approval.request.core.request_id
            || ack.binding_id != record.core.binding_id
            || ack.acknowledged_by != partner.role
        {
            return Err("解绑确认与当前请求不匹配".into());
        }
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
        self.mutate_state(|state| {
            state.revoked = true;
            state.incoming_unbind = None;
        })
        .await?;
        Ok(())
    }

    async fn receive_unbind_rejection(&self, rejection: UnbindAck) -> Result<(), String> {
        let record = self.active_record().await?;
        let partner = partner_enrollment(&record.core);
        let outgoing = self
            .state
            .read()
            .await
            .outgoing_unbind
            .clone()
            .ok_or_else(|| "本机没有对应的解绑请求".to_string())?;
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
        Ok(())
    }

    fn local_enrollment(&self, binding_id: &str) -> Result<DeviceEnrollment, String> {
        let public_key = self.public_key();
        let device_id = short_hash(&public_key, 24);
        Ok(DeviceEnrollment {
            role: self.role().into(),
            pet_name: local_pet_name().into(),
            public_key,
            signaling_user_id: signaling_user_id_from_parts(binding_id, self.role()),
            device_id,
            machine_fingerprint: machine_fingerprint(binding_id)?,
        })
    }

    fn public_key(&self) -> String {
        BASE64.encode(self.signing_key.verifying_key().as_bytes())
    }

    fn ensure_local_machine(&self, record: &BindingRecord) -> Result<(), String> {
        let local = local_enrollment(&record.core);
        if machine_fingerprint(&record.core.binding_id)? != local.machine_fingerprint {
            return Err("本机机器特征与绑定记录不一致".into());
        }
        if local.public_key != self.public_key() {
            return Err("本机设备密钥与绑定记录不一致".into());
        }
        Ok(())
    }

    async fn active_record(&self) -> Result<BindingRecord, String> {
        let state = self.state.read().await;
        if state.revoked || state.approved_unbind.is_some() {
            return Err("当前绑定正在解绑或已经解除".into());
        }
        state
            .record
            .clone()
            .ok_or_else(|| "请先完成一二与布布的首次绑定".to_string())
    }

    async fn communication_record(&self) -> Result<BindingRecord, String> {
        let state = self.state.read().await;
        if state.revoked {
            return Err("当前绑定已经解除".into());
        }
        state
            .record
            .clone()
            .ok_or_else(|| "请先完成一二与布布的首次绑定".to_string())
    }

    fn verify_complete_record(&self, record: &BindingRecord) -> Result<(), String> {
        if record.core.mac.role != "yier" || record.core.windows.role != "bubu" {
            return Err("绑定记录中的设备角色无效".into());
        }
        let core_bytes = binding_core_bytes(&record.core)?;
        verify_bytes(
            &record.core.mac.public_key,
            &core_bytes,
            &record.mac_signature,
        )?;
        verify_bytes(
            &record.core.windows.public_key,
            &core_bytes,
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

    async fn mutate_state(&self, update: impl FnOnce(&mut PersistedBinding)) -> Result<(), String> {
        let bytes = {
            let mut state = self.state.write().await;
            update(&mut state);
            serde_json::to_vec_pretty(&*state).map_err(|error| error.to_string())?
        };
        atomic_write(&self.state_path, &bytes)?;
        let _ = self.app.emit("binding-changed", ());
        Ok(())
    }

    fn sign_serialized<T: Serialize>(&self, value: &T) -> Result<String, String> {
        Ok(self.sign_bytes(&serde_json::to_vec(value).map_err(|error| error.to_string())?))
    }

    fn sign_bytes(&self, bytes: &[u8]) -> String {
        BASE64.encode(self.signing_key.sign(bytes).to_bytes())
    }
}

fn binding_core_bytes(core: &BindingCore) -> Result<Vec<u8>, String> {
    serde_json::to_vec(core).map_err(|error| error.to_string())
}

async fn mqtt_pair_exchange<T>(
    host: &str,
    port: u16,
    outbound_topic: String,
    inbound_topic: String,
    payload: Vec<u8>,
    deadline_ms: u64,
) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client_id = format!("yb_pair_{}", Uuid::new_v4().simple());
    let mut options = MqttOptions::new(client_id, host, port);
    options.set_keep_alive(Duration::from_secs(20));
    options.set_clean_session(true);
    options.set_transport(Transport::tls_with_default_config());
    let (client, mut event_loop) = AsyncClient::new(options, 20);
    client
        .subscribe(inbound_topic.clone(), QoS::AtLeastOnce)
        .await
        .map_err(|error| error.to_string())?;
    client
        .publish(outbound_topic, QoS::AtLeastOnce, true, payload)
        .await
        .map_err(|error| error.to_string())?;

    loop {
        let remaining = Duration::from_millis(deadline_ms.saturating_sub(now_ms()));
        if remaining.is_zero() {
            return Err(format!("{host} 等待对方超时"));
        }
        let event = tokio::time::timeout(remaining, event_loop.poll())
            .await
            .map_err(|_| format!("{host} 等待对方超时"))?
            .map_err(|error| format!("{host}：{error}"))?;
        if let Event::Incoming(Incoming::Publish(message)) = event {
            if message.topic != inbound_topic {
                continue;
            }
            let envelope: PairExchangeEnvelope<T> = serde_json::from_slice(&message.payload)
                .map_err(|error| format!("绑定交换消息无效：{error}"))?;
            if now_ms().saturating_sub(envelope.timestamp_ms) > 5 * 60_000 {
                continue;
            }
            let _ = client.disconnect().await;
            return Ok(envelope.payload);
        }
    }
}

fn pair_channel(passphrase: &str) -> String {
    short_hash(&format!("yier-bubu-pair-v2|{passphrase}"), 48)
}

fn normalize_pairing_code(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_uppercase())
        .collect()
}

fn signaling_user_id(core: &BindingCore, role: &str) -> String {
    let enrollment = if role == "yier" {
        &core.mac
    } else {
        &core.windows
    };
    if enrollment.signaling_user_id.is_empty() {
        signaling_user_id_from_parts(&core.binding_id, role)
    } else {
        enrollment.signaling_user_id.clone()
    }
}

fn signaling_user_id_from_parts(binding_id: &str, role: &str) -> String {
    format!("yb_{}_{}", &short_hash(binding_id, 20), role)
}

fn short_hash(value: &str, length: usize) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex_encode(&digest)[..length.min(digest.len() * 2)].to_string()
}

fn decode_signing_key(encoded: &str) -> Result<SigningKey, String> {
    let bytes = BASE64
        .decode(encoded.trim())
        .map_err(|_| "系统安全存储中的设备密钥损坏".to_string())?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "系统安全存储中的设备密钥长度无效".to_string())?;
    Ok(SigningKey::from_bytes(&bytes))
}

#[cfg(target_os = "macos")]
fn load_or_create_signing_key(app_data_dir: &Path) -> Result<SigningKey, String> {
    use std::os::unix::fs::PermissionsExt;

    let fallback_path = app_data_dir.join(SIGNING_KEY_FILE);
    if fallback_path.exists() {
        return decode_signing_key(
            &fs::read_to_string(&fallback_path).map_err(|error| error.to_string())?,
        );
    }

    let entry = keyring::Entry::new(KEYRING_SERVICE, "device-signing-key")
        .map_err(|error| error.to_string())?;
    let key = match entry.get_password() {
        Ok(encoded) => decode_signing_key(&encoded)?,
        Err(keyring::Error::NoEntry) => {
            let key = SigningKey::generate(&mut OsRng);
            if let Err(error) = entry.set_password(&BASE64.encode(key.to_bytes())) {
                eprintln!("Mac 系统安全存储不可写，改用仅当前用户可读的设备密钥文件：{error}");
            }
            key
        }
        Err(error) => {
            eprintln!("Mac 系统安全存储不可读，改用仅当前用户可读的设备密钥文件：{error}");
            SigningKey::generate(&mut OsRng)
        }
    };

    let temporary = fallback_path.with_extension("tmp");
    fs::write(&temporary, BASE64.encode(key.to_bytes())).map_err(|error| error.to_string())?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    fs::rename(temporary, fallback_path).map_err(|error| error.to_string())?;
    Ok(key)
}

#[cfg(not(target_os = "macos"))]
fn load_or_create_signing_key(_app_data_dir: &Path) -> Result<SigningKey, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, "device-signing-key")
        .map_err(|error| error.to_string())?;
    match entry.get_password() {
        Ok(encoded) => decode_signing_key(&encoded),
        Err(keyring::Error::NoEntry) => {
            let key = SigningKey::generate(&mut OsRng);
            entry
                .set_password(&BASE64.encode(key.to_bytes()))
                .map_err(|error| format!("无法写入系统安全存储：{error}"))?;
            Ok(key)
        }
        Err(error) => Err(format!("无法读取系统安全存储：{error}")),
    }
}

fn machine_fingerprint(binding_id: &str) -> Result<String, String> {
    let raw = platform_machine_material()?;
    let mut hasher = Sha256::new();
    hasher.update(b"yier-bubu-machine-v1");
    hasher.update(binding_id.as_bytes());
    hasher.update(raw.as_bytes());
    Ok(hex_encode(&hasher.finalize()))
}

#[cfg(target_os = "macos")]
fn platform_machine_material() -> Result<String, String> {
    let output = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find_map(|line| {
            line.split_once("IOPlatformUUID")
                .and_then(|(_, value)| value.split('"').nth(2))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .ok_or_else(|| "无法读取 Mac 机器特征".to_string())
}

#[cfg(target_os = "windows")]
fn platform_machine_material() -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    use std::sync::OnceLock;

    // Reading the hardware UUID is needed by several binding checks. Keep the
    // first successful value for this process so normal pet actions do not
    // repeatedly launch a system helper.
    static MACHINE_MATERIAL: OnceLock<String> = OnceLock::new();
    if let Some(value) = MACHINE_MATERIAL.get() {
        return Ok(value.clone());
    }

    // PowerShell is a console application. Without CREATE_NO_WINDOW Windows
    // briefly shows a cmd-style window whenever a binding check runs.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new("powershell.exe");
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            "(Get-CimInstance Win32_ComputerSystemProduct).UUID",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        Err("无法读取 Windows 机器特征".into())
    } else {
        let _ = MACHINE_MATERIAL.set(value.clone());
        Ok(value)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_machine_material() -> Result<String, String> {
    Ok("unsupported-development-platform".into())
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
    encoded: &str,
) -> Result<(), String> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| error.to_string())?;
    mac.update(label);
    mac.update(&serde_json::to_vec(value).map_err(|error| error.to_string())?);
    let bytes = BASE64
        .decode(encoded)
        .map_err(|_| "绑定校验数据无效".to_string())?;
    mac.verify_slice(&bytes)
        .map_err(|_| "绑定校验失败".to_string())
}

fn verify_serialized<T: Serialize>(
    public_key: &str,
    value: &T,
    signature: &str,
) -> Result<(), String> {
    verify_bytes(
        public_key,
        &serde_json::to_vec(value).map_err(|error| error.to_string())?,
        signature,
    )
}

fn verify_bytes(public_key: &str, bytes: &[u8], signature: &str) -> Result<(), String> {
    let key_bytes: [u8; 32] = BASE64
        .decode(public_key)
        .map_err(|_| "设备公钥格式无效".to_string())?
        .try_into()
        .map_err(|_| "设备公钥长度无效".to_string())?;
    let signature_bytes: [u8; 64] = BASE64
        .decode(signature)
        .map_err(|_| "设备签名格式无效".to_string())?
        .try_into()
        .map_err(|_| "设备签名长度无效".to_string())?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|error| error.to_string())?;
    key.verify(bytes, &Signature::from_bytes(&signature_bytes))
        .map_err(|_| "设备数字签名校验失败".to_string())
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

fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn short_code(value: &str) -> String {
    value.chars().take(12).collect::<String>().to_uppercase()
}

fn partner_enrollment(core: &BindingCore) -> &DeviceEnrollment {
    if platform_role() == "yier" {
        &core.windows
    } else {
        &core.mac
    }
}

fn local_enrollment(core: &BindingCore) -> &DeviceEnrollment {
    if platform_role() == "yier" {
        &core.mac
    } else {
        &core.windows
    }
}

fn platform_role() -> &'static str {
    if cfg!(target_os = "windows") {
        "bubu"
    } else {
        "yier"
    }
}

fn partner_role() -> &'static str {
    if platform_role() == "yier" {
        "bubu"
    } else {
        "yier"
    }
}

fn local_pet_name() -> &'static str {
    if platform_role() == "yier" {
        "一二"
    } else {
        "布布"
    }
}

fn partner_pet_name() -> &'static str {
    if platform_role() == "yier" {
        "布布"
    } else {
        "一二"
    }
}

fn pet_name_for_role(role: &str) -> &'static str {
    if role == "yier" {
        "一二"
    } else {
        "布布"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signaling_ids_are_stable_and_valid() {
        let value = signaling_user_id_from_parts("ea8e5021-0000-4000-8000-abcdef123456", "yier");
        assert!(value.starts_with("yb_"));
        assert!(value.ends_with("_yier"));
        assert!(value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '_'));
    }

    #[test]
    fn pairing_code_normalization_ignores_grouping_and_case() {
        assert_eq!(
            normalize_pairing_code("abcd-2345 efgh-6789"),
            "ABCD2345EFGH6789"
        );
    }

    #[test]
    fn signing_key_encoding_round_trip_is_stable() {
        let original = SigningKey::from_bytes(&[7; 32]);
        let decoded = decode_signing_key(&BASE64.encode(original.to_bytes())).unwrap();
        assert_eq!(decoded.verifying_key(), original.verifying_key());
    }

    #[tokio::test]
    #[ignore = "requires the two public MQTT endpoints"]
    async fn public_pairing_brokers_exchange_messages() {
        for (host, port) in [("broker.emqx.io", 8883), ("broker.hivemq.com", 8883)] {
            let route = Uuid::new_v4().simple().to_string();
            let mac_topic = format!("yier-bubu/diagnostic/{route}/mac");
            let windows_topic = format!("yier-bubu/diagnostic/{route}/windows");
            let deadline = now_ms() + 30_000;
            let encode = |payload: &str| {
                serde_json::to_vec(&PairExchangeEnvelope {
                    payload: payload.to_string(),
                    timestamp_ms: now_ms(),
                })
                .unwrap()
            };
            let mac = mqtt_pair_exchange::<String>(
                host,
                port,
                mac_topic.clone(),
                windows_topic.clone(),
                encode("mac"),
                deadline,
            );
            let windows = mqtt_pair_exchange::<String>(
                host,
                port,
                windows_topic,
                mac_topic,
                encode("windows"),
                deadline,
            );
            let (mac_result, windows_result) = tokio::join!(mac, windows);
            assert_eq!(mac_result.unwrap(), "windows");
            assert_eq!(windows_result.unwrap(), "mac");
        }
    }
}
