import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { relaunch } from "@tauri-apps/plugin-process";
import {
  LogicalPosition,
  LogicalSize,
  PhysicalPosition,
  currentMonitor,
  getCurrentWindow,
} from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { PetAnimationStateMachine } from "./animation/stateMachine";
import type { AnimationSnapshot } from "./animation/types";
import { DEFAULT_WALK_CHANCE } from "./animation/weights";
import { buildPetLibrary, type PetRole } from "./petAssets";
import {
  RealtimeMessaging,
  ScreenPublisher,
  VIEW_SESSION_KEY,
  createViewSession,
  loadViewSession,
  realtimeError,
  startScreenViewer,
  type SignedSignal,
  type ViewErrorPayload,
  type ViewPeerSignal,
  type ViewSession,
} from "./realtime";
import "./App.css";

type AppProfile = {
  role: PetRole;
  petName: string;
  partnerName: string;
  remoteMenuLabel: string;
  platform: string;
};

type BindingStatus = {
  state: "unbound" | "bound" | "revoking" | "revoked";
  petName: string;
  partnerName: string;
  bindingId: string | null;
  signalingRoute: string | null;
  partnerUserId: string | null;
  partnerMachineCode: string | null;
  createdAtMs: number | null;
  incomingUnbind: boolean;
  outgoingUnbind: boolean;
  approvalPending: boolean;
  requestedByName: string | null;
  realtimeConfigured: boolean;
  localPublicKey: string;
};
type PairingResult = { state: string; message: string };
type UpdateConfiguration = {
  currentVersion: string;
  appUpdateEnabled: boolean;
};
type AppUpdateCheck = {
  available: boolean;
  currentVersion: string;
  version: string | null;
  notes: string | null;
};
type UpdateProgress = {
  updateType: "app" | "assets";
  phase: "downloading" | "installing" | "complete";
  downloadedBytes: number;
  totalBytes: number | null;
};
function formatBytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

const DEFAULT_PET_SIZE = 160;
const MIN_PET_SIZE = 96;
const MAX_PET_SIZE = 320;
const MENU_WIDTH = 164;
const MENU_HEIGHT = 94;
const SETTINGS_WIDTH = 420;
const SETTINGS_HEIGHT = 700;
const BINDING_WIDTH = 420;
const BINDING_HEIGHT = 450;
const PAIRING_CODE_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

const fallbackProfile: AppProfile = /Mac/i.test(navigator.userAgent)
  ? { role: "yier", petName: "一二", partnerName: "布布", remoteMenuLabel: "看看TA在干嘛", platform: "macos" }
  : { role: "bubu", petName: "布布", partnerName: "一二", remoteMenuLabel: "看看TA在干嘛", platform: "windows" };

function clampPetSize(value: number) {
  return Math.min(MAX_PET_SIZE, Math.max(MIN_PET_SIZE, Math.round(value)));
}

function loadPetSize() {
  const stored = Number(localStorage.getItem("petSize"));
  return Number.isFinite(stored) && stored > 0 ? clampPetSize(stored) : DEFAULT_PET_SIZE;
}

function normalizePairingCode(value: string) {
  return value.toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 16);
}

function formatPairingCode(value: string) {
  return normalizePairingCode(value).match(/.{1,4}/g)?.join("-") ?? "";
}

function generatePairingCode() {
  const random = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(random, (byte) => PAIRING_CODE_ALPHABET[byte & 31]).join("");
}

async function openBindingWindow() {
  try {
    const existing = await WebviewWindow.getByLabel("binding");
    if (existing) {
      await existing.show();
      await existing.setFocus();
      return;
    }
    new WebviewWindow("binding", {
      url: "/?mode=binding", title: "绑定一二与布布",
      width: BINDING_WIDTH, height: BINDING_HEIGHT, center: true,
      decorations: true, transparent: false, resizable: false,
    });
  } catch {
    window.open("/?mode=binding", "_blank");
  }
}

async function resizeMainPet(size: number) {
  const main = await WebviewWindow.getByLabel("main");
  await main?.setSize(new LogicalSize(size, size));
}

async function positionBesidePet(width: number, height: number, gap = 12) {
  const main = await WebviewWindow.getByLabel("main");
  const monitor = await currentMonitor();
  if (!main || !monitor) return null;

  const [petPhysicalPosition, petPhysicalSize] = await Promise.all([
    main.outerPosition(),
    main.outerSize(),
  ]);
  const scale = monitor.scaleFactor;
  const petPosition = petPhysicalPosition.toLogical(scale);
  const petSize = petPhysicalSize.toLogical(scale);
  const workPosition = monitor.workArea.position.toLogical(scale);
  const workSize = monitor.workArea.size.toLogical(scale);
  const minX = workPosition.x + 6;
  const minY = workPosition.y + 6;
  const maxX = workPosition.x + workSize.width - width - 6;
  const maxY = workPosition.y + workSize.height - height - 6;

  const right = petPosition.x + petSize.width + gap;
  const left = petPosition.x - width - gap;
  const x = right <= maxX ? right : left >= minX ? left : Math.min(maxX, Math.max(minX, right));
  const centeredY = petPosition.y + (petSize.height - height) / 2;
  return new LogicalPosition(Math.round(x), Math.round(Math.min(maxY, Math.max(minY, centeredY))));
}

function Viewer() {
  const [status, setStatus] = useState("正在连接对方电脑…");
  const stageRef = useRef<HTMLDivElement>(null);
  const sessionRef = useRef<ViewSession | null>(null);

  useEffect(() => {
    let viewerConnection: Awaited<ReturnType<typeof startScreenViewer>> | null = null;
    let errorListener: (() => void) | null = null;
    let peerListener: (() => void) | null = null;
    const pendingSignals: ViewPeerSignal[] = [];
    listen<ViewErrorPayload>("viewer-error", (event) => {
      if (event.payload.sessionId !== sessionRef.current?.sessionId) return;
      setStatus(`连接失败：${event.payload.message}`);
      emitTo("main", "viewer-status", "failed").catch(() => undefined);
    }).then((dispose) => { errorListener = dispose; }).catch(() => undefined);
    listen<ViewPeerSignal>("viewer-peer-signal", (event) => {
      if (event.payload.session.sessionId !== sessionRef.current?.sessionId) return;
      if (viewerConnection) viewerConnection.accept(event.payload).catch(() => undefined);
      else pendingSignals.push(event.payload);
    }).then((dispose) => { peerListener = dispose; }).catch(() => undefined);
    Promise.resolve().then(async () => {
      const session = loadViewSession();
      sessionRef.current = session;
      if (!stageRef.current) throw new Error("画面窗口尚未就绪");
      viewerConnection = await startScreenViewer(
        stageRef.current,
        session,
        (next) => {
          setStatus(next);
          if (next.startsWith("已连接")) emitTo("main", "viewer-status", "connected").catch(() => undefined);
          else if (next.startsWith("连接失败") || next.startsWith("连接超时")) {
            emitTo("main", "viewer-status", "failed").catch(() => undefined);
          }
        },
        (signal) => emitTo("main", "viewer-peer-signal", signal),
      );
      for (const signal of pendingSignals.splice(0)) await viewerConnection.accept(signal);
    }).catch((reason) => {
      setStatus(`连接失败：${realtimeError(reason)}`);
      emitTo("main", "viewer-status", "failed").catch(() => undefined);
    });
    return () => {
      errorListener?.();
      peerListener?.();
      viewerConnection?.close().catch(() => undefined);
    };
  }, []);

  const endViewing = () => {
    invoke("close_viewer_window", { session: sessionRef.current }).catch(() =>
      getCurrentWindow().destroy().catch(() => undefined));
  };

  return (
    <main className="viewer-shell">
      <header>
        <div><strong>一二布布 · 对方桌面</strong><span>{status}</span></div>
        <button onClick={endViewing}>结束查看</button>
      </header>
      <section ref={stageRef} className="viewer-stage"><div className="viewer-placeholder">{status}</div></section>
    </main>
  );
}

function BindingSetup() {
  const [profile, setProfile] = useState(fallbackProfile);
  const [status, setStatus] = useState<BindingStatus | null>(null);
  const [passphrase, setPassphrase] = useState("");
  const [message, setMessage] = useState("绑定完成后，桌宠才会出现在桌面上。");
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => invoke<BindingStatus>("binding_status").then(setStatus), []);
  useEffect(() => {
    invoke<AppProfile>("app_profile").then(setProfile).catch(() => undefined);
    refresh().catch((reason) => setMessage(String(reason)));
    const unlisten = listen("binding-changed", () => refresh().catch(() => undefined));
    return () => { unlisten.then((dispose) => dispose()).catch(() => undefined); };
  }, [refresh]);

  const pair = async (code = passphrase) => {
    if (busy) return;
    const normalized = normalizePairingCode(code);
    if (normalized.length !== 16) {
      setMessage("请输入 Mac 上生成的完整 16 位配对码。");
      return;
    }
    setBusy(true);
    setMessage(profile.role === "bubu" ? "正在连接 Mac 上的一二…" : "配对码已生成，正在等待 Windows 上的布布输入…");
    try {
      const result = await invoke<PairingResult>("pair_device", { passphrase: normalized });
      setPassphrase("");
      setMessage(result.message);
      await refresh();
      await emitTo("main", "binding-changed").catch(() => undefined);
      window.setTimeout(() => getCurrentWindow().destroy(), 900);
    } catch (reason) {
      setMessage(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const generateAndWait = () => {
    const code = generatePairingCode();
    setPassphrase(code);
    pair(code);
  };

  const alreadyBound = status?.state === "bound" || status?.state === "revoking";

  return (
    <main className="binding-shell">
      <div className="binding-mark">{profile.petName}</div>
      <h1>{alreadyBound ? `${profile.petName}已经绑定` : profile.role === "yier" ? "生成配对码" : "输入配对码"}</h1>
      {alreadyBound ? (
        <>
          <p>当前绑定对象：{status?.partnerName}。绑定后只能由双方签名同意才能解除。</p>
          <button className="primary wide" onClick={async () => {
            await emitTo("main", "binding-changed").catch(() => undefined);
            await getCurrentWindow().destroy();
          }}>知道了</button>
        </>
      ) : (
        <>
          {profile.role === "yier" ? (
            <>
              <p>在 Mac 上生成一次性配对码，再把它输入 Windows 上的布布。配对码 3 分钟内有效。</p>
              {passphrase && <div className="pairing-code" aria-label="配对码">{formatPairingCode(passphrase)}</div>}
              <button className="primary wide" disabled={busy} onClick={generateAndWait}>
                {busy ? "正在等待 Windows 输入…" : "生成配对码"}
              </button>
            </>
          ) : (
            <>
              <p>输入 Mac 上的一二生成的配对码，验证两台电脑后即可显示桌宠。</p>
              <input autoFocus type="text" inputMode="text" autoComplete="off" spellCheck={false}
                value={formatPairingCode(passphrase)} disabled={busy} maxLength={19}
                onChange={(event) => setPassphrase(normalizePairingCode(event.target.value))}
                onKeyDown={(event) => { if (event.key === "Enter") pair(); }}
                placeholder="XXXX-XXXX-XXXX-XXXX" />
              <button className="primary wide" disabled={busy} onClick={() => pair()}>
                {busy ? "正在安全绑定…" : "完成绑定"}
              </button>
            </>
          )}
          <p className="binding-message">{message}</p>
          <p className="binding-hint">配对码不会保存；绑定记录仍由双方设备签名并锁定机器特征。</p>
        </>
      )}
    </main>
  );
}

function Settings() {
  const [petSize, setPetSize] = useState(loadPetSize);
  const [error, setError] = useState("");
  const [binding, setBinding] = useState<BindingStatus | null>(null);
  const [bindingMessage, setBindingMessage] = useState("正在读取绑定状态…");
  const [bindingBusy, setBindingBusy] = useState(false);
  const [updateConfig, setUpdateConfig] = useState<UpdateConfiguration | null>(null);
  const [appUpdate, setAppUpdate] = useState<AppUpdateCheck | null>(null);
  const [updateMessage, setUpdateMessage] = useState("正在读取更新状态…");
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [installingApp, setInstallingApp] = useState(false);
  const [updateProgress, setUpdateProgress] = useState<UpdateProgress | null>(null);

  useEffect(() => {
    invoke<UpdateConfiguration>("update_configuration").then((value) => {
      setUpdateConfig(value);
      if (!value.appUpdateEnabled) {
        setUpdateMessage("发布地址尚未配置；当前安装包仍可正常离线使用。");
      } else {
        setUpdateMessage("点击检查更新。");
      }
    }).catch((reason) => setUpdateMessage(String(reason)));
    invoke<BindingStatus>("binding_status").then((value) => {
      setBinding(value);
      setBindingMessage(value.state === "bound" ? "" : "尚未完成双机绑定");
    }).catch((reason) => setBindingMessage(String(reason)));
  }, []);

  useEffect(() => {
    const unlisten = listen<UpdateProgress>("update-download-progress", (event) => {
      setUpdateProgress(event.payload);
    });
    return () => {
      unlisten.then((dispose) => dispose()).catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    const refresh = () => invoke<BindingStatus>("binding_status").then(setBinding).catch(() => undefined);
    const unlisten = listen("binding-changed", refresh);
    const timer = window.setInterval(refresh, 2_000);
    return () => {
      window.clearInterval(timer);
      unlisten.then((dispose) => dispose()).catch(() => undefined);
    };
  }, []);

  const checkUpdates = async () => {
    if (!updateConfig?.appUpdateEnabled || checkingUpdates || installingApp) return;
    setCheckingUpdates(true);
    setUpdateProgress(null);
    setError("");
    setUpdateMessage("正在检查更新…");
    try {
      const result = await invoke<AppUpdateCheck>("check_app_update");
      setAppUpdate(result);
      if (!result.available) {
        setUpdateMessage("已是最新版");
        return;
      }
      setCheckingUpdates(false);
      setInstallingApp(true);
      setUpdateMessage(`发现新版本 ${result.version ?? ""}，正在下载并安装…`);
      await invoke("install_app_update");
      setUpdateMessage("更新安装完成，正在重新启动…");
      await relaunch();
    } catch (reason) {
      setUpdateMessage(`更新失败：${String(reason)}`);
      setUpdateProgress(null);
      setInstallingApp(false);
    } finally {
      setCheckingUpdates(false);
    }
  };

  const applyPetSize = (value: number) => {
    const size = clampPetSize(value);
    setPetSize(size);
    localStorage.setItem("petSize", String(size));
    resizeMainPet(size).catch(() => undefined);
    emitTo("main", "settings-updated", { petSize: size }).catch(() => undefined);
  };

  const sendUnbindRequest = async () => {
    if (bindingBusy) return;
    setBindingBusy(true);
    setError("");
    try {
      const signal = await invoke<SignedSignal>("request_unbind");
      await emitTo("main", "send-realtime-signal", signal);
      setBindingMessage("已安全发送解绑请求，正在等待对方决定。");
      setBinding(await invoke<BindingStatus>("binding_status"));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBindingBusy(false);
    }
  };

  const answerUnbind = async (approve: boolean) => {
    if (bindingBusy) return;
    setBindingBusy(true);
    setError("");
    try {
      const signal = await invoke<SignedSignal>("respond_unbind", { approve });
      await emitTo("main", "send-realtime-signal", signal);
      setBindingMessage(approve ? "已签名同意，正在等待对方确认完成。" : "已拒绝解绑，当前绑定继续有效。");
      setBinding(await invoke<BindingStatus>("binding_status"));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBindingBusy(false);
    }
  };

  return (
    <main className="settings-shell">
      <header><h1>桌宠设置</h1><p>修改后会立即应用到桌宠。</p></header>

      <section className="settings-section">
        <div className="setting-heading"><h2>桌宠大小</h2><strong>{petSize}px</strong></div>
        <input aria-label="桌宠大小" type="range" min={MIN_PET_SIZE} max={MAX_PET_SIZE} step="8"
          value={petSize} onChange={(event) => applyPetSize(Number(event.target.value))} />
        <div className="range-labels"><span>较小</span><span>默认 160px</span><span>较大</span></div>
      </section>

      <section className="settings-section update-section">
        <div className="setting-heading"><h2>更新</h2><strong>版本 {updateConfig?.currentVersion ?? "…"}</strong></div>
        <p>{updateMessage}</p>
        {updateProgress && <div className="update-progress" aria-live="polite">
          <div className="update-progress-label">
            <span>更新 · {updateProgress.phase === "downloading" ? "正在下载"
                : updateProgress.phase === "installing" ? "正在验证并安装" : "更新完成"
            }</span>
            <strong>{updateProgress.phase === "downloading" && updateProgress.totalBytes
              ? `${Math.min(100, Math.round(updateProgress.downloadedBytes / updateProgress.totalBytes * 100))}%`
              : updateProgress.phase === "complete" ? "100%" : "请稍候"}</strong>
          </div>
          <div className={`update-progress-track ${updateProgress.phase !== "downloading"
            || !updateProgress.totalBytes ? "indeterminate" : ""}`}>
            <span style={updateProgress.phase === "downloading" && updateProgress.totalBytes
              ? { width: `${Math.min(100, updateProgress.downloadedBytes / updateProgress.totalBytes * 100)}%` }
              : undefined} />
          </div>
          {updateProgress.phase === "downloading" && <small>
            已下载 {formatBytes(updateProgress.downloadedBytes)}
            {updateProgress.totalBytes ? ` / ${formatBytes(updateProgress.totalBytes)}` : ""}
          </small>}
        </div>}
        {appUpdate?.notes && <p className="update-notes">{appUpdate.notes}</p>}
        <div className="update-actions">
          <button disabled={checkingUpdates || installingApp || !updateConfig?.appUpdateEnabled} onClick={checkUpdates}>
            {installingApp ? "正在更新…" : checkingUpdates ? "正在检查…" : "检查更新"}
          </button>
        </div>
      </section>

      <section className="settings-section">
        <h2>双机连接</h2>
        <div className={`binding-state state-${binding?.state ?? "loading"}`}>
          <strong>{binding?.state === "bound" ? `已绑定${binding.partnerName}`
            : binding?.state === "revoking" ? "正在完成双方解绑"
              : binding?.state === "revoked" ? "双方已解绑" : "尚未绑定"}</strong>
          {binding?.partnerMachineCode && <span>对方机器校验码：{binding.partnerMachineCode}</span>}
          {binding?.createdAtMs && <span>绑定时间：{new Date(binding.createdAtMs).toLocaleString()}</span>}
        </div>
        {bindingMessage && <p>{bindingMessage}</p>}
        {binding?.localPublicKey && <details className="device-public-key">
          <summary>开发者：本机设备公钥</summary><code>{binding.localPublicKey}</code>
        </details>}
        {binding?.incomingUnbind && !binding.approvalPending && <div className="unbind-request">
          <strong>{binding.requestedByName}请求解除绑定</strong>
          <p>只有你明确同意后才会解绑；拒绝会继续保留当前绑定。</p>
          <div className="update-actions">
            <button disabled={bindingBusy} onClick={() => answerUnbind(false)}>拒绝解绑</button>
            <button className="danger" disabled={bindingBusy} onClick={() => answerUnbind(true)}>同意解绑</button>
          </div>
        </div>}
        {binding?.approvalPending && <button disabled={bindingBusy}
          onClick={() => answerUnbind(true)}>重试完成双方解绑</button>}
        {binding?.state === "bound" && !binding.incomingUnbind && !binding.outgoingUnbind
          && <button className="unbind-button" disabled={bindingBusy} onClick={sendUnbindRequest}>请求双方解绑</button>}
        {binding?.outgoingUnbind && <p className="pending-note">已发送请求，正在等待对方决定。你不能单方面解除绑定。</p>}
        {(binding?.state === "unbound" || binding?.state === "revoked")
          && <button className="primary-inline" onClick={openBindingWindow}>打开首次绑定</button>}
      </section>

      {error && <p className="settings-error">{error}</p>}
      <footer><button className="primary" onClick={() => getCurrentWindow().close()}>关闭</button></footer>
    </main>
  );
}

function PetMenu() {
  const choose = async (action: "viewer" | "settings") => {
    await emitTo("main", "pet-menu-action", action).catch(() => undefined);
    await getCurrentWindow().close();
  };

  useEffect(() => {
    const close = () => getCurrentWindow().close();
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") close(); };
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey);
    };
  }, []);

  return (
    <main className="menu-window-shell">
      <nav className="pet-menu">
        <button onClick={() => choose("viewer")}>看看TA在干嘛</button>
        <button onClick={() => choose("settings")}>设置</button>
      </nav>
    </main>
  );
}

function Pet() {
  const [profile, setProfile] = useState(fallbackProfile);
  const library = useMemo(() => buildPetLibrary(profile.role), [profile.role]);
  const [animation, setAnimation] = useState<AnimationSnapshot>({
    action: "idle",
    assetUrl: "",
    assetKey: 0,
    mirrored: false,
    macroState: "idle",
    mode: "ambient",
  });
  const [isSharing, setIsSharing] = useState(false);
  const [petSize, setPetSize] = useState(loadPetSize);
  const animationRef = useRef<PetAnimationStateMachine | null>(null);
  const realtimeRef = useRef<RealtimeMessaging | null>(null);
  const publisherRef = useRef<ScreenPublisher | null>(null);

  useEffect(() => {
    invoke<AppProfile>("app_profile").then(setProfile).catch(() => undefined);
    invoke("ensure_screen_capture_permission").catch((reason) => {
      console.warn("屏幕录制权限尚未准备好", reason);
    });
    localStorage.removeItem("pairing");
    const size = loadPetSize();
    setPetSize(size);
    getCurrentWindow().setSize(new LogicalSize(size, size)).catch(() => undefined);

    const unlisten = listen<{ petSize: number }>("settings-updated", (event) => {
      const nextSize = clampPetSize(event.payload.petSize);
      setPetSize(nextSize);
      localStorage.setItem("petSize", String(nextSize));
    });
    return () => { unlisten.then((dispose) => dispose()).catch(() => undefined); };
  }, []);

  useEffect(() => {
    const windowHandle = getCurrentWindow();
    const engine = new PetAnimationStateMachine({
      role: profile.role,
      library,
      walkChance: DEFAULT_WALK_CHANCE,
      onState: setAnimation,
      getWindowMetrics: async () => {
        const [monitor, position, size] = await Promise.all([
          currentMonitor(),
          windowHandle.outerPosition(),
          windowHandle.outerSize(),
        ]);
        if (!monitor) throw new Error("无法读取桌宠所在显示器");
        return {
          x: position.x,
          y: position.y,
          width: size.width,
          height: size.height,
          workAreaX: monitor.workArea.position.x,
          workAreaY: monitor.workArea.position.y,
          workAreaWidth: monitor.workArea.size.width,
          workAreaHeight: monitor.workArea.size.height,
          scaleFactor: monitor.scaleFactor,
        };
      },
      moveWindow: (x, y) => windowHandle.setPosition(new PhysicalPosition(x, y)),
    });
    animationRef.current = engine;
    engine.start();
    return () => {
      engine.stop();
      if (animationRef.current === engine) animationRef.current = null;
    };
  }, [profile.role]);

  useEffect(() => {
    animationRef.current?.updateLibrary(library);
  }, [library]);

  useEffect(() => {
    animationRef.current?.updateExternalState({ screenSharing: isSharing });
  }, [isSharing]);

  useEffect(() => {
    let stopped = false;
    let interval = 0;
    const check = async () => {
      const config = await invoke<UpdateConfiguration>("update_configuration").catch(() => null);
      if (!config || stopped) return;
      if (config.appUpdateEnabled && !stopped) {
        const result = await invoke<AppUpdateCheck>("check_app_update").catch(() => null);
        if (result) localStorage.setItem("lastAppUpdateCheck", JSON.stringify(result));
      }
    };
    const initial = window.setTimeout(check, 8_000);
    interval = window.setInterval(check, 6 * 60 * 60_000);
    return () => {
      stopped = true;
      window.clearTimeout(initial);
      window.clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    const check = () => invoke<boolean>("screen_share_active")
      .then(setIsSharing)
      .catch(() => undefined);
    check();
    const timer = window.setInterval(check, 1500);
    return () => window.clearInterval(timer);
  }, []);

  const openSettings = useCallback(async () => {
    const position = await positionBesidePet(SETTINGS_WIDTH, SETTINGS_HEIGHT);
    try {
      const existing = await WebviewWindow.getByLabel("settings");
      if (existing) {
        if (position) await existing.setPosition(position);
        await existing.show();
        await existing.setFocus();
      } else {
        new WebviewWindow("settings", {
          url: "/?mode=settings", title: "桌宠设置", width: SETTINGS_WIDTH, height: SETTINGS_HEIGHT,
          x: position?.x, y: position?.y, decorations: true, transparent: false, resizable: false,
        });
      }
    } catch {
      window.open("/?mode=settings", "_blank");
    }
  }, []);

  const openViewer = useCallback(async () => {
    const binding = await invoke<BindingStatus>("binding_status").catch(() => null);
    if (!binding || binding.state !== "bound") {
      await openBindingWindow();
      return;
    }
    let attemptedSessionId: string | null = null;
    try {
      const messaging = realtimeRef.current;
      if (!messaging) throw new Error("内置联网通道尚未启动");
      const session = await createViewSession();
      attemptedSessionId = session.sessionId;
      localStorage.setItem(VIEW_SESSION_KEY, JSON.stringify(session));
      const existingViewer = await WebviewWindow.getByLabel("viewer");
      if (existingViewer) await existingViewer.destroy();
      const viewer = new WebviewWindow("viewer", {
        url: "/?mode=viewer",
        title: "看看TA在干嘛",
        width: 1_100,
        height: 720,
        center: true,
        decorations: true,
        transparent: false,
        resizable: true,
      });
      viewer.once("tauri://destroyed", () => {
        animationRef.current?.updateExternalState({ viewingRemote: false });
        emitTo("main", "viewer-stop-request", session).catch(() => undefined);
      }).catch(() => undefined);
      await new Promise<void>((resolve, reject) => {
        const timeout = window.setTimeout(resolve, 3_000);
        viewer.once("tauri://created", () => {
          window.clearTimeout(timeout);
          resolve();
        }).catch(reject);
        viewer.once<unknown>("tauri://error", (event) => {
          window.clearTimeout(timeout);
          reject(new Error(`无法创建远程画面窗口：${realtimeError(event.payload)}`));
        }).catch(reject);
      });
    } catch (reason) {
      animationRef.current?.updateExternalState({ viewingRemote: false });
      if (attemptedSessionId) {
        await emitTo("viewer", "viewer-error", {
          sessionId: attemptedSessionId,
          message: realtimeError(reason),
        } satisfies ViewErrorPayload).catch(() => undefined);
      }
      await emitTo("main", "viewer-status", "failed").catch(() => undefined);
      console.error("无法打开远程画面", reason);
    }
  }, []);

  useEffect(() => {
    const publisher = new ScreenPublisher();
    publisherRef.current = publisher;
    const messaging = new RealtimeMessaging(async (result, signal) => {
      const payload = signal.core.payload as Partial<ViewSession> | Partial<ViewPeerSignal> | null;
      const peerSignal = payload as Partial<ViewPeerSignal> | null;
      const session = peerSignal?.session as ViewSession | undefined;
      if (result.event === "view-request") {
        if (!session || typeof session.sessionId !== "string" || !Number.isInteger(session.roomId)
          || typeof session.createdAtMs !== "number" || peerSignal?.kind !== "offer"
          || peerSignal.description?.type !== "offer" || typeof peerSignal.description.sdp !== "string") return;
        const sessionId = session.sessionId;
        const reportError = async (reason: unknown) => {
          const message = realtimeError(reason);
          console.error("无法启动桌面分享", reason);
          const errorSignal = await invoke<SignedSignal>("make_realtime_signal", {
            messageType: "viewError",
            payload: { sessionId, message } satisfies ViewErrorPayload,
          });
          await messaging.send(errorSignal);
        };
        const sendPeerSignal = async (next: ViewPeerSignal) => {
          const outgoing = await invoke<SignedSignal>("make_realtime_signal", {
            messageType: next.kind === "answer" ? "viewAnswer" : "viewIce",
            payload: next,
          });
          await messaging.send(outgoing);
        };
        await publisher.start(session, peerSignal.description, sendPeerSignal, reportError).catch(reportError);
      } else if (result.event === "view-answer" && session?.sessionId) {
        await emitTo("viewer", "viewer-peer-signal", peerSignal as ViewPeerSignal).catch(() => undefined);
      } else if (result.event === "view-ice" && session?.sessionId) {
        if (publisher.matches(session.sessionId)) {
          await publisher.addIceCandidate(peerSignal as ViewPeerSignal).catch(() => undefined);
        } else {
          await emitTo("viewer", "viewer-peer-signal", peerSignal as ViewPeerSignal).catch(() => undefined);
        }
      } else if (result.event === "view-stop" && publisher.matches((payload as Partial<ViewSession>)?.sessionId)) {
        await publisher.stop();
      } else if (result.event === "view-error") {
        const error = signal.core.payload as Partial<ViewErrorPayload> | null;
        if (typeof error?.sessionId === "string" && typeof error.message === "string") {
          await emitTo("viewer", "viewer-error", {
            sessionId: error.sessionId,
            message: error.message.slice(0, 300),
          } satisfies ViewErrorPayload).catch(() => undefined);
        }
      } else if (result.event.startsWith("unbind-")) {
        await emitTo("settings", "binding-changed").catch(() => undefined);
        if (result.event === "unbind-approved" || result.event === "unbind-complete") {
          await publisher.stop();
          await messaging.close();
        }
      }
    });
    realtimeRef.current = messaging;

    const connectIfBound = async () => {
      const status = await invoke<BindingStatus>("binding_status").catch(() => null);
      if (status?.state === "bound" || status?.state === "revoking") {
        await messaging.connect().catch((reason) => console.warn("内置联网通道暂未连接", reason));
      } else {
        await publisher.stop();
        await messaging.close();
      }
    };
    connectIfBound();
    const refreshTimer = window.setInterval(connectIfBound, 6 * 60 * 60_000);
    const signalListener = listen<SignedSignal>("send-realtime-signal", (event) => {
      messaging.send(event.payload).catch((reason) => console.error("联网消息发送失败", reason));
    });
    const viewerPeerListener = listen<ViewPeerSignal>("viewer-peer-signal", async (event) => {
      const next = event.payload;
      if (!next?.session?.sessionId || (next.kind !== "offer" && next.kind !== "ice")) return;
      try {
        const outgoing = await invoke<SignedSignal>("make_realtime_signal", {
          messageType: next.kind === "offer" ? "viewRequest" : "viewIce",
          payload: next,
        });
        await messaging.send(outgoing);
      } catch (reason) {
        await emitTo("viewer", "viewer-error", {
          sessionId: next.session.sessionId,
          message: realtimeError(reason),
        } satisfies ViewErrorPayload).catch(() => undefined);
      }
    });
    const viewerStopListener = listen<ViewSession>("viewer-stop-request", async (event) => {
      const session = event.payload;
      if (!session || typeof session.sessionId !== "string") return;
      try {
        const signal = await invoke<SignedSignal>("make_realtime_signal", {
          messageType: "viewStop", payload: session,
        });
        await messaging.send(signal);
      } catch (reason) {
        console.error("结束查看通知发送失败", reason);
      }
    });
    const bindingListener = listen("binding-changed", connectIfBound);
    return () => {
      window.clearInterval(refreshTimer);
      signalListener.then((dispose) => dispose()).catch(() => undefined);
      viewerPeerListener.then((dispose) => dispose()).catch(() => undefined);
      viewerStopListener.then((dispose) => dispose()).catch(() => undefined);
      bindingListener.then((dispose) => dispose()).catch(() => undefined);
      publisher.stop().catch(() => undefined);
      messaging.close().catch(() => undefined);
      publisherRef.current = null;
      realtimeRef.current = null;
    };
  }, []);

  useEffect(() => {
    const unlisten = listen("unbind-request-received", () => openSettings());
    return () => { unlisten.then((dispose) => dispose()).catch(() => undefined); };
  }, [openSettings]);

  useEffect(() => {
    const unlisten = listen<"viewer" | "settings">("pet-menu-action", (event) => {
      if (event.payload === "viewer") openViewer();
      else openSettings();
    });
    return () => { unlisten.then((dispose) => dispose()).catch(() => undefined); };
  }, [openSettings, openViewer]);

  useEffect(() => {
    const unlisten = listen<"connected" | "failed">("viewer-status", (event) => {
      animationRef.current?.updateExternalState({ viewingRemote: event.payload === "connected" });
    });
    return () => { unlisten.then((dispose) => dispose()).catch(() => undefined); };
  }, []);

  const openPetMenu = async () => {
    const position = await positionBesidePet(MENU_WIDTH, MENU_HEIGHT, 6);
    try {
      const existing = await WebviewWindow.getByLabel("pet-menu");
      if (existing) await existing.close();
      new WebviewWindow("pet-menu", {
        url: "/?mode=menu", title: "桌宠菜单", width: MENU_WIDTH, height: MENU_HEIGHT,
        x: position?.x, y: position?.y, decorations: false, transparent: true,
        backgroundColor: [0, 0, 0, 0], alwaysOnTop: true, skipTaskbar: true,
        shadow: false, resizable: false,
      });
    } catch {
      await openSettings();
    }
  };

  return (
    <main className="pet-shell" data-size={petSize} onContextMenu={(event) => {
      event.preventDefault();
      openPetMenu();
    }}>
      <button className="pet-hitbox" aria-label={`${profile.petName}，当前动作 ${animation.action}`}
        onKeyDown={(event) => {
          if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
          event.preventDefault();
          openPetMenu();
        }}
        onPointerDown={(event) => {
          if (event.button !== 0) return;
          const button = event.currentTarget as HTMLButtonElement;
          button.dataset.pointerX = String(event.clientX);
          button.dataset.pointerY = String(event.clientY);
          button.dataset.dragging = "false";
          event.currentTarget.setPointerCapture(event.pointerId);
        }}
        onPointerMove={(event) => {
          const button = event.currentTarget as HTMLButtonElement;
          if (!button.dataset.pointerX || button.dataset.dragging === "true") return;
          const distance = Math.hypot(
            event.clientX - Number(button.dataset.pointerX),
            event.clientY - Number(button.dataset.pointerY),
          );
          if (distance < 5) return;
          button.dataset.dragging = "true";
          animationRef.current?.dispatch({ type: "drag_start" });
          const finishDrag = () => {
            if (button.dataset.dragging !== "true") return;
            delete button.dataset.pointerX;
            delete button.dataset.pointerY;
            delete button.dataset.dragging;
            animationRef.current?.dispatch({ type: "drag_end" });
          };
          // WebView2 gives pointer ownership to the native move loop and often
          // never emits pointerup. On Windows, watch the physical left button;
          // startDragging() itself can resolve before the mouse is released.
          if (profile.platform === "windows") {
            invoke("wait_for_primary_mouse_release")
              .then(finishDrag)
              .catch(finishDrag);
          }
          getCurrentWindow().startDragging().catch(finishDrag);
        }}
        onPointerUp={(event) => {
          const button = event.currentTarget as HTMLButtonElement;
          if (!button.dataset.pointerX && button.dataset.dragging !== "true") return;
          const dragged = button.dataset.dragging === "true";
          delete button.dataset.pointerX;
          delete button.dataset.pointerY;
          delete button.dataset.dragging;
          if (dragged) {
            animationRef.current?.dispatch({ type: "drag_end" });
          } else {
            animationRef.current?.dispatch({ type: "click" });
          }
        }}
        onPointerCancel={(event) => {
          const button = event.currentTarget as HTMLButtonElement;
          if (!button.dataset.pointerX && button.dataset.dragging !== "true") return;
          const dragged = button.dataset.dragging === "true";
          delete button.dataset.pointerX;
          delete button.dataset.pointerY;
          delete button.dataset.dragging;
          if (dragged) animationRef.current?.dispatch({ type: "drag_end" });
        }}>
        {animation.assetUrl && <img key={animation.assetKey}
          className={`pet-image${animation.mirrored ? " mirrored" : ""}`}
          src={animation.assetUrl} alt={profile.petName} draggable={false} />}
      </button>
      {isSharing && <div className="sharing-badge">桌面正在分享</div>}
    </main>
  );
}

function PetGate() {
  const [usable, setUsable] = useState(false);

  const refresh = useCallback(async () => {
    const main = getCurrentWindow();
    try {
      const status = await invoke<BindingStatus>("binding_status");
      const allowed = status.state === "bound" || status.state === "revoking";
      setUsable(allowed);
      if (allowed) {
        const size = loadPetSize();
        await main.setSize(new LogicalSize(size, size));
        await main.show();
      } else {
        await main.hide();
        await openBindingWindow();
      }
    } catch {
      setUsable(false);
      await main.hide().catch(() => undefined);
      await openBindingWindow();
    }
  }, []);

  useEffect(() => {
    refresh();
    const bindingListener = listen("binding-changed", refresh);
    const activateListener = listen("activate-app", refresh);
    return () => {
      bindingListener.then((dispose) => dispose()).catch(() => undefined);
      activateListener.then((dispose) => dispose()).catch(() => undefined);
    };
  }, [refresh]);

  return usable ? <Pet /> : null;
}

export default function App() {
  const requestedMode = new URLSearchParams(window.location.search).get("mode");
  const mode = requestedMode ?? (getCurrentWindow().label === "viewer" ? "viewer" : null);
  if (mode === "viewer") return <Viewer />;
  if (mode === "binding") return <BindingSetup />;
  if (mode === "settings") return <Settings />;
  if (mode === "menu") return <PetMenu />;
  return <PetGate />;
}
