import type { MacroState, WalkPlan, WindowMetrics } from "./types";

const ARRIVAL_ACTIONS: Record<MacroState, string[]> = {
  sleeping: [],
  drowsy: ["sit", "sleep", "idle"],
  idle: ["sit", "idle", "look", "drink"],
  active: ["work", "look", "sit", "happy"],
  excited: ["happy", "dance", "look", "idle"],
  annoyed: ["angry", "sit", "idle"],
  sad: ["sit", "sad", "idle"],
};

const DISTANCES: Record<MacroState, { min: number; max: number; runChance: number }> = {
  sleeping: { min: 0, max: 0, runChance: 0 },
  drowsy: { min: 45, max: 95, runChance: 0 },
  idle: { min: 65, max: 150, runChance: 0.05 },
  active: { min: 110, max: 220, runChance: 0.2 },
  excited: { min: 100, max: 240, runChance: 0.45 },
  annoyed: { min: 55, max: 120, runChance: 0.25 },
  sad: { min: 40, max: 85, runChance: 0 },
};

export function planWalk(
  metrics: WindowMetrics,
  macroState: MacroState,
  hasAction: (action: string) => boolean,
  random = Math.random,
): WalkPlan | null {
  if (macroState === "sleeping") return null;
  const profile = DISTANCES[macroState];
  const minX = metrics.workAreaX;
  const maxX = metrics.workAreaX + metrics.workAreaWidth - metrics.width;
  const minY = metrics.workAreaY;
  const maxY = metrics.workAreaY + metrics.workAreaHeight - metrics.height;
  const minimumDistance = profile.min * metrics.scaleFactor;
  const canMoveRight = maxX - metrics.x >= minimumDistance * 0.7;
  const canMoveLeft = metrics.x - minX >= minimumDistance * 0.7;
  if (!canMoveRight && !canMoveLeft) return null;

  const direction: -1 | 1 = canMoveRight && canMoveLeft
    ? random() < 0.5 ? -1 : 1
    : canMoveRight ? 1 : -1;
  const distance = (profile.min + random() * (profile.max - profile.min)) * metrics.scaleFactor;
  const targetX = Math.min(maxX, Math.max(minX, metrics.x + direction * distance));
  const targetY = Math.min(maxY, Math.max(
    minY,
    metrics.y + (random() - 0.5) * 40 * metrics.scaleFactor,
  ));
  const actualDistance = Math.hypot(targetX - metrics.x, targetY - metrics.y);
  if (actualDistance < 8 * metrics.scaleFactor) return null;

  const speed = random() < profile.runChance ? "run" : "walk";
  const logicalSpeed = speed === "run" ? 75 : 38;
  const durationMs = Math.min(8_000, Math.max(
    1_500,
    actualDistance / metrics.scaleFactor / logicalSpeed * 1_000,
  ));
  const arrivals = ARRIVAL_ACTIONS[macroState].filter(hasAction);
  const arrivalAction = arrivals.length
    ? arrivals[Math.min(arrivals.length - 1, Math.floor(random() * arrivals.length))]
    : null;
  return { targetX, targetY, direction, speed, durationMs, arrivalAction };
}

export function shouldMirrorForWalk(direction: -1 | 1, sourceFacesLeft: boolean) {
  return sourceFacesLeft !== (direction < 0);
}
