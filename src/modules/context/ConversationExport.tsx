// M07 会话导出入口(WBS-6/P0 收尾):Markdown / JSON,自包含且不含本机路径。
// 支持面板模式与弹窗模式,集成于 AI 助手会话操作中。
import { useEffect, useState } from "react";
import { Download, FileJson, FileText, Loader2, X } from "lucide-react";
import type { ConversationExportResult } from "../../shared/generated/ConversationExportResult";
import { useServices } from "../../app/services";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import { dialogSavePath } from "../../app/ipc/transport";
import { useQaStore } from "../qa/qaStore";
import { useWorkspaceUi } from "../workspace/uiStore";
import "./conversation-export.css";

type ExportFormat = "markdown" | "json";

const FORMAT_META: Record<
  ExportFormat,
  { label: string; extension: string; dialogTitle: string; icon: typeof FileText }
> = {
  markdown: {
    label: "Markdown",
    extension: "md",
    dialogTitle: "导出会话(Markdown)",
    icon: FileText,
  },
  json: {
    label: "JSON",
    extension: "json",
    dialogTitle: "导出会话(JSON)",
    icon: FileJson,
  },
};

function defaultFileName(format: ExportFormat): string {
  const date = new Date().toISOString().slice(0, 10);
  return `会话-${date}.${FORMAT_META[format].extension}`;
}

function presentError(caught: unknown): string {
  if (caught instanceof ApiError) {
    const presentation = describeAppError(caught.appError);
    return (
      presentation.title + (presentation.actionLabel ? `(${presentation.actionLabel})` : "")
    );
  }
  return "会话导出失败,请重试";
}

export function ConversationExportPanel() {
  const { client } = useServices();
  const documentId = useWorkspaceUi((state) => state.currentDocumentId);
  const conversationId = useQaStore((state) => state.currentConversationId);
  const [busy, setBusy] = useState<ExportFormat | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (documentId === null) return null;

  async function exportConversation(format: ExportFormat): Promise<void> {
    if (conversationId === null || busy !== null) return;
    const meta = FORMAT_META[format];
    setBusy(format);
    setError(null);
    setStatus(null);
    try {
      const path = await dialogSavePath({
        title: meta.dialogTitle,
        defaultPath: defaultFileName(format),
        filters: [{ name: meta.label, extensions: [meta.extension] }],
      });
      if (path === null) {
        setStatus("已取消导出");
        return;
      }
      const result = await client.call<ConversationExportResult>("conversation_export", {
        conversationId,
        format,
        path,
      });
      setStatus(`已导出 ${meta.label}(${result.bytesWritten} 字节)`);
    } catch (caught) {
      setError(presentError(caught));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="conversation-export" aria-label="会话导出">
      <div className="conversation-export-head">
        <span className="conversation-export-title">
          <Download className="w-3.5 h-3.5" aria-hidden="true" />
          会话导出
        </span>
        <span className="conversation-export-hint">
          {conversationId === null
            ? "先在 AI 助手提问创建会话,再导出完整记录。"
            : "导出当前会话;图片以 data URI 内嵌,不写入本机路径。"}
        </span>
      </div>
      <div className="conversation-export-actions">
        {(Object.keys(FORMAT_META) as ExportFormat[]).map((format) => {
          const meta = FORMAT_META[format];
          const Icon = meta.icon;
          return (
            <button
              key={format}
              type="button"
              className="btn small"
              disabled={conversationId === null || busy !== null}
              onClick={() => void exportConversation(format)}
            >
              {busy === format ? (
                <Loader2 className="w-3.5 h-3.5 animate-spin" aria-hidden="true" />
              ) : (
                <Icon className="w-3.5 h-3.5" aria-hidden="true" />
              )}
              {busy === format ? "导出中…" : `导出 ${meta.label}`}
            </button>
          );
        })}
      </div>
      {status !== null ? (
        <p className="conversation-export-status" role="status">
          {status}
        </p>
      ) : null}
      {error !== null ? (
        <p className="art-error" role="alert">
          {error}
        </p>
      ) : null}
    </section>
  );
}

export function ConversationExportDialog({
  conversationId: propConversationId,
  onClose,
}: {
  conversationId?: string | null;
  onClose: () => void;
}) {
  const activeConversationId = useQaStore((state) => state.currentConversationId);
  const conversationId = propConversationId ?? activeConversationId;
  const { client } = useServices();
  const [busy, setBusy] = useState<ExportFormat | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  async function exportConversation(format: ExportFormat): Promise<void> {
    if (conversationId === null || busy !== null) return;
    const meta = FORMAT_META[format];
    setBusy(format);
    setError(null);
    setStatus(null);
    try {
      const path = await dialogSavePath({
        title: meta.dialogTitle,
        defaultPath: defaultFileName(format),
        filters: [{ name: meta.label, extensions: [meta.extension] }],
      });
      if (path === null) {
        setStatus("已取消导出");
        return;
      }
      const result = await client.call<ConversationExportResult>("conversation_export", {
        conversationId,
        format,
        path,
      });
      setStatus(`已成功导出 ${meta.label} (${result.bytesWritten} 字节)`);
    } catch (caught) {
      setError(presentError(caught));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="settings-overlay" role="presentation" onClick={onClose}>
      <div
        className="conversation-export-modal"
        role="dialog"
        aria-modal="true"
        aria-label="导出会话"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="conversation-export-modal-head">
          <div className="conversation-export-head-left">
            <Download className="w-4 h-4 text-indigo-500" />
            <h3 className="font-semibold text-sm">导出会话记录</h3>
          </div>
          <button
            type="button"
            className="icon-btn small"
            onClick={onClose}
            aria-label="关闭导出对话框"
            title="关闭 (Esc)"
          >
            <X className="w-4 h-4" />
          </button>
        </div>
        <div className="conversation-export-modal-body">
          <p className="conversation-export-hint">
            {conversationId === null
              ? "暂无可用会话，请先在 AI 助手提问创建会话后再导出。"
              : "将当前会话的完整历史、问答交互与引用信息导出为本地文档。附件截图将内联嵌入，不依赖本地绝对路径。"}
          </p>
          <div className="conversation-export-modal-actions">
            {(Object.keys(FORMAT_META) as ExportFormat[]).map((format) => {
              const meta = FORMAT_META[format];
              const Icon = meta.icon;
              return (
                <button
                  key={format}
                  type="button"
                  className="btn primary flex-1"
                  disabled={conversationId === null || busy !== null}
                  onClick={() => void exportConversation(format)}
                >
                  {busy === format ? (
                    <Loader2 className="w-3.5 h-3.5 animate-spin mr-1" />
                  ) : (
                    <Icon className="w-3.5 h-3.5 mr-1" />
                  )}
                  {busy === format ? "导出中…" : `导出为 ${meta.label}`}
                </button>
              );
            })}
          </div>
          {status !== null ? (
            <p className="conversation-export-status" role="status">
              {status}
            </p>
          ) : null}
          {error !== null ? (
            <p className="art-error" role="alert">
              {error}
            </p>
          ) : null}
        </div>
      </div>
    </div>
  );
}
