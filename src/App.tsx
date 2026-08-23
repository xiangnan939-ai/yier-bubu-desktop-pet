import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { relaunch } from "@tauri-apps/plugin-process";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  LogicalPosition,
  LogicalSize,
  PhysicalPosition,
  currentMonitor,
  getCurrentWindow,
} from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  DEFAULT_AMBIENT_WEIGHTS,
  chooseAmbientAction,
  type AmbientWeights,
} from "./actionPolicy";
import {
  buildPetLibrary,
  chooseAction,
  getGifDuration,
  type HotPetAsset,
  type PetRole,
} from "./petAssets";
import "./App.css";

type AppProfile = {
  role: PetRole;
  petName: string;
  partnerName: string;
  remoteMenuLabel: string;
  platform: string;
};

type DeviceStatus = { batteryPercentage: number | null; charging: boolean; hot: boolean };
type BindingStatus = {
  state: "unbound" | "bound" | "revoking" | "revoked";
  petName: string;
  partnerName: string;
  bindingId: string | null;
  partnerHost: string | null;
  partnerMachineCode: string | null;
  createdAtMs: number | null;
  incomingUnbind: boolean;
  outgoingUnbind: boolean;
  approvalPending: boolean;
  requestedByName: string | null;
};
type PairingResult = { state: string; message: string };
type ScreenAuth = {
  messageType: string;
  bindingId: string;
  role: string;
  publicKey: string;
  nonce: string;
  timestampMs: number;
  signature: string;
};
type ScreenServerAuth = {
  messageType: string;
  bindingId: string;
  clientNonce: string;
  serverNonce: string;
  timestampMs: number;
  signature: string;
};
type ScreenConnectionInfo = { partnerHost: string; auth: ScreenAuth };
type UpdateConfiguration = {
  currentVersion: string;
  appUpdateEnabled: boolean;
  assetUpdateEnabled: boolean;
};
type AppUpdateCheck = {
  available: boolean;
  currentVersion: string;
  version: string | null;
  notes: string | null;
};
type AssetUpdateResult = {
  status: "unconfigured" | "upToDate" | "requiresAppUpdate" | "updated";
  version: string | null;
  message: string;
};
type UpdateProgress = {
  updateType: "app" | "assets";
  phase: "downloading" | "installing" | "complete";
  downloadedBytes: number;
  totalBytes: number | null;
};
type InstalledAssetPack = {
  version: string | null;
  assets: Array<{ action: string; path: string; sourcePath: string }>;
  rules: unknown;
};

type ActionRules = {
  sleepAfterSeconds: number;
  ambientWeights: AmbientWeights;
  drinkMinMinutes: number;
  drinkMaxMinutes: number;
  eatMinMinutes: number;
  eatMaxMinutes: number;
  workMinMinutes: number;
  workMaxMinutes: number;
};

const DEFAULT_ACTION_RULES: ActionRules = {
  sleepAfterSeconds: 10 * 60,
  ambientWeights: DEFAULT_AMBIENT_WEIGHTS,
  drinkMinMinutes: 45,
  drinkMaxMinutes: 75,
  eatMinMinutes: 120,
  eatMaxMinutes: 180,
  workMinMinutes: 3,
  workMaxMinutes: 5,
};

const TAILSCALE_DOWNLOAD_URL = "https://tailscale.com/download";

function formatBytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

function safeNumber(value: unknown, fallback: number, min: number, max: number) {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(max, Math.max(min, value))
    : fallback;
}

function normalizeActionRules(value: unknown): ActionRules {
  const rules = value && typeof value === "object" ? value as Record<string, unknown> : {};
  const weights = rules.ambientWeights && typeof rules.ambientWeights === "object"
    ? rules.ambientWeights as Record<string, unknown> : {};
  const next = {
    sleepAfterSeconds: safeNumber(rules.sleepAfterSeconds, DEFAULT_ACTION_RULES.sleepAfterSeconds, 60, 86_400),
    ambientWeights: {
      walk: safeNumber(weights.walk, DEFAULT_AMBIENT_WEIGHTS.walk, 0, 100),
      look: safeNumber(weights.look, DEFAULT_AMBIENT_WEIGHTS.look, 0, 100),
      sit: safeNumber(weights.sit, DEFAULT_AMBIENT_WEIGHTS.sit, 0, 100),
      idle: safeNumber(weights.idle, DEFAULT_AMBIENT_WEIGHTS.idle, 0, 100),
    },
    drinkMinMinutes: safeNumber(rules.drinkMinMinutes, DEFAULT_ACTION_RULES.drinkMinMinutes, 5, 720),
    drinkMaxMinutes: safeNumber(rules.drinkMaxMinutes, DEFAULT_ACTION_RULES.drinkMaxMinutes, 5, 720),
    eatMinMinutes: safeNumber(rules.eatMinMinutes, DEFAULT_ACTION_RULES.eatMinMinutes, 15, 1_440),
    eatMaxMinutes: safeNumber(rules.eatMaxMinutes, DEFAULT_ACTION_RULES.eatMaxMinutes, 15, 1_440),
    workMinMinutes: safeNumber(rules.workMinMinutes, DEFAULT_ACTION_RULES.workMinMinutes, 1, 120),
    workMaxMinutes: safeNumber(rules.workMaxMinutes, DEFAULT_ACTION_RULES.workMaxMinutes, 1, 120),
  };
  next.drinkMaxMinutes = Math.max(next.drinkMinMinutes, next.drinkMaxMinutes);
  next.eatMaxMinutes = Math.max(next.eatMinMinutes, next.eatMaxMinutes);
  next.workMaxMinutes = Math.max(next.workMinMinutes, next.workMaxMinutes);
  return next;
}

function hotAssetsFromPack(pack: InstalledAssetPack): HotPetAsset[] {
  return pack.assets.map((asset) => ({
    action: asset.action,
    sourcePath: asset.sourcePath,
    url: `${convertFileSrc(asset.path)}?pack=${encodeURIComponent(pack.version ?? "current")}`,
  }));
}

const DEFAULT_PET_SIZE = 160;
const MIN_PET_SIZE = 96;
const MAX_PET_SIZE = 320;
const MENU_WIDTH = 164;
const MENU_HEIGHT = 94;
const SETTINGS_WIDTH = 420;
const SETTINGS_HEIGHT = 700;
const BINDING_WIDTH = 420;
const BINDING_HEIGHT = 410;

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

function wait(milliseconds: number) {
  return new Promise<void>((resolve) => window.setTimeout(resolve, milliseconds));
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

async function waitUnlessCancelled(milliseconds: number, cancelled: () => boolean) {
  const end = Date.now() + milliseconds;
  while (!cancelled() && Date.now() < end) {
    await wait(Math.min(250, end - Date.now()));
  }
  return !cancelled();
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
  const [frame, setFrame] = useState("");
  const lastUrl = useRef("");

  useEffect(() => {
    let socket: WebSocket | null = null;
    let stopped = false;
    let authenticated = false;
    invoke<ScreenConnectionInfo>("screen_connection_info").then((connection) => {
      if (stopped) return;
      socket = new WebSocket(`ws://${connection.partnerHost}:39821/screen`);
      socket.binaryType = "blob";
      socket.onopen = () => {
        setStatus("正在验证双方设备身份…");
        socket?.send(JSON.stringify(connection.auth));
      };
      socket.onerror = () => {
        setStatus("连接失败，请确认对方在线且 Tailscale 已连接");
        emitTo("main", "viewer-status", "failed").catch(() => undefined);
      };
      socket.onclose = () => {
        if (!stopped) setStatus(authenticated ? "连接已断开" : "设备认证未完成");
      };
      socket.onmessage = async (event) => {
        if (typeof event.data === "string") {
          let serverAuth: ScreenServerAuth;
          try {
            serverAuth = JSON.parse(event.data) as ScreenServerAuth;
          } catch {
            setStatus(event.data);
            socket?.close();
            emitTo("main", "viewer-status", "failed").catch(() => undefined);
            return;
          }
          try {
            if (serverAuth.messageType !== "screenAuthOk") throw new Error("对方设备认证响应无效");
            const valid = await invoke<boolean>("verify_screen_server_auth", { auth: serverAuth });
            if (!valid) throw new Error("对方设备签名无效");
            authenticated = true;
            setStatus("已连接 · 双向设备认证 · 只读实时画面");
            emitTo("main", "viewer-status", "connected").catch(() => undefined);
          } catch (reason) {
            setStatus(String(reason instanceof Error ? reason.message : reason));
            socket?.close();
            emitTo("main", "viewer-status", "failed").catch(() => undefined);
          }
          return;
        }
        if (!authenticated || !(event.data instanceof Blob)) return;
        const url = URL.createObjectURL(event.data);
        if (lastUrl.current) URL.revokeObjectURL(lastUrl.current);
        lastUrl.current = url;
        setFrame(url);
      };
    }).catch((reason) => {
      setStatus(String(reason));
      emitTo("main", "viewer-status", "failed").catch(() => undefined);
    });
    return () => {
      stopped = true;
      socket?.close();
      if (lastUrl.current) URL.revokeObjectURL(lastUrl.current);
    };
  }, []);

  return (
    <main className="viewer-shell">
      <header>
        <div><strong>一二布布 · 对方桌面</strong><span>{status}</span></div>
        <button onClick={() => getCurrentWindow().close()}>结束查看</button>
      </header>
      <section className="viewer-stage">
        {frame ? <img src={frame} alt="对方实时桌面" /> : <div className="viewer-placeholder">{status}</div>}
      </section>
    </main>
  );
}

function BindingSetup() {
  const [profile, setProfile] = useState(fallbackProfile);
  const [status, setStatus] = useState<BindingStatus | null>(null);
  const [passphrase, setPassphrase] = useState("");
  const [message, setMessage] = useState("两台电脑输入完全相同的口令后，会自动找到彼此并完成绑定。");
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => invoke<BindingStatus>("binding_status").then(setStatus), []);
  useEffect(() => {
    invoke<AppProfile>("app_profile").then(setProfile).catch(() => undefined);
    refresh().catch((reason) => setMessage(String(reason)));
    const unlisten = listen("binding-changed", () => refresh().catch(() => undefined));
    return () => { unlisten.then((dispose) => dispose()).catch(() => undefined); };
  }, [refresh]);

  const pair = async () => {
    if (busy) return;
    if (passphrase.trim().length < 8) {
      setMessage("绑定口令至少需要 8 个字符；建议使用 12 个以上字符。");
      return;
    }
    setBusy(true);
    setMessage(profile.role === "bubu" ? "正在等待一二输入相同口令…" : "正在寻找等待绑定的布布电脑…");
    try {
      const result = await invoke<PairingResult>("pair_device", { passphrase });
      setPassphrase("");
      setMessage(result.message);
      await refresh();
      await emitTo("main", "binding-changed").catch(() => undefined);
      window.setTimeout(() => getCurrentWindow().close(), 900);
    } catch (reason) {
      setMessage(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const alreadyBound = status?.state === "bound" || status?.state === "revoking";
  return (
    <main className="binding-shell">
      <div className="binding-mark">{profile.petName}</div>
      <h1>{alreadyBound ? `${profile.petName}已经绑定` : `输入口令绑定我的${profile.petName}`}</h1>
      {alreadyBound ? (
        <>
          <p>当前绑定对象：{status?.partnerName}。绑定后只能由双方签名同意才能解除。</p>
          <button className="primary wide" onClick={() => getCurrentWindow().close()}>知道了</button>
        </>
      ) : (
        <>
          <p>请在 Mac 和 Windows 上输入同一串字符。口令只参与这一次绑定，不会保存。</p>
          <input autoFocus type="password" value={passphrase} disabled={busy}
            onChange={(event) => setPassphrase(event.target.value)}
            onKeyDown={(event) => { if (event.key === "Enter") pair(); }}
            placeholder="输入双方约定的绑定口令" />
          <button className="primary wide" disabled={busy} onClick={pair}>
            {busy ? "正在安全绑定…" : `绑定我的${profile.petName}`}
          </button>
          <p className="binding-message">{message}</p>
          <button className="tailscale-help" onClick={() => openUrl(TAILSCALE_DOWNLOAD_URL)}>
            安装或打开 Tailscale
          </button>
          <p className="binding-hint">两台电脑都需安装、登录同一个 Tailscale 账号并保持已连接。</p>
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
  const [profile, setProfile] = useState(fallbackProfile);
  const [updateConfig, setUpdateConfig] = useState<UpdateConfiguration | null>(null);
  const [assetVersion, setAssetVersion] = useState<string | null>(null);
  const [appUpdate, setAppUpdate] = useState<AppUpdateCheck | null>(null);
  const [updateMessage, setUpdateMessage] = useState("正在读取更新状态…");
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [installingApp, setInstallingApp] = useState(false);
  const [updateProgress, setUpdateProgress] = useState<UpdateProgress | null>(null);

  useEffect(() => {
    invoke<AppProfile>("app_profile").then((value) => {
      setProfile(value);
      return invoke<InstalledAssetPack>("installed_asset_pack", { role: value.role });
    }).then((pack) => setAssetVersion(pack.version)).catch(() => undefined);
    invoke<UpdateConfiguration>("update_configuration").then((value) => {
      setUpdateConfig(value);
      if (!value.appUpdateEnabled && !value.assetUpdateEnabled) {
        setUpdateMessage("发布地址尚未配置；当前安装包仍可正常离线使用。");
      } else {
        setUpdateMessage("启动后每 6 小时自动检查，也可以立即检查。");
      }
    }).catch((reason) => setUpdateMessage(String(reason)));
    invoke<BindingStatus>("binding_status").then((value) => {
      setBinding(value);
      setBindingMessage(value.state === "bound" ? `已与${value.partnerName}安全绑定` : "尚未完成双机绑定");
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
    if (!updateConfig || checkingUpdates) return;
    setCheckingUpdates(true);
    setUpdateProgress(null);
    setError("");
    const messages: string[] = [];
    try {
      if (updateConfig.assetUpdateEnabled) {
        const result = await invoke<AssetUpdateResult>("check_and_install_asset_update");
        messages.push(result.message);
        if (result.version) setAssetVersion(result.version);
        if (result.status === "updated") {
          await emitTo("main", "asset-pack-updated").catch(() => undefined);
        }
      }
      if (updateConfig.appUpdateEnabled) {
        const result = await invoke<AppUpdateCheck>("check_app_update");
        setAppUpdate(result);
        messages.push(result.available ? `发现程序新版本 ${result.version}` : "程序已是最新版");
      }
      setUpdateMessage(messages.length ? messages.join("；") : "发布地址尚未配置");
    } catch (reason) {
      setUpdateMessage(`检查更新失败：${String(reason)}`);
      setUpdateProgress(null);
    } finally {
      setCheckingUpdates(false);
    }
  };

  const installProgramUpdate = async () => {
    if (installingApp) return;
    setInstallingApp(true);
    setUpdateProgress(null);
    setUpdateMessage("正在下载并验证程序更新，请不要关闭软件…");
    try {
      await invoke("install_app_update");
      setUpdateMessage("更新安装完成，正在重新启动…");
      await relaunch();
    } catch (reason) {
      setUpdateMessage(`程序更新失败：${String(reason)}`);
      setUpdateProgress(null);
      setInstallingApp(false);
    }
  };

  const save = async () => {
    const size = clampPetSize(petSize);
    localStorage.setItem("petSize", String(size));
    await resizeMainPet(size).catch(() => undefined);
    await emitTo("main", "settings-updated", { petSize: size }).catch(() => undefined);
    await getCurrentWindow().close();
  };

  const sendUnbindRequest = async () => {
    if (bindingBusy) return;
    setBindingBusy(true);
    setError("");
    try {
      setBindingMessage(await invoke<string>("request_unbind"));
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
      setBindingMessage(await invoke<string>("respond_unbind", { approve }));
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
          value={petSize} onChange={(event) => setPetSize(Number(event.target.value))} />
        <div className="range-labels"><span>较小</span><span>默认 160px</span><span>较大</span></div>
      </section>

      <section className="settings-section update-section">
        <div className="setting-heading"><h2>联网更新</h2><strong>程序 {updateConfig?.currentVersion ?? "…"}</strong></div>
        <p>{updateMessage}</p>
        {updateProgress && <div className="update-progress" aria-live="polite">
          <div className="update-progress-label">
            <span>{updateProgress.updateType === "app" ? "程序更新" : "动作素材"} · {
              updateProgress.phase === "downloading" ? "正在下载"
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
        <div className="update-meta">
          <span>{profile.petName}素材：{assetVersion ?? "安装包内置版"}</span>
          <span>校验：HTTPS + 数字签名</span>
        </div>
        {appUpdate?.notes && <p className="update-notes">{appUpdate.notes}</p>}
        <div className="update-actions">
          <button disabled={checkingUpdates || installingApp || !updateConfig
            || (!updateConfig.appUpdateEnabled && !updateConfig.assetUpdateEnabled)} onClick={checkUpdates}>
            {checkingUpdates ? "正在检查…" : "立即检查更新"}
          </button>
          {appUpdate?.available && <button className="primary" disabled={installingApp}
            onClick={installProgramUpdate}>{installingApp ? "正在更新…" : "更新并重启"}</button>}
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
        <p>{bindingMessage}</p>
        {(binding?.state === "unbound" || binding?.state === "revoked")
          && <button onClick={() => openUrl(TAILSCALE_DOWNLOAD_URL)}>安装或打开 Tailscale</button>}
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
      <footer><button onClick={() => getCurrentWindow().close()}>取消</button><button className="primary" onClick={save}>保存</button></footer>
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
        <button className="remote-action" onClick={() => choose("viewer")}>看看TA在干嘛</button>
        <button onClick={() => choose("settings")}>设置</button>
      </nav>
    </main>
  );
}

function Pet() {
  const [profile, setProfile] = useState(fallbackProfile);
  const [hotPack, setHotPack] = useState<{ version: string | null; assets: HotPetAsset[] }>({
    version: null,
    assets: [],
  });
  const [actionRules, setActionRules] = useState(DEFAULT_ACTION_RULES);
  const library = useMemo(
    () => buildPetLibrary(profile.role, hotPack.assets),
    [hotPack.assets, profile.role],
  );
  const firstAsset = useMemo(() => chooseAction(library, "idle"), [library]);
  const [action, setAction] = useState("idle");
  const [asset, setAsset] = useState(firstAsset?.url ?? "");
  const [assetKey, setAssetKey] = useState(0);
  const [isSharing, setIsSharing] = useState(false);
  const [musicPlaying, setMusicPlaying] = useState(false);
  const [systemSleeping, setSystemSleeping] = useState(false);
  const [deviceStatus, setDeviceStatus] = useState<DeviceStatus>({
    batteryPercentage: null,
    charging: false,
    hot: false,
  });
  const [petSize, setPetSize] = useState(loadPetSize);
  const [mirrored, setMirrored] = useState(false);
  const actionRef = useRef("idle");
  const interactionVersion = useRef(0);
  const interactionActive = useRef(false);
  const movementVersion = useRef(0);
  const musicPlayingRef = useRef(false);
  const sharingRef = useRef(false);
  const sleepingRef = useRef(false);
  const lowBatteryRef = useRef(false);
  const chargingRef = useRef(false);
  const hotRef = useRef(false);
  const actionEndsAt = useRef(0);
  const workActiveRef = useRef(false);
  const workDueAt = useRef(Date.now() + 60_000);
  const incomingSharingRef = useRef(false);
  const clickTimesRef = useRef<number[]>([]);
  const drinkDueAt = useRef(Date.now() + DEFAULT_ACTION_RULES.drinkMinMinutes * 60_000);
  const eatDueAt = useRef(Date.now() + DEFAULT_ACTION_RULES.eatMinMinutes * 60_000);

  const showAction = useCallback(async (requested: string, restart = true) => {
    const semanticFallback: Record<string, string> = { drag: "click", drop: "idle", shared: "watching" };
    const fallback = semanticFallback[requested] ?? "idle";
    const resolved = library.has(requested) ? requested : library.has(fallback) ? fallback : "idle";
    if (!restart && actionRef.current === resolved) return 0;
    const selected = chooseAction(library, resolved);
    if (!selected) return 0;

    actionRef.current = resolved;
    setAction(resolved);
    setAsset(selected.url);
    setAssetKey((value) => value + 1);
    const duration = Math.max(600, await getGifDuration(selected.url));
    actionEndsAt.current = Date.now() + duration;
    return duration;
  }, [library]);

  const loadInstalledPack = useCallback(async (afterCurrentAction = false) => {
    const pack = await invoke<InstalledAssetPack>("installed_asset_pack", { role: profile.role });
    if (afterCurrentAction) {
      const remaining = Math.max(0, actionEndsAt.current - Date.now());
      if (remaining) await wait(remaining);
    }
    setHotPack({ version: pack.version, assets: hotAssetsFromPack(pack) });
    setActionRules(normalizeActionRules(pack.rules));
  }, [profile.role]);

  const restorePriorityAction = useCallback(async () => {
    if (interactionActive.current) return;
    if (musicPlayingRef.current) await showAction("dance", false);
    else if (sharingRef.current) await showAction("shared", false);
    else if (lowBatteryRef.current) await showAction("low_battery", false);
    else if (chargingRef.current) await showAction("charging", false);
    else if (hotRef.current) await showAction("hot", false);
    else if (sleepingRef.current) await showAction("sleep", false);
    else await showAction("idle", false);
  }, [showAction]);

  const runTransient = useCallback(async (next: string, wakesSystem: boolean) => {
    const version = ++interactionVersion.current;
    movementVersion.current += 1;
    interactionActive.current = true;
    if (wakesSystem && next !== "wake") {
      sleepingRef.current = false;
      setSystemSleeping(false);
    }
    const duration = await showAction(next);
    await wait(duration || 900);
    if (interactionVersion.current !== version) return;
    interactionActive.current = false;
    await restorePriorityAction();
  }, [restorePriorityAction, showAction]);

  const runInteraction = useCallback((next: string) => runTransient(next, true), [runTransient]);
  const runSystemReaction = useCallback((next: string) => runTransient(next, false), [runTransient]);

  const walkSlowly = useCallback(async (cancelled: () => boolean) => {
    const version = ++movementVersion.current;
    const windowHandle = getCurrentWindow();
    try {
      const monitor = await currentMonitor();
      if (!monitor || cancelled()) return;
      const [start, windowSize] = await Promise.all([
        windowHandle.outerPosition(),
        windowHandle.outerSize(),
      ]);
      const work = monitor.workArea;
      const minX = work.position.x;
      const maxX = work.position.x + work.size.width - windowSize.width;
      const minY = work.position.y;
      const maxY = work.position.y + work.size.height - windowSize.height;
      const desiredDistance = (90 + Math.random() * 130) * monitor.scaleFactor;
      const canMoveRight = maxX - start.x > desiredDistance * 0.7;
      const canMoveLeft = start.x - minX > desiredDistance * 0.7;
      const direction = canMoveRight && canMoveLeft ? (Math.random() < 0.5 ? -1 : 1) : canMoveRight ? 1 : -1;
      const targetX = Math.min(maxX, Math.max(minX, start.x + direction * desiredDistance));
      const targetY = Math.min(maxY, Math.max(minY, start.y + (Math.random() - 0.5) * 36 * monitor.scaleFactor));
      const distance = Math.hypot(targetX - start.x, targetY - start.y);
      if (distance < 8) return;

      const sourceFacesLeft = profile.role === "bubu";
      const shouldFaceLeft = direction < 0;
      setMirrored(sourceFacesLeft !== shouldFaceLeft);
      await showAction("walk");
      const duration = Math.min(7_000, Math.max(2_800, distance / (36 * monitor.scaleFactor) * 1_000));
      const startedAt = performance.now();

      while (!cancelled() && movementVersion.current === version && !interactionActive.current
        && !musicPlayingRef.current && !sharingRef.current) {
        const progress = Math.min(1, (performance.now() - startedAt) / duration);
        await windowHandle.setPosition(new PhysicalPosition(
          Math.round(start.x + (targetX - start.x) * progress),
          Math.round(start.y + (targetY - start.y) * progress),
        ));
        if (progress >= 1) break;
        await wait(33);
      }
    } catch {
      // 浏览器预览没有原生窗口；实际 Tauri 窗口会执行平滑位移。
    }
  }, [profile.role, showAction]);

  useEffect(() => {
    invoke<AppProfile>("app_profile").then(setProfile).catch(() => undefined);
    localStorage.removeItem("pairing");
    invoke<BindingStatus>("binding_status").then((status) => {
      if (status.state === "unbound" || status.state === "revoked") openBindingWindow();
    }).catch(() => undefined);
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
    loadInstalledPack().catch(() => undefined);
  }, [loadInstalledPack]);

  useEffect(() => {
    const unlisten = listen("asset-pack-updated", () => {
      loadInstalledPack(true).catch(() => undefined);
    });
    return () => { unlisten.then((dispose) => dispose()).catch(() => undefined); };
  }, [loadInstalledPack]);

  useEffect(() => {
    let stopped = false;
    let interval = 0;
    const check = async () => {
      const config = await invoke<UpdateConfiguration>("update_configuration").catch(() => null);
      if (!config || stopped) return;
      if (config.assetUpdateEnabled) {
        const result = await invoke<AssetUpdateResult>("check_and_install_asset_update").catch(() => null);
        if (result?.status === "updated" && !stopped) await loadInstalledPack(true).catch(() => undefined);
      }
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
  }, [loadInstalledPack]);

  useEffect(() => {
    setMirrored(false);
    showAction("idle");
  }, [library, profile.role, showAction]);

  useEffect(() => {
    let trueCount = 0;
    let falseCount = 0;
    const check = async () => {
      const playing = await invoke<boolean>("system_audio_playing").catch(() => false);
      if (playing) {
        trueCount += 1;
        falseCount = 0;
        if (trueCount >= 2) setMusicPlaying(true);
      } else {
        falseCount += 1;
        trueCount = 0;
        if (falseCount >= 3) setMusicPlaying(false);
      }
    };
    check();
    const timer = window.setInterval(check, 900);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    const check = () => invoke<boolean>("screen_share_active").then((active) => {
      if (active && !incomingSharingRef.current && !musicPlayingRef.current) {
        runSystemReaction("message");
      }
      incomingSharingRef.current = active;
      setIsSharing(active);
    }).catch(() => undefined);
    check();
    const timer = window.setInterval(check, 1500);
    return () => window.clearInterval(timer);
  }, [runSystemReaction]);

  useEffect(() => {
    let activityScore = 0;
    const check = () => invoke<number>("system_idle_seconds")
      .then((seconds) => {
        setSystemSleeping(seconds >= actionRules.sleepAfterSeconds);
        activityScore = seconds <= 4 ? Math.min(12, activityScore + 1) : Math.max(0, activityScore - 1);
        workActiveRef.current = activityScore >= 6;
      })
      .catch(() => undefined);
    check();
    const timer = window.setInterval(check, 3_000);
    return () => window.clearInterval(timer);
  }, [actionRules.sleepAfterSeconds]);

  useEffect(() => {
    let hotSamples = 0;
    let coolSamples = 0;
    let confirmedHot = false;
    const check = () => invoke<DeviceStatus>("device_status").then((status) => {
      if (status.hot) {
        hotSamples += 1;
        coolSamples = 0;
        if (hotSamples >= 6) confirmedHot = true;
      } else {
        coolSamples += 1;
        hotSamples = 0;
        if (coolSamples >= 3) confirmedHot = false;
      }
      const next = { ...status, hot: confirmedHot };
      setDeviceStatus((previous) => previous.batteryPercentage === next.batteryPercentage
        && previous.charging === next.charging && previous.hot === next.hot ? previous : next);
    }).catch(() => undefined);
    check();
    const timer = window.setInterval(check, 5_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    const wasSleeping = sleepingRef.current;
    const wasVisiblySleeping = actionRef.current === "sleep";
    musicPlayingRef.current = musicPlaying;
    sharingRef.current = isSharing;
    sleepingRef.current = systemSleeping;
    lowBatteryRef.current = deviceStatus.batteryPercentage !== null
      && deviceStatus.batteryPercentage <= 20 && !deviceStatus.charging;
    chargingRef.current = deviceStatus.batteryPercentage !== null
      && deviceStatus.batteryPercentage < 50 && deviceStatus.charging;
    hotRef.current = deviceStatus.hot;
    if (musicPlaying || isSharing || systemSleeping || lowBatteryRef.current
      || chargingRef.current || hotRef.current) movementVersion.current += 1;
    if (!musicPlaying && !isSharing && wasSleeping && !systemSleeping
      && wasVisiblySleeping && !interactionActive.current) {
      runInteraction("wake");
      return;
    }
    restorePriorityAction();
  }, [deviceStatus, isSharing, musicPlaying, restorePriorityAction, runInteraction, systemSleeping]);

  useEffect(() => {
    let stopped = false;
    const cancelled = () => stopped;

    const run = async () => {
      if (!await waitUnlessCancelled(2_800, cancelled)) return;
      while (!stopped) {
        if (interactionActive.current || musicPlayingRef.current || sharingRef.current || sleepingRef.current
          || lowBatteryRef.current || chargingRef.current || hotRef.current) {
          await waitUnlessCancelled(350, cancelled);
          continue;
        }

        const now = Date.now();
        const next = chooseAmbientAction({
          musicPlaying: false,
          screenSharing: false,
          lowBattery: false,
          charging: false,
          hot: false,
          sleeping: false,
          drinkDue: now >= drinkDueAt.current,
          eatDue: now >= eatDueAt.current,
          workDue: workActiveRef.current && now >= workDueAt.current,
        }, Math.random(), actionRules.ambientWeights);
        if (next === "drink") drinkDueAt.current = now + (actionRules.drinkMinMinutes
          + Math.random() * (actionRules.drinkMaxMinutes - actionRules.drinkMinMinutes)) * 60_000;
        if (next === "eat") eatDueAt.current = now + (actionRules.eatMinMinutes
          + Math.random() * (actionRules.eatMaxMinutes - actionRules.eatMinMinutes)) * 60_000;
        if (next === "work") workDueAt.current = now + (actionRules.workMinMinutes
          + Math.random() * (actionRules.workMaxMinutes - actionRules.workMinMinutes)) * 60_000;

        if (next === "walk") {
          await walkSlowly(cancelled);
        } else {
          const duration = await showAction(next);
          await waitUnlessCancelled(duration || 1_200, cancelled);
        }

        await restorePriorityAction();
        await waitUnlessCancelled(3_500 + Math.random() * 4_500, cancelled);
      }
    };

    run();
    return () => {
      stopped = true;
      movementVersion.current += 1;
    };
  }, [actionRules, library, restorePriorityAction, showAction, walkSlowly]);

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
    runInteraction("watching");
    const binding = await invoke<BindingStatus>("binding_status").catch(() => null);
    if (!binding || binding.state !== "bound") {
      await openBindingWindow();
      return;
    }
    try {
      const existing = await WebviewWindow.getByLabel("viewer");
      if (existing) await existing.setFocus();
      else new WebviewWindow("viewer", {
        url: "/?mode=viewer", title: "看看TA在干嘛",
        width: 1100, height: 720, center: true, decorations: true, transparent: false,
      });
    } catch {
      window.open("/?mode=viewer", "_blank");
    }
  }, [runInteraction]);

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
      runInteraction(event.payload === "connected" ? "happy" : "sad");
    });
    return () => { unlisten.then((dispose) => dispose()).catch(() => undefined); };
  }, [runInteraction]);

  const openPetMenu = async () => {
    const position = await positionBesidePet(MENU_WIDTH, MENU_HEIGHT, 6);
    try {
      const existing = await WebviewWindow.getByLabel("pet-menu");
      if (existing) await existing.close();
      new WebviewWindow("pet-menu", {
        url: "/?mode=menu", title: "桌宠菜单", width: MENU_WIDTH, height: MENU_HEIGHT,
        x: position?.x, y: position?.y, decorations: false, transparent: true,
        alwaysOnTop: true, skipTaskbar: true, shadow: false, resizable: false,
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
      <button className="pet-hitbox" aria-label={`${profile.petName}，当前动作 ${action}`}
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
          runInteraction("drag");
          getCurrentWindow().startDragging().catch(() => undefined);
        }}
        onPointerUp={(event) => {
          const button = event.currentTarget as HTMLButtonElement;
          const dragged = button.dataset.dragging === "true";
          delete button.dataset.pointerX;
          delete button.dataset.pointerY;
          delete button.dataset.dragging;
          if (dragged) {
            runInteraction("drop");
          } else {
            const now = Date.now();
            clickTimesRef.current = [...clickTimesRef.current.filter((time) => now - time <= 4_000), now];
            const clicks = clickTimesRef.current;
            if (clicks.length >= 5) {
              clickTimesRef.current = [];
              runInteraction("angry");
            } else if (clicks.length >= 2 && now - clicks[clicks.length - 2] <= 350) {
              runInteraction("happy");
            } else {
              runInteraction("click");
            }
          }
        }}
        onPointerCancel={(event) => {
          const button = event.currentTarget as HTMLButtonElement;
          delete button.dataset.pointerX;
          delete button.dataset.pointerY;
          delete button.dataset.dragging;
        }}>
        {asset && <img key={assetKey} className={`pet-image${mirrored ? " mirrored" : ""}`}
          src={asset} alt={profile.petName} draggable={false} />}
      </button>
      {isSharing && <div className="sharing-badge">桌面正在分享</div>}
    </main>
  );
}

export default function App() {
  const mode = new URLSearchParams(window.location.search).get("mode");
  if (mode === "viewer") return <Viewer />;
  if (mode === "binding") return <BindingSetup />;
  if (mode === "settings") return <Settings />;
  if (mode === "menu") return <PetMenu />;
  return <Pet />;
}
