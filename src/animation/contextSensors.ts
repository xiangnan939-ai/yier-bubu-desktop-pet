import { invoke } from "@tauri-apps/api/core";
import type { SensorContext } from "./types";

type DeviceStatus = {
  batteryPercentage: number | null;
  charging: boolean;
  hot: boolean;
};

type SensorCallbacks = {
  onAudioChange: (playing: boolean) => void;
  onContext: (context: SensorContext) => void;
};

export class ContextSensors {
  private running = false;
  private audioTimer: number | null = null;
  private contextTimer: number | null = null;
  private audioCheckRunning = false;
  private contextCheckRunning = false;
  private audioTrueSamples = 0;
  private audioFalseSamples = 0;
  private audioPlaying = false;
  private hotSamples = 0;
  private coolSamples = 0;
  private confirmedHot = false;

  constructor(private readonly callbacks: SensorCallbacks) {}

  start() {
    if (this.running) return;
    this.running = true;
    this.pollAudio();
    this.pollContext();
    this.audioTimer = window.setInterval(() => this.pollAudio(), 900);
    this.contextTimer = window.setInterval(() => this.pollContext(), 5_000);
  }

  stop() {
    this.running = false;
    if (this.audioTimer !== null) window.clearInterval(this.audioTimer);
    if (this.contextTimer !== null) window.clearInterval(this.contextTimer);
    this.audioTimer = null;
    this.contextTimer = null;
  }

  private async pollAudio() {
    if (!this.running || this.audioCheckRunning) return;
    this.audioCheckRunning = true;
    try {
      const playing = await invoke<boolean>("system_audio_playing").catch(() => false);
      if (!this.running) return;
      if (playing) {
        this.audioTrueSamples += 1;
        this.audioFalseSamples = 0;
      } else {
        this.audioFalseSamples += 1;
        this.audioTrueSamples = 0;
      }
      const confirmed = this.audioPlaying
        ? this.audioFalseSamples < 3
        : this.audioTrueSamples >= 2;
      if (confirmed !== this.audioPlaying) {
        this.audioPlaying = confirmed;
        this.callbacks.onAudioChange(confirmed);
      }
    } finally {
      this.audioCheckRunning = false;
    }
  }

  private async pollContext() {
    if (!this.running || this.contextCheckRunning) return;
    this.contextCheckRunning = true;
    try {
      const [idleSeconds, status] = await Promise.all([
        invoke<number>("system_idle_seconds").catch(() => 0),
        invoke<DeviceStatus>("device_status").catch(() => ({
          batteryPercentage: null,
          charging: false,
          hot: false,
        })),
      ]);
      if (!this.running) return;
      if (status.hot) {
        this.hotSamples += 1;
        this.coolSamples = 0;
        if (this.hotSamples >= 6) this.confirmedHot = true;
      } else {
        this.coolSamples += 1;
        this.hotSamples = 0;
        if (this.coolSamples >= 3) this.confirmedHot = false;
      }
      this.callbacks.onContext({
        hourOfDay: new Date().getHours(),
        idleTimeMs: Math.max(0, idleSeconds) * 1_000,
        batteryLevel: status.batteryPercentage,
        charging: status.charging,
        hot: this.confirmedHot,
      });
    } finally {
      this.contextCheckRunning = false;
    }
  }
}
