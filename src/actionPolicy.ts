export const DEFAULT_WALK_CHANCE = 0.24;

/**
 * walk 是一次真实的移动决策，不参与普通动作随机。
 */
export function shouldStartWalk(
  random = Math.random(),
  chance = DEFAULT_WALK_CHANCE,
): boolean {
  return random < Math.min(1, Math.max(0, chance));
}
