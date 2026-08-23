import { json, readActive } from "./_shared.js";

export async function onRequest({ request }) {
  if (request.method !== "GET") return new Response("Method not allowed", { status: 405 });
  const active = await readActive();
  return json({ ok: true, locked: active?.state === "active", service: "yier-bubu-private-v1" });
}
