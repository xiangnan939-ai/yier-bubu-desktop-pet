#!/usr/bin/env node
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

const [version, signaturesDir, baseUrl, output = "latest.json"] = process.argv.slice(2);
if (!version || !signaturesDir || !baseUrl) {
  throw new Error(
    "用法：node scripts/make-updater-json.mjs <version> <signatures-dir> <base-url> [output]",
  );
}

const macArchive = `yier-bubu_${version}_aarch64.app.tar.gz`;
const windowsArchive = `yier-bubu_${version}_x64-setup.exe`;
const readSignature = async (archive) => {
  const signature = (await readFile(join(signaturesDir, `${archive}.sig`), "utf8")).trim();
  if (!signature) throw new Error(`${archive}.sig 为空`);
  return signature;
};

const mac = {
  signature: await readSignature(macArchive),
  url: `${baseUrl.replace(/\/$/, "")}/${macArchive}`,
};
const windows = {
  signature: await readSignature(windowsArchive),
  url: `${baseUrl.replace(/\/$/, "")}/${windowsArchive}`,
};

const manifest = {
  version,
  notes: `一二布布私人桌宠 ${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    "darwin-aarch64": mac,
    "darwin-aarch64-app": mac,
    "windows-x86_64": windows,
    "windows-x86_64-nsis": windows,
  },
};

await writeFile(output, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(output);
