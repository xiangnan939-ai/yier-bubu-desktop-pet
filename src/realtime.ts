import { invoke } from "@tauri-apps/api/core";
import type { ChatSDK, Message } from "@tencentcloud/chat";
import type TRTC from "trtc-sdk-v5";

let trtcModule: Promise<typeof import("trtc-sdk-v5")["default"]> | null = null;
function loadTRTC() {
  trtcModule ??= import("trtc-sdk-v5").then((module) => module.default);
  return trtcModule;
}

type ChatNamespace = typeof import("@tencentcloud/chat")["default"];
let chatModule: Promise<ChatNamespace> | null = null;
function loadChat() {
  chatModule ??= import("@tencentcloud/chat").then((module) => module.default);
  return chatModule;
}

export type RealtimeCredentials = {
  sdkAppId: number;
  userId: string;
  partnerUserId: string;
  userSig: string;
  expiresAtMs: number;
  endpoint: string;
};

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
export const VIEW_SESSION_KEY = "yier-bubu-view-session";

type SignalHandler = (result: SignalProcessResult, signal: SignedSignal) => void | Promise<void>;

function reasonText(reason: unknown) {
  if (reason instanceof Error) return reason.message;
  if (typeof reason === "object" && reason && "message" in reason) return String(reason.message);
  return String(reason);
}

export class RealtimeMessaging {
  private chat: ChatSDK | null = null;
  private chatNamespace: ChatNamespace | null = null;
  private credentials: RealtimeCredentials | null = null;
  private connectPromise: Promise<void> | null = null;
  private historyPollTimer: number | null = null;
  private seenSignalNonces = new Set<string>();
  private ready = false;
  private handler: SignalHandler;

  constructor(handler: SignalHandler) {
    this.handler = handler;
  }

  connect(forceRefresh = false) {
    const expiring = Boolean(this.credentials && this.credentials.expiresAtMs < Date.now() + 48 * 60 * 60_000);
    if (this.ready && !forceRefresh && !expiring) return Promise.resolve();
    if (this.connectPromise) return this.connectPromise;
    this.connectPromise = this.connectInternal(forceRefresh || expiring).finally(() => {
      this.connectPromise = null;
    });
    return this.connectPromise;
  }

  private async connectInternal(forceRefresh: boolean) {
    const TencentCloudChat = await loadChat();
    this.chatNamespace = TencentCloudChat;
    const credentials = await invoke<RealtimeCredentials>("realtime_credentials", { forceRefresh });
    if (this.chat && (this.credentials?.userId !== credentials.userId || forceRefresh)) {
      this.chat.off(TencentCloudChat.EVENT.MESSAGE_RECEIVED, this.receiveMessages, this);
      this.chat.off(TencentCloudChat.EVENT.CONVERSATION_LIST_UPDATED, this.conversationUpdated, this);
      this.chat.off(TencentCloudChat.EVENT.SDK_READY, this.markReady, this);
      this.chat.off(TencentCloudChat.EVENT.SDK_NOT_READY, this.markNotReady, this);
      this.chat.off(TencentCloudChat.EVENT.KICKED_OUT, this.reconnect, this);
      await this.chat.logout().catch(() => undefined);
      this.chat = null;
      this.ready = false;
    }
    this.credentials = credentials;
    if (!this.chat) {
      this.chat = TencentCloudChat.create({ SDKAppID: credentials.sdkAppId });
      this.chat.on(TencentCloudChat.EVENT.MESSAGE_RECEIVED, this.receiveMessages, this);
      this.chat.on(TencentCloudChat.EVENT.CONVERSATION_LIST_UPDATED, this.conversationUpdated, this);
      this.chat.on(TencentCloudChat.EVENT.SDK_READY, this.markReady, this);
      this.chat.on(TencentCloudChat.EVENT.SDK_NOT_READY, this.markNotReady, this);
      this.chat.on(TencentCloudChat.EVENT.KICKED_OUT, this.reconnect, this);
    }
    const readyPromise = this.ready ? Promise.resolve() : new Promise<void>((resolve, reject) => {
      const onReady = () => {
        window.clearTimeout(timeout);
        this.chat?.off(TencentCloudChat.EVENT.SDK_READY, onReady);
        resolve();
      };
      const timeout = window.setTimeout(() => {
        this.chat?.off(TencentCloudChat.EVENT.SDK_READY, onReady);
        reject(new Error("联网消息通道启动超时"));
      }, 15_000);
      this.chat?.on(TencentCloudChat.EVENT.SDK_READY, onReady);
    });
    await this.chat.login({ userID: credentials.userId, userSig: credentials.userSig });
    await readyPromise;
    this.ready = true;
    this.startHistoryPolling();
  }

  private markReady() {
    this.ready = true;
  }

  private markNotReady() {
    this.ready = false;
  }

  private reconnect() {
    this.ready = false;
    window.setTimeout(() => this.connect(true).catch(() => undefined), 1_500);
  }

  private receiveMessages(event: { data?: Message[] }) {
    this.processMessages(event.data ?? []).catch(() => undefined);
  }

  private conversationUpdated() {
    this.pollRecentMessages().catch(() => undefined);
  }

  private async processMessages(messages: Message[]) {
    const expectedSender = this.credentials?.partnerUserId;
    for (const message of messages) {
      if (!expectedSender || message.from !== expectedSender
        || message.type !== this.chatNamespace?.TYPES.MSG_TEXT) continue;
      let signal: SignedSignal;
      try {
        signal = JSON.parse(String(message.payload?.text ?? "")) as SignedSignal;
      } catch {
        continue;
      }
      if (typeof signal.core?.nonce !== "string" || this.seenSignalNonces.has(signal.core.nonce)) continue;
      this.seenSignalNonces.add(signal.core.nonce);
      if (this.seenSignalNonces.size > 200) {
        const oldest = this.seenSignalNonces.values().next().value;
        if (oldest) this.seenSignalNonces.delete(oldest);
      }
      try {
        const result = await invoke<SignalProcessResult>("process_realtime_signal", { signal });
        if (result.reply) await this.send(result.reply);
        await this.handler(result, signal);
      } catch {
        // Expired, duplicate, or invalid messages are intentionally ignored.
      }
    }
  }

  private startHistoryPolling() {
    if (this.historyPollTimer !== null) return;
    const poll = () => this.pollRecentMessages().catch(() => undefined);
    poll();
    // Tencent Chat occasionally records a WebView message without firing its
    // MESSAGE_RECEIVED callback. Pulling the latest signed messages provides a
    // low-latency fallback while Rust still verifies sender, binding and nonce.
    this.historyPollTimer = window.setInterval(poll, 10_000);
  }

  private async pollRecentMessages() {
    if (!this.ready || !this.chat || !this.credentials) return;
    const response = await this.chat.getMessageList({
      conversationID: `C2C${this.credentials.partnerUserId}`,
    });
    const messages = Array.isArray(response?.data?.messageList)
      ? response.data.messageList as Message[] : [];
    await this.processMessages(messages);
  }

  async send(signal: SignedSignal) {
    await this.connect();
    if (!this.chat || !this.credentials || !this.chatNamespace) throw new Error("联网消息通道尚未就绪");
    const message = this.chat.createTextMessage({
      to: this.credentials.partnerUserId,
      conversationType: this.chatNamespace.TYPES.CONV_C2C,
      payload: { text: JSON.stringify(signal) },
    });
    await this.chat.sendMessage(message);
  }

  async close() {
    if (this.historyPollTimer !== null) {
      window.clearInterval(this.historyPollTimer);
      this.historyPollTimer = null;
    }
    if (!this.chat || !this.chatNamespace) return;
    this.chat.off(this.chatNamespace.EVENT.MESSAGE_RECEIVED, this.receiveMessages, this);
    this.chat.off(this.chatNamespace.EVENT.CONVERSATION_LIST_UPDATED, this.conversationUpdated, this);
    this.chat.off(this.chatNamespace.EVENT.SDK_READY, this.markReady, this);
    this.chat.off(this.chatNamespace.EVENT.SDK_NOT_READY, this.markNotReady, this);
    this.chat.off(this.chatNamespace.EVENT.KICKED_OUT, this.reconnect, this);
    await this.chat.logout().catch(() => undefined);
    this.chat = null;
    this.chatNamespace = null;
    this.ready = false;
  }
}

function frameBytes(value: unknown): Uint8Array {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (Array.isArray(value)) return new Uint8Array(value);
  throw new Error("桌面画面数据格式无效");
}

export class ScreenPublisher {
  private trtc: TRTC | null = null;
  private canvas: HTMLCanvasElement | null = null;
  private track: MediaStreamTrack | null = null;
  private stopped = true;
  private session: ViewSession | null = null;
  private safetyTimer = 0;

  async start(session: ViewSession, onError?: (message: string) => void | Promise<void>) {
    if (this.session?.sessionId === session.sessionId && !this.stopped) return;
    await this.stop();
    this.stopped = false;
    this.session = session;
    try {
      await invoke("ensure_screen_capture_permission");
      await invoke("set_screen_share_active", { active: true });
      const credentials = await invoke<RealtimeCredentials>("realtime_credentials", { forceRefresh: false });
      const TRTCClass = await loadTRTC();
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
      const trtc = TRTCClass.create();
      this.trtc = trtc;
      let viewerJoined = false;
      trtc.on(TRTCClass.EVENT.REMOTE_USER_ENTER, ({ userId }) => {
        if (userId !== credentials.partnerUserId) return;
        viewerJoined = true;
        window.clearTimeout(this.safetyTimer);
        this.safetyTimer = window.setTimeout(() => this.stop(), 30 * 60_000);
      });
      trtc.on(TRTCClass.EVENT.REMOTE_USER_EXIT, ({ userId }) => {
        if (viewerJoined && userId === credentials.partnerUserId) this.stop().catch(() => undefined);
      });
      await trtc.enterRoom({
        roomId: session.roomId,
        sdkAppId: credentials.sdkAppId,
        userId: credentials.userId,
        userSig: credentials.userSig,
        autoReceiveAudio: false,
        autoReceiveVideo: false,
        enableAutoPlayDialog: false,
      });
      await trtc.startLocalVideo({
        publish: true,
        option: {
          videoTrack: track,
          mirror: false,
          fillMode: "contain",
          profile: { width: 1280, height: 720, frameRate: 10, bitrate: 1200 },
          qosPreference: TRTCClass.TYPE.QOS_PREFERENCE_SMOOTH,
        },
      });
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
      this.safetyTimer = window.setTimeout(
        () => this.stop().catch(() => undefined),
        viewerJoined ? 30 * 60_000 : 30_000,
      );
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

  async stop() {
    this.stopped = true;
    window.clearTimeout(this.safetyTimer);
    this.safetyTimer = 0;
    this.session = null;
    if (this.trtc) {
      await this.trtc.stopLocalVideo().catch(() => undefined);
      await this.trtc.exitRoom().catch(() => undefined);
      this.trtc.destroy();
      this.trtc = null;
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
) {
  const credentials = await invoke<RealtimeCredentials>("realtime_credentials", { forceRefresh: false });
  const TRTCClass = await loadTRTC();
  const trtc = TRTCClass.create();
  let playing = false;
  const availabilityTimer = window.setTimeout(() => {
    if (!playing) onStatus("连接超时：未收到对方画面，请确认对方电脑已开机且桌宠正在运行");
  }, 25_000);
  trtc.on(TRTCClass.EVENT.REMOTE_VIDEO_AVAILABLE, async ({ userId, streamType }) => {
    if (userId !== credentials.partnerUserId || playing) return;
    playing = true;
    window.clearTimeout(availabilityTimer);
    target.querySelector(".viewer-placeholder")?.remove();
    await trtc.startRemoteVideo({
      userId,
      streamType,
      view: target,
      option: {
        fillMode: "contain",
        // WebView2 can promote a <video> into a native hardware overlay. In a
        // transparent multi-window desktop app that overlay may cover the
        // complete client area and appear as a white window. Canvas rendering
        // keeps the remote frame inside the viewer stage on both platforms.
        canvasRender: true,
      },
    });
    onStatus("已连接 · 双向设备签名认证 · 只读实时画面");
  });
  await trtc.enterRoom({
    roomId: session.roomId,
    sdkAppId: credentials.sdkAppId,
    userId: credentials.userId,
    userSig: credentials.userSig,
    autoReceiveAudio: false,
    autoReceiveVideo: false,
    enableAutoPlayDialog: false,
  });
  onStatus("已进入安全房间，正在等待对方画面…");
  return async () => {
    window.clearTimeout(availabilityTimer);
    await trtc.exitRoom().catch(() => undefined);
    trtc.destroy();
  };
}

export function realtimeError(reason: unknown) {
  return reasonText(reason);
}
