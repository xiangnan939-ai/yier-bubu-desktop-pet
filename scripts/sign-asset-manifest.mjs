#!/usr/bin/env node
import { readFile, writeFile } from "node:fs/promises";
import { createPrivateKey, createPublicKey, sign } from "node:crypto";

const [input, output = "asset-manifest.json"] = process.argv.slice(2);
if (!input) {
  throw new Error("用法：node scripts/sign-asset-manifest.mjs <unsigned.json> [output.json]");
}
const privateKeyPem = process.env.YIER_BUBU_ASSET_PRIVATE_KEY;
if (!privateKeyPem) {
  throw new Error("缺少 YIER_BUBU_ASSET_PRIVATE_KEY（Ed25519 PEM 私钥）");
}

const manifest = JSON.parse(await readFile(input, "utf8"));
const payload = [
  manifest.schemaVersion,
  manifest.version,
  manifest.minAppVersion,
  manifest.packUrl,
  String(manifest.sha256).toLowerCase(),
].join("\n");
const privateKey = createPrivateKey(privateKeyPem);
if (privateKey.asymmetricKeyType !== "ed25519") throw new Error("素材私钥必须是 Ed25519");
manifest.signature = sign(null, Buffer.from(payload), privateKey).toString("base64");
await writeFile(output, `${JSON.stringify(manifest, null, 2)}\n`);

// Ed25519 SubjectPublicKeyInfo DER 的最后 32 字节是原始公钥，供 Rust 客户端验证。
const spki = createPublicKey(privateKey).export({ type: "spki", format: "der" });
const publicKey = spki.subarray(spki.length - 32).toString("base64");
await writeFile(`${output}.public-key.txt`, `${publicKey}\n`);
console.log(output);
console.log(`素材更新公钥：${publicKey}`);
