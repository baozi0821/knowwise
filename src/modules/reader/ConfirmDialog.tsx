// 阅读区统一删除确认框:显示对象与数量,不提供"5 秒撤销"。
// 用真实模态语义(role=alertdialog),Esc 取消;确认按钮在忙碌时禁用。
import { useEffect, useRef, type ReactNode } from "react";
import { AlertTriangle } from "lucide-react";

export type ConfirmRequest = {
  title: string;
  message: string;
  confirmLabel?: string;
  danger?: boolean;
  onConfirm: () => void | Promise<void>;
};

export function ConfirmDialog({
  request,
  busy,
  onCancel,
}: {
  request: ConfirmRequest | null;
  busy?: boolean;
  onCancel: () => void;
}): ReactNode {
  const confirmRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (request === null) return;
    confirmRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onCancel();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [request, onCancel]);
  if (request === null) return null;
  return (
    <div className="kw-confirm-overlay" role="presentation">
      <div
        className="kw-confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-label={request.title}
      >
        <div className="kw-confirm-head">
          <AlertTriangle className="w-4 h-4" aria-hidden />
          <strong>{request.title}</strong>
        </div>
        <p className="kw-confirm-message">{request.message}</p>
        <div className="kw-confirm-actions">
          <button type="button" className="btn small ghost" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button
            ref={confirmRef}
            type="button"
            className={`btn small${request.danger === false ? "" : " danger"}`}
            disabled={busy}
            onClick={() => void request.onConfirm()}
          >
            {busy ? "处理中…" : (request.confirmLabel ?? "删除")}
          </button>
        </div>
      </div>
    </div>
  );
}
