import { invoke } from "@tauri-apps/api/core";
import type { MqttClient } from "mqtt";

let mqttModule: Promise<typeof import("mqtt")> | null = null;
function loadMqtt() {
  mqttModule ??= import("mqtt");
  return mqttModule;
}

export type SignalCore = {
  version: number;
  messageType: string;
  bindingId: string;
  senderRole: string;
  recipientRole: string;
  nonce: string;
  createdAtMs: number;
  expiresAtMs: number;
  payload: unknown;
};

export type SignedSignal = { core: SignalCore; signature: string };
export type SignalProcessResult = {
  accepted: boolean;
  event: string;
  reply: SignedSignal | null;
};

export type ViewSession = { sessionId: string; roomId: number; createdAtMs: number };
export type ViewErrorPayload = { sessionId: string; message: string };
export type ViewPeerSignal = {
  session: ViewSession;
  kind: "offer" | "answer" | "ice";
  description?: RTCSessionDescriptionInit;
  candidate?: RTCIceCandidateInit;
};
export const VIEW_SESSION_KEY = "yier-bubu-view-session";

const PEER_CONFIGURATION: RTCConfiguration = {
  iceServers: [
    { urls: ["stun:stun.cloudflare.com:3478", "stun:stun.cloudflare.com:53"] },
  ],
  iceCandidatePoolSize: 4,
};

type SignalHandler = (result: SignalProcessResult, signal: SignedSignal) => void | Promise<void>;

function reasonText(reason: unknown) {
  if (reason instanceof Error) return reason.message;
  if (typeof reason === "object" && reason && "message" in reason) return String(reason.message);
  return String(reason);
}

const SIGNAL_BROKERS = [
  "wss://broker.emqx.io:8084/mqtt",
  "wss://broker.hivemq.com:8884/mqtt",
] as const;

type RealtimeRoute = {
  bindingId: string | null;
  state: string;
};

type RealtimeProfile = {
  role: "yier" | "bubu";
};

async function sha256Hex(value: string) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function firstSuccessful(operations: Promise<void>[]) {
  return new Promise<void>((resolve, reject) => {
    let failures = 0;
    for (const operation of operations) {
      operation.then(resolve).catch(() => {
        failures += 1;
        if (failures === operations.length) reject(new Error("全部节点均连接失败"));
      });
    }
  });
}

export class RealtimeMessaging {
  private clients = new Map<string, MqttClient>();
  private connectPromise: Promise<void> | null = null;
  private seenSignalNonces = new Set<string>();
  private inboundTopic = "";
  private outboundTopic = "";
  private clientId = "";
  private ready = false;
  private handler: SignalHandler;

  constructor(handler: SignalHandler) {
    this.handler = handler;
  }

  connect(forceRefresh = false): Promise<void> {
    if (forceRefresh && this.clients.size > 0) {
      return this.close().then(() => this.connect());
    }
    if (this.ready && [...this.clients.values()].some((client) => client.connected)) {
      return Promise.resolve();
    }
    if (this.connectPromise) return this.connectPromise;
    this.connectPromise = this.connectInternal().finally(() => {
      this.connectPromise = null;
    });
    return this.connectPromise;
  }

  private async connectInternal() {
    if (!this.inboundTopic) {
      const [binding, profile] = await Promise.all([
        invoke<RealtimeRoute>("binding_status"),
        invoke<RealtimeProfile>("app_profile"),
      ]);
      if ((binding.state !== "bound" && binding.state !== "revoking") || !binding.bindingId) {
        throw new Error("双机尚未完成安全绑定");
      }
      const routeHash = await sha256Hex(`yier-bubu-mqtt-v1|${binding.bindingId}`);
      const partnerRole = profile.role === "yier" ? "bubu" : "yier";
      const base = `yier-bubu/v1/${routeHash}`;
      this.inboundTopic = `${base}/${profile.role}/+`;
      this.outboundTopic = `${base}/${partnerRole}`;
      this.clientId = `yb_${routeHash.slice(0, 12)}_${profile.role}`;
    }

    const attempts = SIGNAL_BROKERS.map((url) => this.connectBroker(url));
    try {
      await firstSuccessful(attempts);
      this.ready = true;
    } catch {
      await this.close();
      throw new Error("免费双机信令节点暂时无法连接，请检查网络后重试");
    }
  }

  private async connectBroker(url: string) {
    if (this.clients.get(url)?.connected) return;
    const mqtt = await loadMqtt();
    await new Promise<void>((resolve, reject) => {
      let settled = false;
      const client = mqtt.connect(url, {
        protocolVersion: 5,
        clean: false,
        clientId: this.clientId,
        connectTimeout: 10_000,
        reconnectPeriod: 2_500,
        keepalive: 30,
      });
      this.clients.set(url, client);
      const timeout = window.setTimeout(() => {
        if (settled) return;
        settled = true;
        client.end(true);
        this.clients.delete(url);
        reject(new Error("连接超时"));
      }, 12_000);
      const fail = (reason: Error) => {
        if (settled) return;
        settled = true;
        window.clearTimeout(timeout);
        client.end(true);
        this.clients.delete(url);
        reject(reason);
      };
      client.on("connect", () => {
        client.subscribe(this.inboundTopic, { qos: 1 }, (error) => {
          if (error) {
            fail(error);
            return;
          }
          if (!settled) {
            settled = true;
            window.clearTimeout(timeout);
            resolve();
          }
        });
      });
      client.on("message", (_topic, payload) => {
        let signal: SignedSignal;
        try {
          signal = JSON.parse(payload.toString()) as SignedSignal;
        } catch {
          return;
        }
        this.processMessages([signal]).catch(() => undefined);
      });
      client.on("error", fail);
    });
  }

  private async processMessages(signals: SignedSignal[]) {
    for (const signal of signals) {
      if (typeof signal.core?.nonce !== "string" || this.seenSignalNonces.has(signal.core.nonce)) continue;
      this.seenSignalNonces.add(signal.core.nonce);
      if (this.seenSignalNonces.size > 300) {
        const oldest = this.seenSignalNonces.values().next().value;
        if (oldest) this.seenSignalNonces.delete(oldest);
      }
      try {
        const result = await invoke<SignalProcessResult>("process_realtime_signal", { signal });
        if (result.reply) await this.send(result.reply);
        await this.handler(result, signal);
      } catch {
        // Rust rejects expired, duplicate, forged, or other-device messages.
      }
    }
  }

  private publish(client: MqttClient, topic: string, payload: string, expirySeconds: number) {
    return new Promise<void>((resolve, reject) => {
      client.publish(topic, payload, {
        qos: 1,
        retain: false,
        properties: { messageExpiryInterval: expirySeconds },
      }, (error) => error ? reject(error) : resolve());
    });
  }

  async send(signal: SignedSignal) {
    await this.connect();
    const clients = [...this.clients.values()].filter((client) => client.connected);
    if (clients.length === 0) {
      this.ready = false;
      await this.connect();
    }
    const active = [...this.clients.values()].filter((client) => client.connected);
    if (active.length === 0) throw new Error("免费双机信令节点暂时离线");
    const topic = `${this.outboundTopic}/${signal.core.nonce}`;
    const expirySeconds = Math.max(
      1,
      Math.min(7 * 24 * 60 * 60, Math.ceil((signal.core.expiresAtMs - Date.now()) / 1000)),
    );
    try {
      await firstSuccessful(active.map((client) =>
        this.publish(client, topic, JSON.stringify(signal), expirySeconds)));
    } catch {
      throw new Error("双机连接请求发送失败，请稍后重试");
    }
  }

  async close() {
    const clients = [...this.clients.values()];
    this.clients.clear();
    this.ready = false;
    await Promise.all(clients.map((client) => new Promise<void>((resolve) => {
      client.end(true, {}, () => resolve());
    })));
  }
}

function frameBytes(value: unknown): Uint8Array {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (Array.isArray(value)) return new Uint8Array(value);
  throw new Error("桌面画面数据格式无效");
}

export class ScreenPublisher {
  private peer: RTCPeerConnection | null = null;
  private canvas: HTMLCanvasElement | null = null;
  private track: MediaStreamTrack | null = null;
  private stopped = true;
  private session: ViewSession | null = null;
  private safetyTimer = 0;

  async start(
    session: ViewSession,
    offer: RTCSessionDescriptionInit,
    onSignal: (signal: ViewPeerSignal) => void | Promise<void>,
    onError?: (message: string) => void | Promise<void>,
  ) {
    if (this.session?.sessionId === session.sessionId && !this.stopped) return;
    await this.stop();
    this.stopped = false;
    this.session = session;
    try {
      await invoke("ensure_screen_capture_permission");
      await invoke("set_screen_share_active", { active: true });
      const canvas = document.createElement("canvas");
      canvas.width = 1280;
      canvas.height = 720;
      canvas.hidden = true;
      document.body.appendChild(canvas);
      this.canvas = canvas;
      const stream = canvas.captureStream(10);
      const track = stream.getVideoTracks()[0];
      if (!track) throw new Error("当前系统无法创建内置画面通道");
      this.track = track;
      const peer = new RTCPeerConnection(PEER_CONFIGURATION);
      this.peer = peer;
      let answerSent = false;
      const queuedCandidates: RTCIceCandidateInit[] = [];
      peer.addTrack(track, stream);
      peer.onicecandidate = (event) => {
        if (!event.candidate || this.stopped || !this.matches(session.sessionId)) return;
        const candidate = event.candidate.toJSON();
        if (!answerSent) {
          queuedCandidates.push(candidate);
          return;
        }
        void Promise.resolve(onSignal({ session, kind: "ice", candidate })).catch(() => undefined);
      };
      peer.onconnectionstatechange = () => {
        if (peer.connectionState === "connected") {
          window.clearTimeout(this.safetyTimer);
          this.safetyTimer = window.setTimeout(() => this.stop(), 30 * 60_000);
        } else if (peer.connectionState === "failed" && !this.stopped) {
          void this.stop().then(() => onError?.("点对点画面通道连接失败，请检查两台电脑的网络后重试"));
        }
      };
      await peer.setRemoteDescription(offer);
      const answer = await peer.createAnswer();
      await peer.setLocalDescription(answer);
      if (!peer.localDescription) throw new Error("无法生成点对点画面响应");
      await onSignal({ session, kind: "answer", description: peer.localDescription.toJSON() });
      answerSent = true;
      for (const candidate of queuedCandidates.splice(0)) {
        await onSignal({ session, kind: "ice", candidate });
      }
      this.drawFrames().catch(async (reason) => {
        const message = reasonText(reason);
        await this.stop().catch(() => undefined);
        try {
          await onError?.(message);
        } catch {
          // The local capture has already been stopped; the viewer also has a timeout fallback.
        }
      });
      window.clearTimeout(this.safetyTimer);
      this.safetyTimer = window.setTimeout(() => this.stop().catch(() => undefined), 45_000);
    } catch (reason) {
      await this.stop();
      throw new Error(`无法分享桌面：${reasonText(reason)}`);
    }
  }

  private async drawFrames() {
    while (!this.stopped && this.canvas) {
      const started = performance.now();
      const raw = await invoke<unknown>("capture_screen_frame");
      if (this.stopped || !this.canvas) return;
      const bitmap = await createImageBitmap(new Blob([frameBytes(raw)], { type: "image/jpeg" }));
      const context = this.canvas.getContext("2d", { alpha: false });
      if (context) {
        context.fillStyle = "#111";
        context.fillRect(0, 0, this.canvas.width, this.canvas.height);
        const scale = Math.min(this.canvas.width / bitmap.width, this.canvas.height / bitmap.height);
        const width = bitmap.width * scale;
        const height = bitmap.height * scale;
        context.drawImage(bitmap, (this.canvas.width - width) / 2, (this.canvas.height - height) / 2, width, height);
      }
      bitmap.close();
      await new Promise((resolve) => window.setTimeout(resolve, Math.max(0, 100 - (performance.now() - started))));
    }
  }

  matches(sessionId: unknown) {
    return typeof sessionId === "string" && this.session?.sessionId === sessionId;
  }

  async addIceCandidate(signal: ViewPeerSignal) {
    if (!this.matches(signal.session.sessionId) || !this.peer || !signal.candidate) return;
    await this.peer.addIceCandidate(signal.candidate);
  }

  async stop() {
    this.stopped = true;
    window.clearTimeout(this.safetyTimer);
    this.safetyTimer = 0;
    this.session = null;
    if (this.peer) {
      this.peer.onicecandidate = null;
      this.peer.onconnectionstatechange = null;
      this.peer.close();
      this.peer = null;
    }
    this.track?.stop();
    this.track = null;
    this.canvas?.remove();
    this.canvas = null;
    await invoke("set_screen_share_active", { active: false }).catch(() => undefined);
  }
}

export async function createViewSession(): Promise<ViewSession> {
  const random = new Uint32Array(1);
  crypto.getRandomValues(random);
  return {
    sessionId: crypto.randomUUID(),
    roomId: 1 + random[0] % 4_294_967_293,
    createdAtMs: Date.now(),
  };
}

export function loadViewSession(): ViewSession {
  const raw = localStorage.getItem(VIEW_SESSION_KEY);
  if (!raw) throw new Error("缺少本次查看会话");
  const session = JSON.parse(raw) as ViewSession;
  if (!session.sessionId || !Number.isInteger(session.roomId) || Date.now() - session.createdAtMs > 2 * 60_000) {
    throw new Error("本次查看会话已过期");
  }
  return session;
}

export async function startScreenViewer(
  target: HTMLElement,
  session: ViewSession,
  onStatus: (status: string) => void,
  onSignal: (signal: ViewPeerSignal) => void | Promise<void>,
) {
  const peer = new RTCPeerConnection(PEER_CONFIGURATION);
  let playing = false;
  let stopped = false;
  let frameRequest = 0;
  let video: HTMLVideoElement | null = null;
  let offerSent = false;
  const queuedCandidates: RTCIceCandidateInit[] = [];
  const pendingRemoteCandidates: RTCIceCandidateInit[] = [];
  const availabilityTimer = window.setTimeout(() => {
    if (!playing) onStatus("连接超时：未收到对方画面，请确认对方电脑已开机且桌宠正在运行");
  }, 25_000);
  peer.onicecandidate = (event) => {
    if (!event.candidate || stopped) return;
    const candidate = event.candidate.toJSON();
    if (!offerSent) {
      queuedCandidates.push(candidate);
      return;
    }
    void Promise.resolve(onSignal({ session, kind: "ice", candidate })).catch(() => undefined);
  };
  peer.onconnectionstatechange = () => {
    if (peer.connectionState === "failed") {
      onStatus("连接失败：当前网络无法建立点对点画面通道");
    } else if (peer.connectionState === "disconnected" && playing) {
      onStatus("连接中断，正在等待网络恢复…");
    }
  };
  peer.ontrack = async (event) => {
    if (playing || stopped || event.track.kind !== "video") return;
    playing = true;
    window.clearTimeout(availabilityTimer);
    target.replaceChildren();
    video = document.createElement("video");
    video.autoplay = true;
    video.muted = true;
    video.playsInline = true;
    video.style.display = "none";
    video.srcObject = event.streams[0] ?? new MediaStream([event.track]);
    const canvas = document.createElement("canvas");
    canvas.className = "viewer-canvas";
    target.append(video, canvas);
    await video.play();
    const draw = () => {
      if (stopped || !video) return;
      const width = Math.max(1, target.clientWidth);
      const height = Math.max(1, target.clientHeight);
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }
      const context = canvas.getContext("2d", { alpha: false });
      if (context && video.videoWidth > 0 && video.videoHeight > 0) {
        context.fillStyle = "#111";
        context.fillRect(0, 0, width, height);
        const scale = Math.min(width / video.videoWidth, height / video.videoHeight);
        const drawWidth = video.videoWidth * scale;
        const drawHeight = video.videoHeight * scale;
        context.drawImage(video, (width - drawWidth) / 2, (height - drawHeight) / 2, drawWidth, drawHeight);
      }
      frameRequest = window.requestAnimationFrame(draw);
    };
    draw();
    onStatus("已连接 · 双向设备签名认证 · 点对点加密画面");
  };
  peer.addTransceiver("video", { direction: "recvonly" });
  const offer = await peer.createOffer();
  await peer.setLocalDescription(offer);
  if (!peer.localDescription) throw new Error("无法生成点对点画面请求");
  await onSignal({ session, kind: "offer", description: peer.localDescription.toJSON() });
  offerSent = true;
  for (const candidate of queuedCandidates.splice(0)) {
    await onSignal({ session, kind: "ice", candidate });
  }
  onStatus("已发出安全查看请求，正在建立点对点画面…");
  return {
    async accept(signal: ViewPeerSignal) {
      if (signal.session.sessionId !== session.sessionId || stopped) return;
      if (signal.kind === "answer" && signal.description) {
        await peer.setRemoteDescription(signal.description);
        for (const candidate of pendingRemoteCandidates.splice(0)) {
          await peer.addIceCandidate(candidate);
        }
      } else if (signal.kind === "ice" && signal.candidate) {
        if (peer.remoteDescription) await peer.addIceCandidate(signal.candidate);
        else pendingRemoteCandidates.push(signal.candidate);
      }
    },
    async close() {
      stopped = true;
      window.cancelAnimationFrame(frameRequest);
      peer.onicecandidate = null;
      peer.onconnectionstatechange = null;
      peer.ontrack = null;
      peer.close();
      if (video) {
        (video.srcObject as MediaStream | null)?.getTracks().forEach((track) => track.stop());
        video.srcObject = null;
      }
      target.replaceChildren();
      window.clearTimeout(availabilityTimer);
    },
  };
}

export function realtimeError(reason: unknown) {
  return reasonText(reason);
}
