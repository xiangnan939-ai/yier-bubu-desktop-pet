const FALLBACK_DURATION_MS = 2500;
const durationCache = new Map<string, Promise<number>>();

function skipSubBlocks(bytes: Uint8Array, start: number) {
  let offset = start;
  while (offset < bytes.length) {
    const size = bytes[offset];
    offset += 1;
    if (size === 0) break;
    offset += size;
  }
  return offset;
}

export function parseGifDuration(bytes: Uint8Array) {
  if (bytes.length < 14 || String.fromCharCode(...bytes.slice(0, 3)) !== "GIF") {
    return FALLBACK_DURATION_MS;
  }

  let offset = 13;
  if ((bytes[10] & 0x80) !== 0) offset += 3 * 2 ** ((bytes[10] & 0x07) + 1);

  let duration = 0;
  while (offset < bytes.length) {
    const marker = bytes[offset++];
    if (marker === 0x3b) break;

    if (marker === 0x21) {
      const label = bytes[offset++];
      if (label === 0xf9 && bytes[offset] === 4) {
        const delayHundredths = bytes[offset + 2] | (bytes[offset + 3] << 8);
        // WebKit displays frames of 10 ms or less for 100 ms. Matching that
        // clamp prevents macOS from changing actions before the GIF is done.
        duration += delayHundredths <= 1 ? 100 : delayHundredths * 10;
      }
      offset = skipSubBlocks(bytes, offset);
      continue;
    }

    if (marker === 0x2c) {
      if (offset + 9 > bytes.length) break;
      const packed = bytes[offset + 8];
      offset += 9;
      if ((packed & 0x80) !== 0) offset += 3 * 2 ** ((packed & 0x07) + 1);
      offset += 1;
      offset = skipSubBlocks(bytes, offset);
      continue;
    }
    break;
  }
  return duration > 0 ? duration : FALLBACK_DURATION_MS;
}

export function getGifDuration(url: string) {
  const cached = durationCache.get(url);
  if (cached) return cached;

  const pending = fetch(url)
    .then((response) => {
      if (!response.ok) throw new Error(`GIF request failed: ${response.status}`);
      return response.arrayBuffer();
    })
    .then((buffer) => parseGifDuration(new Uint8Array(buffer)))
    .catch(() => FALLBACK_DURATION_MS);
  durationCache.set(url, pending);
  return pending;
}
