// 快捷键帮助(F1 / 顶栏键盘按钮)。SHORTCUT_HELP 常量是唯一事实来源,
// 这里只负责呈现;Esc 或点击遮罩关闭,焦点陷阱复用 focusTrap 工具。
import { useEffect, useRef, type ReactNode } from "react";
import { X } from "lucide-react";
import { SHORTCUT_HELP } from "./shortcuts";
import { captureTriggerFocus, handleTrapKey } from "./focusTrap";

export function ShortcutHelp({ onClose }: { onClose: () => void }): ReactNode {
  const dialogRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const restore = captureTriggerFocus(document.activeElement as HTMLElement | null);
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      const dialog = dialogRef.current;
      if (dialog !== null) handleTrapKey(dialog, event);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      restore();
    };
  }, [onClose]);

  return (
    <div className="shortcut-backdrop" onPointerDown={onClose}>
      <div
        ref={dialogRef}
        className="shortcut-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="shortcut-help-title"
        onPointerDown={(event) => event.stopPropagation()}
      >
        <header className="shortcut-dialog-head">
          <h2 id="shortcut-help-title">键盘快捷键</h2>
          <button
            ref={closeRef}
            type="button"
            className="icon-btn"
            aria-label="关闭快捷键帮助"
            onClick={onClose}
          >
            <X className="w-4 h-4" />
          </button>
        </header>
        <ul className="shortcut-list">
          {SHORTCUT_HELP.map((item) => (
            <li key={item.action}>
              <span className="shortcut-desc">{item.description}</span>
              <kbd className="shortcut-keys">{item.keys}</kbd>
            </li>
          ))}
          <li>
            <span className="shortcut-desc">在笔记编辑器中撤销 / 重做</span>
            <kbd className="shortcut-keys">Ctrl+Z / Ctrl+Shift+Z</kbd>
          </li>
          <li>
            <span className="shortcut-desc">在笔记编辑器中粘贴截图</span>
            <kbd className="shortcut-keys">Ctrl+V</kbd>
          </li>
        </ul>
        <p className="shortcut-hint">
          提示:输入框内 Ctrl 组合键留给输入法与文本编辑,只有 Tab 系切换会被应用接管。
        </p>
      </div>
    </div>
  );
}
