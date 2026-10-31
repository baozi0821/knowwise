// 应用主题(浅色 / 深色 / 跟随系统)。可重建的界面偏好,存
// localStorage,不进入业务数据库;启动时在 React 挂载前应用,避免闪烁。
//
// 主题实现:html[data-theme="dark"] 覆盖 styles.css 的设计变量,
// 少量硬编码浅色面在 theme.css 里做定点覆盖(见该文件说明)。

export type ThemeMode = "light" | "dark" | "system";

/** 强调色预设:同一套语义变量,浅色/深色由 theme.css 分别给值。 */
export type AccentPreset =
  | "indigo"
  | "violet"
  | "teal"
  | "emerald"
  | "amber"
  | "rose"
  | "sky"
  | "slate";

export const ACCENT_PRESETS: ReadonlyArray<{
  id: AccentPreset;
  label: string;
  hint: string;
  light: string;
  dark: string;
}> = [
  { id: "indigo", label: "靛蓝", hint: "默认,冷静专注", light: "#5b5bd6", dark: "#8f8ff2" },
  { id: "violet", label: "紫罗兰", hint: "灵感与创作", light: "#7c3aed", dark: "#a78bfa" },
  { id: "teal", label: "青绿", hint: "清爽耐读", light: "#0d9488", dark: "#2dd4bf" },
  { id: "emerald", label: "翡翠", hint: "平静、生长", light: "#059669", dark: "#34d399" },
  { id: "amber", label: "琥珀", hint: "暖纸与夜读", light: "#b45309", dark: "#fbbf24" },
  { id: "rose", label: "玫瑰", hint: "柔和但有存在感", light: "#e11d48", dark: "#fb7185" },
  { id: "sky", label: "天蓝", hint: "明亮、开放", light: "#0284c7", dark: "#38bdf8" },
  { id: "slate", label: "石墨", hint: "克制、近中性", light: "#475569", dark: "#94a3b8" },
];

const THEME_KEY = "knowwise.theme.v1";
const ACCENT_KEY = "knowwise.accent.v1";
const THEME_STORAGE_OK = (() => {
  try {
    globalThis.localStorage?.getItem(THEME_KEY);
    return true;
  } catch {
    return false;
  }
})();

/** 解析存储值:`system` 跟随 prefers-color-scheme,非法值回退浅色。 */
export function parseTheme(raw: string | null): ThemeMode {
  return raw === "light" || raw === "dark" || raw === "system" ? raw : "light";
}

export function loadTheme(): ThemeMode {
  if (!THEME_STORAGE_OK) return "light";
  try {
    return parseTheme(globalThis.localStorage?.getItem(THEME_KEY) ?? null);
  } catch {
    return "light";
  }
}

export function saveTheme(mode: ThemeMode): void {
  try {
    globalThis.localStorage?.setItem(THEME_KEY, mode);
  } catch {
    // 只影响下次启动的记忆,不影响当前主题。
  }
}

/** 实际生效的明暗(system 时取系统偏好)。 */
export function resolveTheme(mode: ThemeMode): "light" | "dark" {
  if (mode !== "system") return mode;
  return globalThis.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

/** 应用主题到 <html>:写 data-theme 供 CSS 覆盖,并同步 color-scheme。 */
export function applyTheme(mode: ThemeMode): void {
  if (typeof document === "undefined") return;
  const resolved = resolveTheme(mode);
  const root = document.documentElement;
  root.dataset.theme = resolved;
  root.dataset.themeMode = mode;
  root.style.colorScheme = resolved;
}

let systemListener: ((event: MediaQueryListEvent) => void) | null = null;

/**
 * 启动时调用:应用当前主题并监听系统明暗变化(仅 system 模式生效)。
 * 返回清理函数(测试/热更新用)。
 */
export function initTheme(): () => void {
  applyTheme(loadTheme());
  if (globalThis.matchMedia === undefined) return () => {};
  const query = globalThis.matchMedia("(prefers-color-scheme: dark)");
  const listener = () => {
    if (loadTheme() === "system") applyTheme("system");
  };
  systemListener = listener;
  query.addEventListener?.("change", listener);
  return () => {
    query.removeEventListener?.("change", listener);
    if (systemListener === listener) systemListener = null;
  };
}

/** 切换深浅(供顶栏按钮使用):在 light/dark 间切换,不进入 system。 */
export function toggleTheme(current: ThemeMode): ThemeMode {
  return resolveTheme(current) === "dark" ? "light" : "dark";
}

// —— 强调色 ——

export function parseAccent(raw: string | null): AccentPreset {
  const known = ACCENT_PRESETS.some((preset) => preset.id === raw);
  return known ? (raw as AccentPreset) : "indigo";
}

export function loadAccent(): AccentPreset {
  try {
    return parseAccent(globalThis.localStorage?.getItem(ACCENT_KEY) ?? null);
  } catch {
    return "indigo";
  }
}

export function saveAccent(preset: AccentPreset): void {
  try {
    globalThis.localStorage?.setItem(ACCENT_KEY, preset);
  } catch {
    // 只影响下次启动的记忆,不影响当前颜色。
  }
}

/** 应用强调色到 <html data-accent>:CSS 负责浅/深两套取值。 */
export function applyAccent(preset: AccentPreset): void {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.accent = preset;
}

/** 启动时调用:在 React 挂载前恢复强调色,避免默认靛蓝闪一下。 */
export function initAccent(): AccentPreset {
  const preset = loadAccent();
  applyAccent(preset);
  return preset;
}

/** 跨组件同步(设置面板与顶栏各自切换时广播)。 */
export function notifyAccentChanged(preset: AccentPreset): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent("knowwise-accent-changed", { detail: preset }));
}

export function accentHex(preset: AccentPreset, resolved: "light" | "dark"): string {
  const entry = ACCENT_PRESETS.find((item) => item.id === preset) ?? ACCENT_PRESETS[0]!;
  return resolved === "dark" ? entry.dark : entry.light;
}
