// M13 拥有:全局快捷键映射(《M13》§10)。只包含必要项,避开常见输入法
// 组合(Ctrl+Space/Ctrl+Shift 等);全部快捷键在帮助/设置中可发现。
// 纯函数:组件把 KeyboardEvent 的只读字段传入。

export type ShortcutAction =
  | "open-document"
  | "focus-search"
  | "toggle-focus-mode"
  | "toggle-agent"
  | "toggle-notes"
  | "toggle-shortcut-help"
  | "go-reader"
  | "go-agent-input"
  | "go-status"
  | "next-tab"
  | "prev-tab"
  | "close-tab"
  | "toggle-sidebar";

export const SHORTCUT_HELP: ReadonlyArray<{
  action: ShortcutAction;
  keys: string;
  description: string;
}> = [
  { action: "open-document", keys: "Ctrl+O", description: "打开文档" },
  { action: "focus-search", keys: "Ctrl+F", description: "聚焦搜索" },
  { action: "toggle-focus-mode", keys: "Alt+Shift+F", description: "切换专注模式" },
  { action: "toggle-agent", keys: "Alt+Shift+A", description: "右栏:AI 助手" },
  { action: "toggle-notes", keys: "Alt+Shift+N", description: "右栏:笔记" },
  { action: "toggle-shortcut-help", keys: "F1", description: "快捷键帮助" },
  { action: "go-reader", keys: "Alt+1", description: "跳转到主内容(阅读器)" },
  { action: "go-agent-input", keys: "Alt+2", description: "跳转到 Agent 输入" },
  { action: "go-status", keys: "Alt+3", description: "跳转到状态区" },
  { action: "next-tab", keys: "Ctrl+Tab", description: "下一个标签页" },
  { action: "prev-tab", keys: "Ctrl+Shift+Tab", description: "上一个标签页" },
  { action: "close-tab", keys: "Ctrl+W", description: "关闭当前标签页" },
  { action: "toggle-sidebar", keys: "Ctrl+B", description: "显示/隐藏文档库" },
];

export type ShortcutKeyInput = {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
};

/** 目标焦点在文本输入控件内时,仅放行不与编辑冲突的组合。 */
export function isTextInput(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    // jsdom 无布局,isContentEditable 可能不推导;同时接受显式属性。
    target.isContentEditable === true ||
    target.getAttribute("contenteditable") === "true"
  );
}

export function matchShortcut(
  event: ShortcutKeyInput,
  context: { inTextInput: boolean } = { inTextInput: false },
): ShortcutAction | null {
  const ctrl = event.ctrlKey || event.metaKey;
  // IME 组合键(含 Shift 的 Ctrl 组合、Ctrl+Space)一律不拦截;
  // 例外:Tab 系标签页切换(无输入法冲突)。
  if (ctrl && event.altKey) return null;
  if (ctrl && event.key === "Tab") return event.shiftKey ? "prev-tab" : "next-tab";
  if (ctrl && event.shiftKey) return null;
  if (ctrl && event.key.toLowerCase() === "o") return "open-document";
  if (ctrl && event.key.toLowerCase() === "f") return "focus-search";
  if (ctrl && event.key.toLowerCase() === "w") return "close-tab";
  if (ctrl && event.key.toLowerCase() === "b") return "toggle-sidebar";

  if (event.altKey && event.shiftKey && event.key.toLowerCase() === "f") {
    return "toggle-focus-mode";
  }
  if (event.altKey && event.shiftKey && event.key.toLowerCase() === "a") {
    return "toggle-agent";
  }
  if (event.altKey && event.shiftKey && event.key.toLowerCase() === "n") {
    return "toggle-notes";
  }
  // F1 打开快捷键帮助:输入控件内也放行(F1 不属于输入法与文本编辑组合,
  // 且帮助面板本身就是"快捷键找不到"时的唯一出路)。
  if (event.key === "F1") {
    return "toggle-shortcut-help";
  }
  // Alt+ 数字跳转是导航,输入控件内也不劫持(与跳转链接等价的键盘路径)。
  if (event.altKey && !event.shiftKey && !ctrl) {
    if (event.key === "1") return "go-reader";
    if (event.key === "2") return "go-agent-input";
    if (event.key === "3") return "go-status";
  }
  if (context.inTextInput) return null;
  return null;
}
