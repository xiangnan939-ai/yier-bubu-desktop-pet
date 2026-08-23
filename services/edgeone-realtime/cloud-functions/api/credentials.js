import {
  authorizeRecord, enrollmentForRole, failure, generateUserSig, json, partnerRole,
  readBody, signalingUserId, verifyEd25519,
} from "./_shared.js";

const EXPIRE_SECONDS = 30 * 24 * 60 * 60;

export async function onRequest({ request }) {
  try {
    if (request.method !== "POST") return failure("请求方法无效", 405);
    const sdkAppId = Number(process.env.TENCENT_SDK_APP_ID);
    const sdkSecret = process.env.TENCENT_SDK_SECRET;
    if (!Number.isInteger(sdkAppId) || !sdkSecret) return failure("腾讯实时应用尚未配置", 503);
    const body = await readBody(request);
    const { record, request: proof, signature } = body;
    await authorizeRecord(record);
    const enrollment = enrollmentForRole(record, proof?.role);
    if (!proof || proof.bindingId !== record.core.bindingId || proof.publicKey !== enrollment.publicKey
      || typeof proof.nonce !== "string" || proof.nonce.length > 80
      || Math.abs(Date.now() - proof.timestampMs) > 5 * 60_000) {
      return failure("联网凭证请求与绑定记录不匹配", 403);
    }
    const proofBytes = Buffer.from(`credential-v1|${proof.bindingId}|${proof.role}|${proof.publicKey}|${proof.nonce}|${proof.timestampMs}`);
    if (!verifyEd25519(proof.publicKey, proofBytes, signature)) return failure("联网凭证请求签名无效", 403);
    const userId = await signalingUserId(record, proof.role);
    const partnerUserId = await signalingUserId(record, partnerRole(proof.role));
    const userSig = generateUserSig(sdkAppId, sdkSecret, userId, EXPIRE_SECONDS);
    return json({
      sdkAppId,
      userId,
      partnerUserId,
      userSig,
      expiresAtMs: Date.now() + EXPIRE_SECONDS * 1_000,
    });
  } catch (error) {
    return failure(error instanceof Error ? error.message : String(error), 403);
  }
}
