import { getGifDuration, type PetAsset, type PetRole } from "../petAssets";
import { getActionConfig } from "./actionConfigs";
import { ContextSensors } from "./contextSensors";
import { planWalk, shouldMirrorForWalk } from "./walkPlanner";
import {
  DEFAULT_WALK_CHANCE,
  MACRO_WAIT_MS,
  chooseWeightedAction,
  deriveMacroState,
  walkChanceFor,
} from "./weights";
import type {
  AnimationMode,
  AnimationSnapshot,
  ExternalAnimationState,
  FixedPetAction,
  MacroState,
  PetAnimationEvent,
  PetContext,
  SensorContext,
  WindowMetrics,
} from "./types";

type StateMachineOptions = {
  role: PetRole;
  library: Map<string, PetAsset[]>;
  onState: (state: AnimationSnapshot) => void;
  getWindowMetrics: () => Promise<WindowMetrics>;
  moveWindow: (x: number, y: number) => Promise<void>;
  walkChance?: number;
  random?: () => number;
  fixedAction?: FixedPetAction | null;
};

type DisplayResult = { action: string; durationMs: number };

const ACTION_FALLBACKS: Record<string, string[]> = {
  click: ["idle"],
  dance: ["happy", "idle"],
  drag: ["click", "idle"],
  drop: ["idle"],
  hugging: ["idle"],
  shared: ["watching", "idle"],
  watching: ["look", "idle"],
  walk: ["idle"],
};

function wait(milliseconds: number) {
  return new Promise<void>((resolve) => window.setTimeout(resolve, milliseconds));
}

export class PetAnimationStateMachine {
  private library: Map<string, PetAsset[]>;
  private readonly role: PetRole;
  private readonly onState: (state: AnimationSnapshot) => void;
  private readonly getWindowMetrics: () => Promise<WindowMetrics>;
  private readonly moveWindow: (x: number, y: number) => Promise<void>;
  private readonly random: () => number;
  private readonly sensors: ContextSensors;
  private readonly sourceFacesLeft: boolean;

  private running = false;
  private operationVersion = 0;
  private displayVersion = 0;
  private mode: AnimationMode = "ambient";
  private macroState: MacroState = "idle";
  private currentAction = "idle";
  private previousAction: string | null = null;
  private currentAssetUrl = "";
  private currentAssetKey = 0;
  private mirrored = false;
  private configuredWalkChance: number;
  private dragging = false;
  private holdingFile = false;
  private fixedAction: FixedPetAction | null;
  private partnerOfflineSince: number | null = null;
  private recentClicks: number[] = [];
  private context: PetContext = {
    hourOfDay: new Date().getHours(),
    idleTimeMs: 0,
    batteryLevel: null,
    charging: false,
    hot: false,
    audioPlaying: false,
    screenSharing: false,
    viewingRemote: false,
    recentClickCount: 0,
    partnerOnline: null,
    partnerOfflineMs: 0,
  };

  constructor(options: StateMachineOptions) {
    this.role = options.role;
    this.library = options.library;
    this.onState = options.onState;
    this.getWindowMetrics = options.getWindowMetrics;
    this.moveWindow = options.moveWindow;
    this.random = options.random ?? Math.random;
    this.configuredWalkChance = options.walkChance ?? DEFAULT_WALK_CHANCE;
    this.fixedAction = options.fixedAction ?? null;
    this.sourceFacesLeft = this.role === "bubu";
    this.sensors = new ContextSensors({
      onAudioChange: (playing) => this.updateAudio(playing),
      onContext: (context) => this.updateSensorContext(context),
    });
  }

  start() {
    if (this.running) return;
    this.running = true;
    this.sensors.start();
    this.resumeBaseAnimation("idle");
  }

  stop() {
    if (!this.running) return;
    this.running = false;
    this.operationVersion += 1;
    this.displayVersion += 1;
    this.sensors.stop();
  }

  updateLibrary(library: Map<string, PetAsset[]>) {
    // The current <img> keeps its already-decoded URL. New assets become active
    // at the next natural action boundary, so a hot update never cuts a GIF short.
    this.library = library;
  }

  updateWalkChance(chance: number) {
    this.configuredWalkChance = Math.min(1, Math.max(0, chance));
  }

  setFixedAction(action: FixedPetAction | null) {
    if (action === this.fixedAction) return;
    this.fixedAction = action;
    if (!this.running || this.dragging || this.holdingFile || this.mode === "interaction") return;
    this.resumeBaseAnimation();
  }

  updateExternalState(state: ExternalAnimationState) {
    if (state.screenSharing !== undefined) this.context.screenSharing = state.screenSharing;
    if (state.viewingRemote !== undefined) this.context.viewingRemote = state.viewingRemote;
    if (state.partnerOnline !== undefined && state.partnerOnline !== this.context.partnerOnline) {
      this.context.partnerOnline = state.partnerOnline;
      if (state.partnerOnline === false) this.partnerOfflineSince = Date.now();
      else this.partnerOfflineSince = null;
    }
    if (state.holdingFile !== undefined && state.holdingFile !== this.holdingFile) {
      this.holdingFile = state.holdingFile;
      if (this.running && !this.dragging) this.resumeBaseAnimation();
    }
    this.refreshMacroState();
  }

  dispatch(event: PetAnimationEvent) {
    if (!this.running) return;
    if (this.holdingFile) return;
    if (event.type === "click") {
      if (this.dragging) return;
      this.recentClicks.push(Date.now());
      this.refreshMacroState();
      this.beginInteraction("click");
      return;
    }
    if (event.type === "drag_start") {
      this.dragging = true;
      const operation = ++this.operationVersion;
      this.mode = "dragging";
      void this.displayAction("drag", false, operation);
      return;
    }
    if (!this.dragging) return;
    this.dragging = false;
    this.beginInteraction("drop");
  }

  private snapshot(): AnimationSnapshot {
    return {
      action: this.currentAction,
      assetUrl: this.currentAssetUrl,
      assetKey: this.currentAssetKey,
      mirrored: this.mirrored,
      macroState: this.macroState,
      mode: this.mode,
    };
  }

  private emit() {
    this.onState(this.snapshot());
  }

  private isCurrent(operation: number) {
    return this.running && operation === this.operationVersion;
  }

  private async waitFor(milliseconds: number, operation: number) {
    await wait(Math.max(0, milliseconds));
    return this.isCurrent(operation);
  }

  private updateSensorContext(sensor: SensorContext) {
    this.context = { ...this.context, ...sensor };
    this.refreshMacroState();
  }

  private refreshMacroState() {
    const now = Date.now();
    this.recentClicks = this.recentClicks.filter((time) => now - time < 60_000);
    this.context.recentClickCount = this.recentClicks.length;
    this.context.partnerOfflineMs = this.partnerOfflineSince === null
      ? 0
      : now - this.partnerOfflineSince;
    const next = deriveMacroState(this.context);
    if (next === this.macroState) return;
    this.macroState = next;
    this.emit();
  }

  private updateAudio(playing: boolean) {
    if (playing === this.context.audioPlaying) return;
    this.context.audioPlaying = playing;
    this.refreshMacroState();
    if (this.fixedAction) {
      if (this.mode === "music" || this.mode === "ambient" || this.mode === "walking") {
        this.beginFixed();
      }
    } else if (playing) {
      if (this.mode !== "interaction" && this.mode !== "dragging") this.beginMusic();
    } else if (this.mode === "music") {
      // Music is the one animation source that follows a live signal. Once
      // silence is confirmed, invalidate the pending GIF-duration wait and
      // return immediately instead of appearing to dance long after playback.
      this.beginAmbient();
    }
  }

  private beginInteraction(action: "click" | "drop") {
    const operation = ++this.operationVersion;
    this.mode = "interaction";
    void this.runInteraction(action, operation);
  }

  private async runInteraction(action: string, operation: number) {
    const result = await this.displayAction(action, false, operation);
    if (!result || !await this.waitFor(result.durationMs, operation)) return;
    this.resumeBaseAnimation();
  }

  private resumeBaseAnimation(preferredAction?: string) {
    if (this.holdingFile) this.beginHolding();
    else if (this.fixedAction) this.beginFixed();
    else if (this.context.audioPlaying) this.beginMusic();
    else this.beginAmbient(preferredAction);
  }

  private beginHolding() {
    if (!this.running || this.dragging || !this.holdingFile) return;
    const operation = ++this.operationVersion;
    this.mode = "holding";
    void this.runHolding(operation);
  }

  private async runHolding(operation: number) {
    while (this.isCurrent(operation) && this.holdingFile && !this.dragging) {
      const result = await this.displayAction("hugging", false, operation);
      if (!result || !await this.waitFor(result.durationMs, operation)) return;
      this.previousAction = result.action;
    }
    if (this.isCurrent(operation) && !this.dragging) this.resumeBaseAnimation();
  }

  private beginFixed() {
    if (!this.running || this.dragging || this.holdingFile || !this.fixedAction) return;
    const operation = ++this.operationVersion;
    this.mode = "fixed";
    void this.runFixed(operation);
  }

  private async runFixed(operation: number) {
    while (this.isCurrent(operation) && this.fixedAction && !this.dragging && !this.holdingFile) {
      const result = await this.displayAction(this.fixedAction, false, operation);
      if (!result || !await this.waitFor(result.durationMs, operation)) return;
      this.previousAction = result.action;
    }
    if (this.isCurrent(operation) && !this.dragging) this.resumeBaseAnimation();
  }

  private beginMusic() {
    if (!this.running || this.dragging || this.holdingFile) return;
    if (this.mode === "music") return;
    const operation = ++this.operationVersion;
    this.mode = "music";
    void this.runMusic(operation);
  }

  private async runMusic(operation: number) {
    while (this.isCurrent(operation) && this.context.audioPlaying && !this.dragging && !this.holdingFile) {
      const result = await this.displayAction("dance", false, operation);
      if (!result || !await this.waitFor(result.durationMs, operation)) return;
    }
    if (this.isCurrent(operation) && !this.dragging) this.resumeBaseAnimation();
  }

  private beginAmbient(preferredAction?: string) {
    if (!this.running || this.dragging || this.holdingFile) return;
    const operation = ++this.operationVersion;
    this.mode = "ambient";
    void this.runAmbient(operation, preferredAction);
  }

  private async runAmbient(operation: number, initialAction?: string) {
    let preferredAction = initialAction;
    while (this.isCurrent(operation) && !this.context.audioPlaying && !this.dragging && !this.holdingFile) {
      this.refreshMacroState();
      const walkChance = walkChanceFor(this.macroState, this.configuredWalkChance);
      if (!preferredAction && this.library.has("walk") && this.random() < walkChance) {
        const arrival = await this.performWalk(operation);
        if (!this.isCurrent(operation)) return;
        preferredAction = arrival;
      }

      const requested = preferredAction ?? chooseWeightedAction(
        this.library,
        this.macroState,
        this.context,
        this.previousAction,
        this.currentAction,
        this.random,
      );
      preferredAction = undefined;
      if (!requested) {
        await this.waitFor(500, operation);
        continue;
      }

      const result = await this.displayAction(requested, false, operation);
      if (!result || !await this.waitFor(result.durationMs, operation)) return;
      this.previousAction = result.action;
      const timing = MACRO_WAIT_MS[this.macroState];
      const pause = timing.base + this.random() * timing.random;
      if (!await this.waitFor(pause, operation)) return;
    }
    if (this.isCurrent(operation) && !this.dragging) this.resumeBaseAnimation();
  }

  private async performWalk(operation: number): Promise<string | undefined> {
    try {
      const metrics = await this.getWindowMetrics();
      if (!this.isCurrent(operation) || this.context.audioPlaying || this.dragging || this.holdingFile) return undefined;
      const plan = planWalk(
        metrics,
        this.macroState,
        (action) => this.library.has(action),
        this.random,
      );
      if (!plan) return undefined;

      this.mode = "walking";
      const mirrored = shouldMirrorForWalk(plan.direction, this.sourceFacesLeft);
      void this.displayAction("walk", mirrored, operation);
      const startedAt = performance.now();
      while (this.isCurrent(operation) && !this.context.audioPlaying && !this.dragging && !this.holdingFile) {
        const progress = Math.min(1, (performance.now() - startedAt) / plan.durationMs);
        await this.moveWindow(
          Math.round(metrics.x + (plan.targetX - metrics.x) * progress),
          Math.round(metrics.y + (plan.targetY - metrics.y) * progress),
        );
        if (progress >= 1) break;
        if (!await this.waitFor(33, operation)) return undefined;
      }
      if (!this.isCurrent(operation) || this.context.audioPlaying || this.dragging || this.holdingFile) return undefined;
      this.previousAction = "walk";
      this.mode = "ambient";
      return plan.arrivalAction ?? undefined;
    } catch {
      if (this.isCurrent(operation)) this.mode = "ambient";
      return undefined;
    }
  }

  private resolveAction(requested: string) {
    if ((this.library.get(requested)?.length ?? 0) > 0) return requested;
    for (const fallback of ACTION_FALLBACKS[requested] ?? ["idle"]) {
      if ((this.library.get(fallback)?.length ?? 0) > 0) return fallback;
    }
    return [...this.library.entries()].find(([action, assets]) => action !== "walk" && assets.length)?.[0]
      ?? null;
  }

  private async displayAction(
    requested: string,
    mirrored: boolean,
    operation: number,
  ): Promise<DisplayResult | null> {
    if (!this.isCurrent(operation)) return null;
    const action = this.resolveAction(requested);
    if (!action) return null;
    const assets = this.library.get(action) ?? [];
    if (!assets.length) return null;
    const index = Math.min(assets.length - 1, Math.floor(this.random() * assets.length));
    const selected = assets[index];
    const display = ++this.displayVersion;
    this.currentAction = action;
    this.currentAssetUrl = selected.url;
    this.currentAssetKey += 1;
    this.mirrored = mirrored;
    this.emit();

    const gifDuration = await getGifDuration(selected.url);
    if (!this.isCurrent(operation) || display !== this.displayVersion) return null;
    const durationMs = Math.max(getActionConfig(action).minimumDisplayMs, gifDuration);
    return { action, durationMs };
  }
}
