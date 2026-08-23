import { createPublicKey, verify as verifySignature } from "node:crypto";
import { getStore } from "@edgeone/pages-blob";

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const STORE_NAME = "yier-bubu-private-v1";
const ACTIVE_KEY = "deployment/active-binding.json";

export function store() {
  return getStore({ name: STORE_NAME, consistency: "strong" });
}

export function json(value, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
    },
  });
}

export function failure(message, status = 400) {
  return json({ error: message }, status);
}

export async function readBody(request, maxBytes = 96_000) {
  const declared = Number(request.headers.get("content-length") || 0);
  if (declared > maxBytes) throw new Error("请求数据过大");
  const text = await request.text();
  if (Buffer.byteLength(text) > maxBytes) throw new Error("请求数据过大");
  return JSON.parse(text);
}

function enrollmentV2(value) {
  const result = {
    role: value.role,
    petName: value.petName,
    publicKey: value.publicKey,
    deviceId: value.deviceId || "",
    signalingUserId: value.signalingUserId || "",
    machineFingerprint: value.machineFingerprint,
  };
  if (value.tailscaleStableId) result.tailscaleStableId = value.tailscaleStableId;
  if (value.tailscaleHost) result.tailscaleHost = value.tailscaleHost;
  return result;
}

function enrollmentV1(value) {
  return {
    role: value.role,
    petName: value.petName,
    publicKey: value.publicKey,
    tailscaleStableId: value.tailscaleStableId || "",
    tailscaleHost: value.tailscaleHost || "",
    machineFingerprint: value.machineFingerprint,
  };
}

export function bindingCoreBytes(core) {
  const enrollment = core.version === 1 ? enrollmentV1 : enrollmentV2;
  return Buffer.from(JSON.stringify({
    version: core.version,
    bindingId: core.bindingId,
    createdAtMs: core.createdAtMs,
    mac: enrollment(core.mac),
    windows: enrollment(core.windows),
  }));
}

export function verifyEd25519(publicKey, bytes, signature) {
  try {
    const rawKey = Buffer.from(publicKey, "base64");
    const rawSignature = Buffer.from(signature, "base64");
    if (rawKey.length !== 32 || rawSignature.length !== 64) return false;
    const key = createPublicKey({
      key: Buffer.concat([ED25519_SPKI_PREFIX, rawKey]),
      format: "der",
      type: "spki",
    });
    return verifySignature(null, Buffer.from(bytes), key, rawSignature);
  } catch {
    return false;
  }
}

export function validateRecord(record) {
  const core = record?.core;
  if (!core || ![1, 2].includes(core.version) || typeof core.bindingId !== "string"
    || core.bindingId.length < 16 || core.mac?.role !== "yier" || core.windows?.role !== "bubu") {
    throw new Error("绑定记录格式无效");
  }
  for (const enrollment of [core.mac, core.windows]) {
    if (typeof enrollment.publicKey !== "string" || typeof enrollment.machineFingerprint !== "string"
      || enrollment.machineFingerprint.length < 32) throw new Error("绑定设备信息无效");
  }
  const ownerYierPublicKey = process.env.YIER_BUBU_OWNER_YIER_PUBLIC_KEY;
  if (!ownerYierPublicKey) throw new Error("私有服务尚未写入一二的设备公钥");
  if (core.mac.publicKey !== ownerYierPublicKey) {
    throw new Error("该绑定不包含白名单中的一二设备");
  }
  const bytes = bindingCoreBytes(core);
  if (!verifyEd25519(core.mac.publicKey, bytes, record.macSignature)
    || !verifyEd25519(core.windows.publicKey, bytes, record.windowsSignature)) {
    throw new Error("绑定记录缺少双方有效签名");
  }
  return record;
}

export function identityFor(record, state = "active") {
  return {
    version: 1,
    state,
    bindingId: record.core.bindingId,
    yierPublicKey: record.core.mac.publicKey,
    bubuPublicKey: record.core.windows.publicKey,
    updatedAtMs: Date.now(),
  };
}

export async function readActive() {
  return await store().get(ACTIVE_KEY, { type: "json", consistency: "strong" }).catch(() => null);
}

function sameKeys(active, record) {
  return active?.yierPublicKey === record.core.mac.publicKey
    && active?.bubuPublicKey === record.core.windows.publicKey;
}

export async function authorizeRecord(record) {
  validateRecord(record);
  const dataStore = store();
  let active = await readActive();
  if (!active) {
    try {
      await dataStore.setJSON(ACTIVE_KEY, identityFor(record), { onlyIfNew: true });
    } catch {
      // A simultaneous request from the other bound device may have won the lock.
    }
    active = await readActive();
  }
  if (!active) throw new Error("无法创建私有设备白名单");
  if (active.state === "active") {
    if (active.bindingId !== record.core.bindingId || !sameKeys(active, record)) {
      throw new Error("该联网服务已锁定给另外两台设备");
    }
    return active;
  }
  if (active.state === "revoked" && sameKeys(active, record)
    && active.bindingId !== record.core.bindingId && record.core.createdAtMs > active.updatedAtMs) {
    active = identityFor(record);
    await dataStore.setJSON(ACTIVE_KEY, active);
    return active;
  }
  throw new Error("新绑定的设备密钥与原来两台电脑不一致");
}

export function enrollmentForRole(record, role) {
  if (role === "yier") return record.core.mac;
  if (role === "bubu") return record.core.windows;
  throw new Error("设备角色无效");
}

export function partnerRole(role) {
  return role === "yier" ? "bubu" : "yier";
}

export async function signalingUserId(record, role) {
  const enrollment = enrollmentForRole(record, role);
  if (enrollment.signalingUserId) return enrollment.signalingUserId;
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(record.core.bindingId));
  const hex = [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  return `yb_${hex.slice(0, 20)}_${role}`;
}

export async function revokeActive(record) {
  const active = await readActive();
  if (active?.state === "revoked" && active.bindingId === record.core.bindingId
    && sameKeys(active, record)) return;
  if (!active || active.state !== "active" || active.bindingId !== record.core.bindingId
    || !sameKeys(active, record)) throw new Error("联网白名单与解绑记录不匹配");
  await store().setJSON(ACTIVE_KEY, identityFor(record, "revoked"));
}

export function noLongerThan(value, length) {
  return typeof value === "string" && value.length > 0 && value.length <= length;
}
