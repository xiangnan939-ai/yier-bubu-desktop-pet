import { PROGRAM_ASSET_PATTERN, proxyAsset } from "./_updates.js";

export async function onRequest({ request }) {
  if (!['GET', 'HEAD'].includes(request.method)) {
    return new Response("Method not allowed", { status: 405 });
  }
  return await proxyAsset(request, PROGRAM_ASSET_PATTERN);
}
