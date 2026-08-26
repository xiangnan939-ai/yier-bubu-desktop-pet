export type PetRole = "yier" | "bubu";
export { getGifDuration } from "./gifDuration";

export type PetAsset = {
  url: string;
  sourcePath: string;
};

export type HotPetAsset = PetAsset & { action: string };

const gifModules = import.meta.glob<string>(
  "../assets/characters/**/*.gif",
  { eager: true, query: "?url", import: "default" },
);

const roleFolder: Record<PetRole, string> = { yier: "/一二/", bubu: "/布布/" };

function actionFromPath(path: string) {
  const filename = path.split("/").pop()?.replace(/\.gif$/i, "") ?? "idle";
  const raw = filename.split("（")[0].replace(/\d+$/, "");
  return raw === "idel" ? "idle" : raw;
}

export function buildPetLibrary(role: PetRole, hotAssets: HotPetAsset[] = []) {
  const actions = new Map<string, PetAsset[]>();
  for (const [path, url] of Object.entries(gifModules)) {
    if (!path.includes(roleFolder[role])) continue;
    const action = actionFromPath(path);
    actions.set(action, [...(actions.get(action) ?? []), { url, sourcePath: path }]);
  }
  const hotActions = new Map<string, PetAsset[]>();
  for (const { action, url, sourcePath } of hotAssets) {
    hotActions.set(action, [...(hotActions.get(action) ?? []), { url, sourcePath }]);
  }
  // 热更新包只覆盖它实际提供的动作；缺少的动作继续使用安装包内素材。
  for (const [action, assets] of hotActions) actions.set(action, assets);
  return actions;
}

export function chooseAction(library: Map<string, PetAsset[]>, requested: string) {
  const candidates = library.get(requested) ?? library.get("idle") ?? [];
  return candidates[Math.floor(Math.random() * candidates.length)] ?? null;
}
