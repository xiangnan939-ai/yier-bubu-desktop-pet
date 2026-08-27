import { fetchReleaseJson, localUrl, programAssetName } from "./_updates.js";
import { failure, json } from "./_shared.js";

export async function onRequest({ request }) {
  if (request.method !== "GET") return new Response("Method not allowed", { status: 405 });
  try {
    const release = await fetchReleaseJson("latest.json");
    for (const platform of Object.values(release.platforms || {})) {
      const asset = programAssetName(platform?.url);
      if (!asset) throw new Error("invalid program asset");
      platform.url = localUrl(request, "/api/update-program", asset);
    }
    return json(release);
  } catch {
    return failure("更新源暂时不可用", 502);
  }
}
