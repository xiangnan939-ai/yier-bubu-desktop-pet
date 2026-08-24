import {
  failure, json, noLongerThan, readActive, readBody, store, verifyEd25519,
} from "./_shared.js";

const PHASES = new Set(["hello", "enrollment", "signature"]);
const ROLES = new Set(["yier", "bubu"]);

export async function onRequest({ request }) {
  try {
    const body = await readBody(request);
    if (request.method === "DELETE") {
      if (!/^[a-f0-9]{48}$/.test(body.channel || "")) return failure("绑定频道无效");
      await Promise.all([...PHASES].flatMap((phase) => [...ROLES].map((role) =>
        store().delete(`pair/${body.channel}/${phase}/${role}.json`).catch(() => undefined))));
      return json({ cleaned: true });
    }
    if (request.method !== "POST") return failure("请求方法无效", 405);
    if (!/^[a-f0-9]{48}$/.test(body.channel || "") || !PHASES.has(body.phase)
      || !ROLES.has(body.role) || !body.payload || Math.abs(Date.now() - body.timestampMs) > 5 * 60_000) {
      return failure("绑定交换请求无效");
    }
    const ownerPublicKey = process.env.YIER_BUBU_OWNER_YIER_PUBLIC_KEY;
    if (!ownerPublicKey) return failure("私有服务尚未写入一二的设备公钥", 503);
    const dataStore = store();
    const ownerKey = `pair/${body.channel}/owner.json`;
    if (body.role === "yier") {
      const bytes = Buffer.from(`pair-channel-v1|${body.channel}`);
      if (!verifyEd25519(ownerPublicKey, bytes, body.ownerAuthorization)) {
        return failure("这不是白名单中的一二设备", 403);
      }
      await dataStore.setJSON(ownerKey, { timestampMs: Date.now() });
    } else {
      const owner = await dataStore.get(ownerKey, { type: "json", consistency: "strong" }).catch(() => null);
      if (!owner || Date.now() - owner.timestampMs > 4 * 60_000) {
        return failure("请先在 Mac 的一二上输入同一口令", 403);
      }
    }
    const active = await readActive();
    if (active?.state === "active") {
      const macHello = await dataStore.get(
        `pair/${body.channel}/hello/yier.json`,
        { type: "json", consistency: "strong" },
      ).catch(() => null);
      const samePairing = macHello?.payload?.bindingId === active.bindingId
        && Date.now() - macHello.timestampMs <= 4 * 60_000;
      if (!samePairing) return failure("私有双机服务已完成锁定", 423);
    }
    if (!noLongerThan(JSON.stringify(body.payload), 48_000)) return failure("绑定交换数据无效");
    const ownKey = `pair/${body.channel}/${body.phase}/${body.role}.json`;
    const peerRole = body.role === "yier" ? "bubu" : "yier";
    const peerKey = `pair/${body.channel}/${body.phase}/${peerRole}.json`;
    await dataStore.setJSON(ownKey, { payload: body.payload, timestampMs: Date.now() });
    const peer = await dataStore.get(peerKey, { type: "json", consistency: "strong" }).catch(() => null);
    if (peer && Date.now() - peer.timestampMs > 4 * 60_000) {
      await dataStore.delete(peerKey).catch(() => undefined);
      return json({ peerPayload: null });
    }
    return json({ peerPayload: peer?.payload ?? null });
  } catch (error) {
    return failure(error instanceof Error ? error.message : String(error));
  }
}
