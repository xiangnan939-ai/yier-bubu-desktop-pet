import {
  authorizeRecord, failure, json, readActive, readBody, store, validateRecord, verifyEd25519,
} from "./_shared.js";

const ACTIVE_RECORD_KEY = "deployment/active-binding-record.json";

function validProof(proof, signature) {
  if (!proof || !["yier", "bubu"].includes(proof.role)
    || typeof proof.publicKey !== "string" || typeof proof.nonce !== "string"
    || proof.nonce.length < 16 || proof.nonce.length > 80
    || !Number.isFinite(proof.timestampMs)
    || Math.abs(Date.now() - proof.timestampMs) > 5 * 60_000) return false;
  const bytes = Buffer.from(
    `binding-recovery-v1|${proof.role}|${proof.publicKey}|${proof.nonce}|${proof.timestampMs}`,
  );
  return verifyEd25519(proof.publicKey, bytes, signature);
}

export async function onRequest({ request }) {
  try {
    if (request.method !== "POST") return failure("请求方法无效", 405);
    const body = await readBody(request);
    if (!validProof(body.proof, body.signature)) return failure("绑定恢复签名无效", 403);

    if (body.action === "upload") {
      const record = validateRecord(body.record);
      const enrollment = body.proof.role === "yier" ? record.core.mac : record.core.windows;
      if (body.proof.publicKey !== enrollment.publicKey) return failure("绑定恢复设备不匹配", 403);
      await authorizeRecord(record);
      await store().setJSON(ACTIVE_RECORD_KEY, record);
      return json({ state: "stored" });
    }

    if (body.action === "download") {
      const active = await readActive();
      if (active?.state !== "active" || body.proof.role !== "bubu"
        || body.proof.publicKey !== active.bubuPublicKey) {
        return failure("当前设备没有可恢复的绑定", 403);
      }
      const record = await store().get(
        ACTIVE_RECORD_KEY,
        { type: "json", consistency: "strong" },
      ).catch(() => null);
      if (!record) return failure("正在等待 Mac 同步绑定记录", 409);
      validateRecord(record);
      if (record.core.bindingId !== active.bindingId
        || record.core.mac.publicKey !== active.yierPublicKey
        || record.core.windows.publicKey !== active.bubuPublicKey) {
        return failure("云端绑定恢复记录不一致", 409);
      }
      return json({ state: "recovered", record });
    }

    return failure("绑定恢复操作无效");
  } catch (error) {
    return failure(error instanceof Error ? error.message : String(error), 403);
  }
}
