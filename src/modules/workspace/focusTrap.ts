// M13 拥有:对话框焦点陷阱与恢复(《M13》§9)。核心路径纯键盘可达;
// Tab 循环限制在对话框内,关闭后焦点回到原触发点。

export const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(", ");

export function focusableElements(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (el) => {
      // jsdom 无布局:offsetParent 恒为 null,改用显示/隐藏语义判定。
      if (el.closest("[hidden]") !== null) return false;
      const display = window.getComputedStyle(el).display;
      return display !== "none";
    },
  );
}

export function focusFirst(container: HTMLElement): void {
  const elements = focusableElements(container);
  (elements[0] ?? container).focus();
}

/**
 * 对话框内按键处理:Tab/Shift+Tab 在容器内循环;返回是否已处理
 * (调用方对已处理事件 preventDefault)。焦点意外逃逸时拉回容器。
 * 入参用结构化键盘事件,兼容 React 合成事件与原生事件。
 */
export type TrappableKeyEvent = {
  key: string;
  shiftKey: boolean;
  preventDefault(): void;
};

export function handleTrapKey(
  container: HTMLElement,
  event: TrappableKeyEvent,
): boolean {
  if (event.key !== "Tab") return false;
  const elements = focusableElements(container);
  if (elements.length === 0) {
    event.preventDefault();
    container.focus();
    return true;
  }
  const first = elements[0]!;
  const last = elements[elements.length - 1]!;
  const active = document.activeElement;
  if (event.shiftKey) {
    if (active === first || active === container || !container.contains(active)) {
      event.preventDefault();
      last.focus();
      return true;
    }
  } else if (active === last || active === container || !container.contains(active)) {
    event.preventDefault();
    first.focus();
    return true;
  }
  return false;
}

/** 记录触发点并在关闭时恢复焦点;返回恢复函数。 */
export function captureTriggerFocus(trigger: HTMLElement | null): () => void {
  return () => {
    trigger?.focus();
  };
}
