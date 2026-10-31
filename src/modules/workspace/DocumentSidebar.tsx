// 左侧文档导航:搜索 + 按格式分组(PDF / Word / Markdown)+ 导入入口。
// 路径只来自原生对话框;导入幂等键由客户端生成;列表只显示元数据。
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import {
  Plus,
  FileText,
  FileType2,
  Hash,
  ChevronRight,
  Search,
  AlertCircle,
  Trash2,
  PanelLeftClose,
} from "lucide-react";
import type { DocumentSummary } from "../../shared/generated/DocumentSummary";
import type { DocumentFormat } from "../../shared/generated/DocumentFormat";
import type { ImportResult } from "../../shared/generated/ImportResult";
import { useServices } from "../../app/services";
import { qk } from "../../app/api/keys";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import type { PickDocumentFn } from "../../app/ipc/transport";
import { useWorkspaceUi } from "./uiStore";

const FORMAT_META: Record<DocumentFormat, { label: string; icon: typeof FileText }> = {
  pdf: { label: "PDF", icon: FileText },
  docx: { label: "Word", icon: FileType2 },
  markdown: { label: "Markdown", icon: Hash },
};

function relativeTime(ms: number): string {
  const diff = Date.now() - ms;
  const minutes = Math.round(diff / 60_000);
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days} 天前`;
  const months = Math.round(days / 30);
  if (months < 12) return `${months} 个月前`;
  return `${Math.round(months / 12)} 年前`;
}

function docTimestamp(doc: DocumentSummary): string {
  const last = doc.lastOpenedAt != null ? Number(doc.lastOpenedAt) : Number(doc.updatedAt);
  return Number.isFinite(last) ? relativeTime(last) : "";
}

export function DocumentSidebar({
  currentDocumentId,
  onOpenDocument,
  pickDocument,
  width,
  onClose,
}: {
  currentDocumentId: string | null;
  onOpenDocument: (documentId: string, versionId: string | null) => void;
  pickDocument: PickDocumentFn;
  width?: number;
  onClose?: () => void;
}) {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const [importing, setImporting] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  const recent = useQuery({
    queryKey: qk.recentDocuments(50),
    queryFn: () =>
      client.call<{ items: DocumentSummary[] }>("document_list_recent", {
        limit: 50,
      }),
  });

  const groups = useMemo(() => {
    const keyword = filter.trim().toLowerCase();
    const items = (recent.data?.items ?? []).filter(
      (doc) => keyword === "" || doc.displayName.toLowerCase().includes(keyword),
    );
    const byFormat = new Map<DocumentFormat, DocumentSummary[]>();
    for (const doc of items) {
      const list = byFormat.get(doc.format) ?? [];
      list.push(doc);
      byFormat.set(doc.format, list);
    }
    const order: DocumentFormat[] = ["pdf", "docx", "markdown"];
    return order
      .filter((f) => (byFormat.get(f)?.length ?? 0) > 0)
      .map((f) => ({ format: f, docs: byFormat.get(f)! }));
  }, [recent.data, filter]);

  async function handleDelete(documentId: string) {
    setConfirmDelete(null);
    try {
      await client.call("document_delete", { documentId });
      await queryClient.invalidateQueries({ queryKey: ["documents"] });
      await queryClient.invalidateQueries({ queryKey: ["document"] });
      useWorkspaceUi.getState().closeTab(documentId);
    } catch {
      setImportError("删除失败,请重试");
    }
  }

  async function handleImport() {
    setImporting(true);
    setImportError(null);
    try {
      const path = await pickDocument();
      if (path === null) return;
      const result = await client.call<ImportResult>("document_import", {
        path,
        source: "picker",
        idempotencyKey: client.newIdempotencyKey(),
      });
      await queryClient.invalidateQueries({ queryKey: ["documents"] });
      onOpenDocument(result.documentId, result.versionId);
    } catch (error) {
      const apiError = error instanceof ApiError ? error : null;
      const presentation = apiError
        ? describeAppError(apiError.appError)
        : { title: "导入未完成", actionLabel: null, retryable: false, detailLines: [] };
      setImportError(presentation.title);
    } finally {
      setImporting(false);
    }
  }

  const total = recent.data?.items.length ?? 0;

  return (
    <nav
      className="doc-sidebar"
      aria-label="文档库"
      style={width !== undefined ? { width } : undefined}
    >
      {onClose !== undefined ? (
        <button
          type="button"
          className="sidebar-close-btn"
          title="收起文档库 (Ctrl+B)"
          aria-label="收起文档库"
          onClick={onClose}
        >
          <PanelLeftClose className="w-4 h-4" />
        </button>
      ) : null}
      <button
        type="button"
        className="btn primary sidebar-import"
        onClick={handleImport}
        disabled={importing}
      >
        <Plus className="w-4 h-4" aria-hidden />
        <span>{importing ? "导入中…" : "导入文档"}</span>
      </button>

      <div className="sidebar-search">
        <Search className="w-3.5 h-3.5" aria-hidden />
        <input
          id="doc-search"
          type="search"
          value={filter}
          placeholder="搜索文档"
          aria-label="搜索文档"
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>

      {importError ? (
        <p className="sidebar-error" role="alert">
          <AlertCircle className="w-3.5 h-3.5 shrink-0" aria-hidden />
          <span>{importError}</span>
        </p>
      ) : null}

      {recent.isError ? (
        <p role="alert" className="sidebar-error">
          <AlertCircle className="w-3.5 h-3.5 shrink-0" aria-hidden />
          <span>文档列表加载失败,请重试。</span>
        </p>
      ) : null}

      {recent.isLoading ? <p className="sidebar-status">加载中…</p> : null}

      {!recent.isLoading && !recent.isError && total === 0 ? (
        <div className="sidebar-empty">
          <FileText className="w-8 h-8" aria-hidden />
          <p>文档库还是空的</p>
          <p className="hint">导入 PDF、Word 或 Markdown 开始阅读。</p>
        </div>
      ) : null}

      {groups.map(({ format, docs }) => {
        const meta = FORMAT_META[format];
        const Icon = meta.icon;
        const isCollapsed = collapsed[format] === true;
        return (
          <section key={format} className="doc-group" aria-label={meta.label}>
            <button
              type="button"
              className="doc-group-head"
              aria-expanded={!isCollapsed}
              onClick={() => setCollapsed((prev) => ({ ...prev, [format]: !collapsed[format] }))}
            >
              <ChevronRight
                className={`doc-group-chevron${isCollapsed ? "" : " open"}`}
                aria-hidden
              />
              <Icon className={`tab-format-icon format-${format}`} aria-hidden />
              <span className="doc-group-title">{meta.label}</span>
              <span className="doc-group-count">{docs.length}</span>
            </button>
            {!isCollapsed ? (
              <ul className="doc-list">
                {docs.map((doc) => (
                  <li key={doc.id}>
                    {confirmDelete === doc.id ? (
                      <div className="doc-delete-confirm" role="alert">
                        <span>删除「{doc.displayName}」及其笔记与标注?</span>
                        <span className="doc-delete-actions">
                          <button
                            type="button"
                            className="btn small"
                            onClick={() => void handleDelete(doc.id)}
                          >
                            删除
                          </button>
                          <button
                            type="button"
                            className="btn small ghost"
                            onClick={() => setConfirmDelete(null)}
                          >
                            取消
                          </button>
                        </span>
                      </div>
                    ) : (
                      <div
                        className={
                          doc.id === currentDocumentId ? "doc-item current" : "doc-item"
                        }
                      >
                        <button
                          type="button"
                          className="doc-item-main"
                          aria-current={doc.id === currentDocumentId ? "true" : undefined}
                          onClick={() => onOpenDocument(doc.id, doc.currentVersionId)}
                          title={doc.displayName}
                        >
                          <span className="doc-item-name">{doc.displayName}</span>
                          {doc.fileState !== "present" ? (
                            <span className="doc-file-flag" title="原文件状态异常" aria-hidden>
                              !
                            </span>
                          ) : (
                            <span className="doc-item-time">{docTimestamp(doc)}</span>
                          )}
                        </button>
                        <button
                          type="button"
                          className="doc-item-delete"
                          aria-label={`删除 ${doc.displayName}`}
                          title="删除文档(含笔记与标注)"
                          onClick={() => setConfirmDelete(doc.id)}
                        >
                          <Trash2 className="w-3.5 h-3.5" aria-hidden />
                        </button>
                      </div>
                    )}
                  </li>
                ))}
              </ul>
            ) : null}
          </section>
        );
      })}

      {!recent.isLoading && !recent.isError && filter.trim() !== "" && groups.length === 0 ? (
        <p className="sidebar-status">没有匹配「{filter.trim()}」的文档。</p>
      ) : null}
    </nav>
  );
}
