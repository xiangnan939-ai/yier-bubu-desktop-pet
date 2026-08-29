#!/usr/bin/env node

import { readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  buildActionWeights,
  deriveMacroState,
  walkChanceFor,
} from "../src/animation/weights.ts";
import { planWalk } from "../src/animation/walkPlanner.ts";

const project = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const macroStates = ["sleeping", "drowsy", "idle", "active", "excited", "annoyed", "sad"];
const selectableFixedActions = ["happy", "angry", "dance", "eat", "drink", "sleep", "work"];

function actionFromFilename(filename) {
  const raw = filename.replace(/\.gif$/i, "").split("（")[0].replace(/\d+$/, "");
  return raw === "idel" ? "idle" : raw;
}

function buildLibrary(role) {
  const library = new Map();
  const directory = join(project, "assets", "characters", role);
  for (const filename of readdirSync(directory).filter((name) => name.toLowerCase().endsWith(".gif"))) {
    const action = actionFromFilename(filename);
    const assets = library.get(action) ?? [];
    assets.push({ url: filename, sourcePath: join(directory, filename) });
    library.set(action, assets);
  }
  return library;
}

const baseContext = {
  hourOfDay: 12,
  idleTimeMs: 2 * 60_000,
  batteryLevel: 80,
  charging: false,
  hot: false,
  audioPlaying: false,
  screenSharing: false,
  viewingRemote: false,
  recentClickCount: 0,
  partnerOnline: null,
  partnerOfflineMs: 0,
};

for (const role of ["一二", "布布"]) {
  const library = buildLibrary(role);
  const missingFixedActions = selectableFixedActions.filter((action) => !library.get(action)?.length);
  if (missingFixedActions.length) {
    throw new Error(`${role}：桌宠状态菜单缺少动作素材：${missingFixedActions.join("、")}`);
  }
  if (!library.get("hugging")?.length) {
    throw new Error(`${role}：文件中转站缺少 hugging 动作素材`);
  }
  const expectedAmbientActions = [...library.keys()]
    .filter((action) => action !== "walk" && action !== "hugging")
    .sort();
  for (const macroState of macroStates) {
    const weights = buildActionWeights(library, macroState, baseContext, "idle");
    const actualActions = weights.map(({ action }) => action).sort();
    if (JSON.stringify(actualActions) !== JSON.stringify(expectedAmbientActions)) {
      throw new Error(`${role}/${macroState}：非 walk 动作没有全部进入随机池`);
    }
    if (weights.some(({ action, weight }) => action === "walk" || action === "hugging" || !(weight > 0))) {
      throw new Error(`${role}/${macroState}：walk/hugging 进入了随机池，或存在零概率动作`);
    }
  }
  console.log(`${role}：${expectedAmbientActions.length} 类非 walk/hugging 动作在全部宏观状态下均保留非零概率`);
  console.log(`${role}：7 种固定状态均有对应动作素材`);
  console.log(`${role}：文件中转站 hugging 专属动作存在`);
}

if (walkChanceFor("sleeping", 0.24) !== 0 || walkChanceFor("active", 0.24) <= 0) {
  throw new Error("走路概率的宏观状态约束无效");
}
if (deriveMacroState({ ...baseContext, audioPlaying: true }) !== "excited") {
  throw new Error("声音状态没有导出 excited");
}
if (deriveMacroState({ ...baseContext, hourOfDay: 1, idleTimeMs: 11 * 60_000 }) !== "sleeping") {
  throw new Error("深夜长时间无操作没有导出 sleeping");
}

const plan = planWalk({
  x: 500,
  y: 300,
  width: 160,
  height: 160,
  workAreaX: 0,
  workAreaY: 0,
  workAreaWidth: 1_920,
  workAreaHeight: 1_080,
  scaleFactor: 1,
}, "idle", () => true, () => 0.5);
if (!plan || plan.targetX === 500 || plan.arrivalAction === "walk") {
  throw new Error("走路规划没有生成真实位移，或到达动作错误地使用了 walk");
}

console.log("动画策略不变量校验通过");
