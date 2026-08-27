export type PetRole = "yier" | "bubu";
export { getGifDuration } from "./gifDuration";

export type PetAsset = {
  url: string;
  sourcePath: string;
};

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

export function buildPetLibrary(role: PetRole) {
  const actions = new Map<string, PetAsset[]>();
  for (const [path, url] of Object.entries(gifModules)) {
    if (!path.includes(roleFolder[role])) continue;
    const action = actionFromPath(path);
    actions.set(action, [...(actions.get(action) ?? []), { url, sourcePath: path }]);
  }
  return actions;
}

export function chooseAction(library: Map<string, PetAsset[]>, requested: string) {
  const candidates = library.get(requested) ?? library.get("idle") ?? [];
  return candidates[Math.floor(Math.random() * candidates.length)] ?? null;
}
