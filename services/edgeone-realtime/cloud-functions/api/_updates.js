const RELEASE_BASE = "https://github.com/xiangnan939-ai/yier-bubu-desktop-pet/releases/latest/download";
const PUBLIC_ORIGIN = "https://yier-bubu-private-realtime-btqjaq8u.edgeone.dev";
const PROGRAM_ASSET = /^yier-bubu_\d+\.\d+\.\d+_(?:aarch64\.app\.tar\.gz|x64-setup\.exe)$/;
const CHARACTER_ASSET = /^character-assets-\d+\.\d+\.\d+\.zip$/;

function upstreamUrl(asset) {
  return `${RELEASE_BASE}/${encodeURIComponent(asset)}`;
}

export async function fetchReleaseJson(name) {
  const response = await fetch(upstreamUrl(name), {
    headers: { "user-agent": "yier-bubu-edgeone-updater/1" },
  });
  if (!response.ok) throw new Error(`GitHub release returned ${response.status}`);
  return await response.json();
}

export function assetName(value, pattern) {
  try {
    const name = decodeURIComponent(new URL(value).pathname.split("/").pop() || "");
    return pattern.test(name) ? name : null;
  } catch {
    return null;
  }
}

export function programAssetName(value) {
  return assetName(value, PROGRAM_ASSET);
}

export function characterAssetName(value) {
  return assetName(value, CHARACTER_ASSET);
}

export function localUrl(request, path, asset) {
  // Cloud Functions expose an internal qcloudteo origin in request.url. Never
  // leak that non-public HTTP address into the signed updater response.
  const url = new URL(path, PUBLIC_ORIGIN);
  url.searchParams.set("asset", asset);
  return url.toString();
}

export async function proxyAsset(request, pattern) {
  const asset = new URL(request.url).searchParams.get("asset") || "";
  if (!pattern.test(asset)) return new Response("Invalid update asset", { status: 400 });
  const headers = { "user-agent": "yier-bubu-edgeone-updater/1" };
  const range = request.headers.get("range");
  if (range) headers.range = range;
  const upstream = await fetch(upstreamUrl(asset), { headers, redirect: "follow" });
  if (!upstream.ok && upstream.status !== 206) {
    return new Response("Update asset unavailable", { status: 502 });
  }
  const responseHeaders = new Headers();
  for (const name of ["content-length", "content-range", "content-type", "accept-ranges", "etag"]) {
    const value = upstream.headers.get(name);
    if (value) responseHeaders.set(name, value);
  }
  responseHeaders.set("cache-control", "public, max-age=300");
  responseHeaders.set("x-content-type-options", "nosniff");
  return new Response(upstream.body, { status: upstream.status, headers: responseHeaders });
}

export const PROGRAM_ASSET_PATTERN = PROGRAM_ASSET;
export const CHARACTER_ASSET_PATTERN = CHARACTER_ASSET;
