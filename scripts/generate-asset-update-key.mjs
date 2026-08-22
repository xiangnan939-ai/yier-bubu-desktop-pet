#!/usr/bin/env node
import { generateKeyPairSync } from "node:crypto";
import { writeFile } from "node:fs/promises";

const privateKeyPath = process.argv[2] ?? "asset-update-private.pem";
const { privateKey, publicKey } = generateKeyPairSync("ed25519");
const privateKeyPem = privateKey.export({ type: "pkcs8", format: "pem" });
const spki = publicKey.export({ type: "spki", format: "der" });
const rawPublicKey = spki.subarray(spki.length - 32).toString("base64");

await writeFile(privateKeyPath, privateKeyPem, { mode: 0o600 });
await writeFile(`${privateKeyPath}.public-key.txt`, `${rawPublicKey}\n`, { mode: 0o600 });
console.log(`私钥已保存：${privateKeyPath}`);
console.log(`素材更新公钥：${rawPublicKey}`);
