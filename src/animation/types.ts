export type MacroState =
  | "sleeping"
  | "drowsy"
  | "idle"
  | "active"
  | "excited"
  | "annoyed"
  | "sad";

export type AnimationMode = "ambient" | "fixed" | "walking" | "music" | "interaction" | "dragging" | "holding";

export type FixedPetAction = "happy" | "angry" | "dance" | "eat" | "drink" | "sleep" | "work";

export type PetAnimationEvent =
  | { type: "click" }
  | { type: "drag_start" }
  | { type: "drag_end" };

export type SensorContext = {
  hourOfDay: number;
  idleTimeMs: number;
  batteryLevel: number | null;
  charging: boolean;
  hot: boolean;
};

export type PetContext = SensorContext & {
  audioPlaying: boolean;
  screenSharing: boolean;
  viewingRemote: boolean;
  recentClickCount: number;
  partnerOnline: boolean | null;
  partnerOfflineMs: number;
};

export type AnimationSnapshot = {
  action: string;
  assetUrl: string;
  assetKey: number;
  mirrored: boolean;
  macroState: MacroState;
  mode: AnimationMode;
};

export type WindowMetrics = {
  x: number;
  y: number;
  width: number;
  height: number;
  workAreaX: number;
  workAreaY: number;
  workAreaWidth: number;
  workAreaHeight: number;
  scaleFactor: number;
};

export type WalkPlan = {
  targetX: number;
  targetY: number;
  direction: -1 | 1;
  speed: "walk" | "run";
  durationMs: number;
  arrivalAction: string | null;
};

export type ExternalAnimationState = {
  screenSharing?: boolean;
  viewingRemote?: boolean;
  partnerOnline?: boolean | null;
  holdingFile?: boolean;
};
