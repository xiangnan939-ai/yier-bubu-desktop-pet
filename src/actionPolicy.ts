export type AmbientAction = "idle" | "look" | "sit" | "walk" | "eat" | "drink";

export type ActionContext = {
  musicPlaying: boolean;
  screenSharing: boolean;
  lowBattery: boolean;
  charging: boolean;
  hot: boolean;
  sleeping: boolean;
  drinkDue: boolean;
  eatDue: boolean;
  workDue: boolean;
};

export type AmbientWeights = {
  walk: number;
  look: number;
  sit: number;
  idle: number;
};

export const DEFAULT_AMBIENT_WEIGHTS: AmbientWeights = {
  walk: 24,
  look: 25,
  sit: 18,
  idle: 33,
};

/**
 * 动作只在有对应语义时出现：
 * - dance：系统正在持续播放声音；
 * - shared：对方正在查看本机桌面；
 * - sleep：系统连续 10 分钟没有键鼠输入；
 * - low_battery/charging：真实电池状态；
 * - hot：系统报告严重热状态或持续高负载；
 * - work：检测到一段持续键鼠操作后低频出现；
 * - drink/eat：到达低频提醒时间；
 * - walk：这一次确实会同步移动窗口；
 * - idle/look/sit：普通闲置。
 * 用户点击、拖动和“看看 TA”属于更高优先级的直接操作，由 Pet 单独处理。
 */
export function chooseAmbientAction(
  context: ActionContext,
  random = Math.random(),
  weights: AmbientWeights = DEFAULT_AMBIENT_WEIGHTS,
): string {
  if (context.musicPlaying) return "dance";
  if (context.screenSharing) return "shared";
  if (context.lowBattery) return "low_battery";
  if (context.charging) return "charging";
  if (context.hot) return "hot";
  if (context.sleeping) return "sleep";
  if (context.drinkDue) return "drink";
  if (context.eatDue) return "eat";
  if (context.workDue) return "work";

  const total = Math.max(1, weights.walk + weights.look + weights.sit + weights.idle);
  const value = random * total;
  if (value < weights.walk) return "walk";
  if (value < weights.walk + weights.look) return "look";
  if (value < weights.walk + weights.look + weights.sit) return "sit";
  return "idle";
}
