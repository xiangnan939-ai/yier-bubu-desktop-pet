import type { PetAsset } from "../petAssets";
import type { MacroState, PetContext } from "./types";

export const DEFAULT_WALK_CHANCE = 0.24;

const BASE_WEIGHT_PER_ASSET = 0.2;

const MACRO_BOOSTS: Record<MacroState, Record<string, number>> = {
  sleeping: { sleep: 250, idle: 3 },
  drowsy: { sleep: 100, sit: 35, idle: 22, wake: 12, look: 6 },
  idle: { idle: 75, sit: 38, look: 28, drink: 10, eat: 8, work: 5, happy: 4 },
  active: { work: 58, idle: 28, look: 22, happy: 16, sit: 14, eat: 8, drink: 6 },
  excited: { dance: 95, happy: 45, idle: 10, look: 8 },
  annoyed: { angry: 90, hot: 55, sit: 24, idle: 18, look: 12 },
  sad: { sad: 110, sit: 35, idle: 20, sleep: 8 },
};

const TRANSITION_BOOSTS: Record<string, Record<string, number>> = {
  walk: { sit: 35, idle: 28, look: 20, drink: 8 },
  wake: { idle: 35, sit: 18, happy: 8 },
  eat: { sit: 24, drink: 20, happy: 10 },
  drink: { sit: 22, idle: 20, happy: 5 },
  dance: { idle: 25, sit: 18, happy: 12 },
  drop: { idle: 28, angry: 12, look: 8 },
  click: { happy: 28, idle: 20, look: 14 },
  work: { sit: 24, idle: 18, drink: 12, look: 10 },
  happy: { idle: 24, sit: 16, look: 8 },
  angry: { sit: 25, idle: 20, sad: 5 },
  sit: { idle: 22, look: 16, eat: 8, drink: 8 },
  look: { idle: 24, sit: 16, happy: 5 },
  sad: { sit: 24, idle: 20, sleep: 8 },
  low_battery: { sit: 28, sleep: 20, idle: 8 },
  charging: { idle: 25, happy: 18, sit: 8 },
  message: { happy: 30, idle: 18, look: 8 },
};

export const MACRO_WAIT_MS: Record<MacroState, { base: number; random: number }> = {
  sleeping: { base: 5_000, random: 4_000 },
  drowsy: { base: 2_500, random: 2_000 },
  idle: { base: 1_300, random: 2_200 },
  active: { base: 700, random: 1_500 },
  excited: { base: 300, random: 900 },
  annoyed: { base: 1_500, random: 1_800 },
  sad: { base: 2_500, random: 2_500 },
};

const MACRO_WALK_CHANCE: Record<MacroState, number> = {
  sleeping: 0,
  drowsy: 0.04,
  idle: 0.15,
  active: 0.30,
  excited: 0.18,
  annoyed: 0.07,
  sad: 0.06,
};

export function deriveMacroState(context: PetContext): MacroState {
  if (context.audioPlaying) return "excited";
  if (context.hot || context.recentClickCount >= 6) return "annoyed";
  if (context.batteryLevel !== null && context.batteryLevel < 20 && !context.charging) {
    return "drowsy";
  }
  if (context.partnerOnline === false && context.partnerOfflineMs > 60 * 60_000) return "sad";

  const lateNight = context.hourOfDay >= 23 || context.hourOfDay < 6;
  const earlyMorning = context.hourOfDay >= 6 && context.hourOfDay < 9;
  if (lateNight && context.idleTimeMs >= 10 * 60_000) return "sleeping";
  if ((lateNight || earlyMorning) && context.idleTimeMs >= 5 * 60_000) return "drowsy";
  if (!lateNight && context.idleTimeMs < 60_000) return "active";
  return "idle";
}

function contextualBoost(action: string, context: PetContext, hasSharedAsset: boolean) {
  let boost = 0;
  if (context.screenSharing && (action === "shared" || (!hasSharedAsset && action === "watching"))) {
    boost += 120;
  }
  if (context.viewingRemote && action === "watching") boost += 120;
  if (context.charging && context.batteryLevel !== null && context.batteryLevel < 80
    && action === "charging") boost += 70;
  if (context.batteryLevel !== null && context.batteryLevel < 20 && !context.charging
    && action === "low_battery") boost += 90;
  if (context.hot && action === "hot") boost += 100;
  return boost;
}

export function chooseWeightedAction(
  library: Map<string, PetAsset[]>,
  macroState: MacroState,
  context: PetContext,
  previousAction: string | null,
  currentAction: string,
  random = Math.random,
): string | null {
  const candidates = buildActionWeights(library, macroState, context, previousAction);
  if (!candidates.length) return null;

  const withoutImmediateRepeat = candidates.filter(({ action }) => action !== currentAction);
  const usable = withoutImmediateRepeat.length ? withoutImmediateRepeat : candidates;
  const total = usable.reduce((sum, candidate) => sum + candidate.weight, 0);
  let roll = random() * total;
  for (const candidate of usable) {
    roll -= candidate.weight;
    if (roll <= 0) return candidate.action;
  }
  return usable[usable.length - 1].action;
}

export function buildActionWeights(
  library: Map<string, PetAsset[]>,
  macroState: MacroState,
  context: PetContext,
  previousAction: string | null,
) {
  const hasSharedAsset = (library.get("shared")?.length ?? 0) > 0;
  return [...library.entries()]
    .filter(([action, assets]) => action !== "walk" && action !== "hugging" && assets.length > 0)
    .map(([action, assets]) => {
      const weight = assets.length * BASE_WEIGHT_PER_ASSET
        + (MACRO_BOOSTS[macroState][action] ?? 0)
        + (previousAction ? TRANSITION_BOOSTS[previousAction]?.[action] ?? 0 : 0)
        + contextualBoost(action, context, hasSharedAsset);
      return { action, weight };
    });
}

export function walkChanceFor(macroState: MacroState, configuredChance: number) {
  const scale = configuredChance / DEFAULT_WALK_CHANCE;
  return Math.min(0.65, Math.max(0, MACRO_WALK_CHANCE[macroState] * scale));
}
