#!/usr/bin/env node

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const project = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = join(project, "dist", ".vite", "manifest.json");
if (!existsSync(manifestPath)) throw new Error("缺少 Vite manifest，无法校验桌宠素材是否已打包");

const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const roles = ["一二", "布布"];
const requiredActions = new Set(["click", "dance", "walk"]);
const expectedKeys = new Set();

function actionFromFilename(filename) {
  const stem = filename.replace(/\.gif$/i, "");
  const raw = stem.split("（")[0].replace(/\d+$/, "");
  return raw === "idel" ? "idle" : raw;
}

function findNestedGifs(directory) {
  const nested = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const child = join(directory, entry.name);
    for (const childEntry of readdirSync(child, { recursive: true, withFileTypes: true })) {
      if (childEntry.isFile() && childEntry.name.toLowerCase().endsWith(".gif")) nested.push(childEntry.name);
    }
  }
  return nested;
}

for (const role of roles) {
  const roleDirectory = join(project, "assets", "characters", role);
  const files = readdirSync(roleDirectory, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith(".gif"))
    .map((entry) => entry.name)
    .sort();
  if (findNestedGifs(roleDirectory).length) {
    throw new Error(`${role} 素材请直接放在角色文件夹，不要放入子文件夹`);
  }
  if (!files.length) throw new Error(`${role} 没有可打包的 GIF 素材`);

  const actions = new Set(files.map(actionFromFilename));
  for (const required of requiredActions) {
    if (!actions.has(required)) throw new Error(`${role} 缺少 ${required} 动作`);
  }

  for (const filename of files) {
    const sourceKey = ["assets", "characters", role, filename].join("/");
    expectedKeys.add(sourceKey);
    const entry = manifest[sourceKey];
    if (!entry?.file) throw new Error(`素材未进入生产包：${sourceKey}`);
    const outputPath = join(project, "dist", entry.file);
    if (!existsSync(outputPath) || !statSync(outputPath).isFile() || statSync(outputPath).size === 0) {
      throw new Error(`素材产物缺失或为空：${relative(project, outputPath).split(sep).join("/")}`);
    }
  }
  console.log(`${role}：${files.length} 张 GIF，${actions.size} 类动作，已全部进入生产包`);
}

const bundledKeys = Object.keys(manifest)
  .filter((key) => key.startsWith("assets/characters/") && key.toLowerCase().endsWith(".gif"));
const unexpected = bundledKeys.filter((key) => !expectedKeys.has(key));
if (bundledKeys.length !== expectedKeys.size || unexpected.length) {
  throw new Error(`源素材与生产包数量不一致：源文件 ${expectedKeys.size}，生产包 ${bundledKeys.length}`);
}

console.log(`素材完整性校验通过：共 ${expectedKeys.size} 张 GIF`);
