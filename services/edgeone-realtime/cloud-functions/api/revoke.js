import {
  enrollmentForRole, failure, json, partnerRole, readBody, revokeActive,
  validateRecord, verifyEd25519,
} from "./_shared.js";

export async function onRequest({ request }) {
  try {
    if (request.method !== "POST") return failure("请求方法无效", 405);
    const { record, approval, ack } = await readBody(request);
    validateRecord(record);
    const unbind = approval?.request;
    const core = unbind?.core;
    if (!core || core.bindingId !== record.core.bindingId || core.expiresAtMs < Date.now()
      || approval.approvedBy !== partnerRole(core.requestedBy)
      || ack?.requestId !== core.requestId || ack.bindingId !== core.bindingId
      || ack.acknowledgedBy !== core.requestedBy) {
      return failure("双向解绑证明不完整", 403);
    }
    const requester = enrollmentForRole(record, core.requestedBy);
    const approver = enrollmentForRole(record, approval.approvedBy);
    const requestBytes = Buffer.from(`unbind-request-v1|${core.requestId}|${core.bindingId}|${core.requestedBy}|${core.createdAtMs}|${core.expiresAtMs}`);
    const approvalBytes = Buffer.from(`unbind-approve-v1|${core.requestId}|${core.bindingId}|${approval.approvedBy}|${approval.approvedAtMs}`);
    const ackBytes = Buffer.from(`unbind-ack-v1|${ack.requestId}|${ack.bindingId}|${ack.acknowledgedBy}|${ack.acknowledgedAtMs}`);
    if (!verifyEd25519(requester.publicKey, requestBytes, unbind.signature)
      || !verifyEd25519(approver.publicKey, approvalBytes, approval.signature)
      || !verifyEd25519(requester.publicKey, ackBytes, ack.signature)) {
      return failure("双向解绑签名无效", 403);
    }
    await revokeActive(record);
    return json({ revoked: true, bindingId: core.bindingId });
  } catch (error) {
    return failure(error instanceof Error ? error.message : String(error), 403);
  }
}
