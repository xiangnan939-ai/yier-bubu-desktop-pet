export type ActionConfig = {
  minimumDisplayMs: number;
};

const DEFAULT_ACTION_CONFIG: ActionConfig = { minimumDisplayMs: 900 };

const ACTION_CONFIGS: Record<string, ActionConfig> = {
  idle: { minimumDisplayMs: 1_200 },
  sit: { minimumDisplayMs: 1_500 },
  look: { minimumDisplayMs: 1_000 },
  walk: { minimumDisplayMs: 800 },
  sleep: { minimumDisplayMs: 3_000 },
  wake: { minimumDisplayMs: 1_200 },
  work: { minimumDisplayMs: 2_000 },
  eat: { minimumDisplayMs: 1_800 },
  drink: { minimumDisplayMs: 1_500 },
  happy: { minimumDisplayMs: 1_200 },
  sad: { minimumDisplayMs: 1_800 },
  angry: { minimumDisplayMs: 1_200 },
  dance: { minimumDisplayMs: 1_500 },
  hot: { minimumDisplayMs: 1_500 },
  click: { minimumDisplayMs: 800 },
  drag: { minimumDisplayMs: 0 },
  drop: { minimumDisplayMs: 700 },
  watching: { minimumDisplayMs: 1_200 },
  shared: { minimumDisplayMs: 1_200 },
  charging: { minimumDisplayMs: 1_500 },
  low_battery: { minimumDisplayMs: 1_500 },
  message: { minimumDisplayMs: 1_200 },
};

export function getActionConfig(action: string): ActionConfig {
  return ACTION_CONFIGS[action] ?? DEFAULT_ACTION_CONFIG;
}
