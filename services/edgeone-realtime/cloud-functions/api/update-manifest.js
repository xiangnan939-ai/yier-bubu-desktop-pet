import { characterAssetName, fetchReleaseJson } from "./_updates.js";
import { failure, json } from "./_shared.js";

export async function onRequest({ request }) {
  if (request.method !== "GET") return new Response("Method not allowed", { status: 405 });
  try {
    const manifest = await fetchReleaseJson("asset-manifest.json");
    const asset = characterAssetName(manifest.packUrl);
    if (!asset) throw new Error("invalid character asset");
    return json(manifest);
  } catch {
    return failure("素材更新源暂时不可用", 502);
  }
}
