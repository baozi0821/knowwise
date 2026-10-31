// M13 拥有:全部 Tauri IPC/系统能力访问的唯一入口(《M13》§5/§10)。
// 动态导入保证 vitest(jsdom)与纯浏览器环境不触发 Tauri 运行时;
// 组装根与组件只依赖本文件导出的函数类型,测试注入替身。
//
// 业务数据一律走 invoke 自定义命令;对话框/剪贴板/外部链接是仅有的
// 三项系统能力,对应 capabilities/default.json 的最小权限。

export type InvokeFn = (
  command: string,
  payload?: Record<string, unknown>,
) => Promise<unknown>;

export type UnsubscribeFn = () => void;

export type SubscribeFn = (
  channel: string,
  handler: (payload: unknown) => void,
) => Promise<UnsubscribeFn>;

export type PickDocumentFn = () => Promise<string | null>;

export type WriteClipboardTextFn = (text: string) => Promise<void>;

export type OpenExternalUrlFn = (url: string) => Promise<void>;

/** 命令调用(唯一入口;参数统一为 { request },见《02》§1)。 */
export const tauriInvoke: InvokeFn = async (command, payload) => {
  const core = await import("@tauri-apps/api/core");
  return core.invoke(command, payload);
};

export type StreamInvokeFn = (
  command: string,
  request: unknown,
  onEvent: (event: unknown) => void,
) => Promise<unknown>;

/**
 * 带 Tauri Channel 的命令调用:高频有序增量,不经过全局事件总线。
 * Channel 只承载临时展示事件;最终状态仍以消息/任务查询为准。
 */
export const tauriInvokeStreamed: StreamInvokeFn = async (command, request, onEvent) => {
  const core = await import("@tauri-apps/api/core");
  const channel = new core.Channel<unknown>();
  channel.onmessage = onEvent;
  return core.invoke(command, { request, onEvent: channel });
};

/** 事件订阅:Rust TauriEventSink 在 COMMIT 后广播 DomainEvent 到该通道。 */
export const tauriListen: SubscribeFn = async (channel, handler) => {
  const event = await import("@tauri-apps/api/event");
  const unlisten = await event.listen(channel, (e) => handler(e.payload));
  return unlisten;
};

/** 原生文件选择对话框(导入入口;路径只来自用户选择,不接受自由输入)。 */
export const dialogPickDocument: PickDocumentFn = async () => {
  const dialog = await import("@tauri-apps/plugin-dialog");
  const picked = await dialog.open({
    multiple: false,
    directory: false,
    title: "选择要导入的文档(PDF / Markdown / DOCX)",
  });
  return typeof picked === "string" ? picked : null;
};

/** 原生图片选择对话框(多选;返回用户实际选择的路径)。 */
export const dialogPickImages: () => Promise<string[]> = async () => {
  const dialog = await import("@tauri-apps/plugin-dialog");
  const picked = await dialog.open({
    multiple: true,
    directory: false,
    title: "选择图片(PNG / JPEG / WebP,最多 3 张)",
    filters: [{ name: "图片", extensions: ["png", "jpg", "jpeg", "webp"] }],
  });
  if (typeof picked === "string") return [picked];
  return Array.isArray(picked) ? picked.filter((p): p is string => typeof p === "string") : [];
};

/** 剪贴板导出(复制引用/结果;成功失败都向用户反馈,见《M13》§8)。 */
export const clipboardWriteText: WriteClipboardTextFn = async (text) => {
  const clip = await import("@tauri-apps/plugin-clipboard-manager");
  await clip.writeText(text);
};

/** 外部链接:经系统打开前由调用方确认(应用内不加载远程内容)。 */
export const openExternalUrl: OpenExternalUrlFn = async (url) => {
  const opener = await import("@tauri-apps/plugin-opener");
  await opener.openUrl(url);
};

// —— 屏幕截图与保存路径(capture 追加;既有导出保持不变)——

/** 原生整屏捕获结果:虚拟屏幕物理像素 PNG data-URI 与像素尺寸。 */
export type CaptureScreenResult = {
  dataUrl: string;
  width: number;
  height: number;
};

/**
 * 原生整屏捕获(Win32 GDI,虚拟屏幕物理像素)。命令无 `request` 形参,
 * 因此这里直接 `core.invoke`,不经 CommandClient.call 的 request 包裹。
 */
export const captureScreen: () => Promise<CaptureScreenResult> = async () => {
  const core = await import("@tauri-apps/api/core");
  const raw = await core.invoke<CaptureScreenResult>("capture_screen");
  if (!raw || typeof raw.dataUrl !== "string" || !raw.dataUrl.startsWith("data:")) {
    throw new Error("capture_screen 返回了无效的截图数据");
  }
  return raw;
};

export type SavePathFilter = { name: string; extensions: string[] };

export type SavePathOptions = {
  title?: string;
  defaultPath?: string;
  filters?: SavePathFilter[];
};

/** 「另存为」路径选择:默认 Markdown 过滤器;用户取消返回 null。 */
export const dialogSavePath: (opts?: SavePathOptions) => Promise<string | null> = async (
  opts = {},
) => {
  const dialog = await import("@tauri-apps/plugin-dialog");
  const picked = await dialog.save({
    title: opts.title ?? "导出 Markdown",
    defaultPath: opts.defaultPath ?? "笔记.md",
    filters:
      opts.filters ?? [{ name: "Markdown", extensions: ["md", "markdown"] }],
  });
  return typeof picked === "string" ? picked : null;
};
