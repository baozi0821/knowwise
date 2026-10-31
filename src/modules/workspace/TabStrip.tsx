// 浏览器式文档标签栏:每个打开的文档一个标签;点击切换、中键/X 关闭、
// 「+」打开导入。标题来自最近文档查询;未知文档回退为占位名。
// 双文档对照:点击对照按钮后从「已打开的文档 / 最近文档」中选择右侧文档,
// 不再自动挑第一个非当前标签(用户要求可指定文档进行双页阅读)。
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useQuery } from "@tanstack/react-query";
import { Plus, X, FileText, FileType2, Hash, Columns2 } from "lucide-react";
import type { DocumentSummary } from "../../shared/generated/DocumentSummary";
import type { DocumentFormat } from "../../shared/generated/DocumentFormat";
import { useServices } from "../../app/services";
import { qk } from "../../app/api/keys";
import { useWorkspaceUi } from "./uiStore";

const FORMAT_ICON: Record<DocumentFormat, typeof FileText> = {
  pdf: FileText,
  docx: FileType2,
  markdown: Hash,
};

function FormatBadge({ format }: { format: DocumentFormat }) {
  const Icon = FORMAT_ICON[format] ?? FileText;
  return <Icon className={`tab-format-icon format-${format}`} aria-hidden />;
}

type CompareCandidate = {
  documentId: string;
  versionId: string | null;
  name: string;
  format: DocumentFormat;
  source: "tab" | "recent";
};

export function TabStrip({ onOpenDocument }: { onOpenDocument: () => void }) {
  const { client } = useServices();
  const tabs = useWorkspaceUi((s) => s.tabs);
  const currentDocumentId = useWorkspaceUi((s) => s.currentDocumentId);
  const openDocument = useWorkspaceUi((s) => s.openDocument);
  const closeTab = useWorkspaceUi((s) => s.closeTab);
  const compareTarget = useWorkspaceUi((s) => s.compareTarget);
  const setCompareTarget = useWorkspaceUi((s) => s.setCompareTarget);
  const [comparePickerOpen, setComparePickerOpen] = useState(false);
  const [compareQuery, setCompareQuery] = useState("");
  const [menuPos, setMenuPos] = useState<{ top: number; left: number } | null>(null);
  const pickerRef = useRef<HTMLDivElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const compareButtonRef = useRef<HTMLButtonElement | null>(null);

  const openComparePicker = () => {
    const rect = compareButtonRef.current?.getBoundingClientRect();
    if (rect) {
      setMenuPos({
        top: Math.round(rect.bottom + 6),
        left: Math.round(Math.max(8, Math.min(rect.left, window.innerWidth - 308))),
      });
    }
    setComparePickerOpen(true);
  };

  useEffect(() => {
    if (!comparePickerOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (pickerRef.current?.contains(target) || menuRef.current?.contains(target)) return;
      setComparePickerOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setComparePickerOpen(false);
    };
    const onReposition = () => setComparePickerOpen(false);
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    window.addEventListener("resize", onReposition);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("resize", onReposition);
    };
  }, [comparePickerOpen]);

  const recent = useQuery({
    queryKey: qk.recentDocuments(50),
    queryFn: () =>
      client.call<{ items: DocumentSummary[] }>("document_list_recent", { limit: 50 }),
    staleTime: 30_000,
  });
  const byId = new Map((recent.data?.items ?? []).map((d) => [d.id, d]));

  useEffect(() => {
    if (compareTarget !== null) setComparePickerOpen(false);
  }, [compareTarget]);

  if (tabs.length === 0) return null;

  // 候选:已打开的标签(除当前文档)优先,其次最近文档(含未打开的)。
  const seen = new Set<string>([currentDocumentId ?? ""]);
  const candidates: CompareCandidate[] = [];
  for (const tab of tabs) {
    if (seen.has(tab.documentId)) continue;
    seen.add(tab.documentId);
    const doc = byId.get(tab.documentId);
    candidates.push({
      documentId: tab.documentId,
      versionId: tab.versionId,
      name: doc?.displayName ?? "文档",
      format: doc?.format ?? "pdf",
      source: "tab",
    });
  }
  for (const doc of recent.data?.items ?? []) {
    if (seen.has(doc.id) || doc.currentVersionId === null) continue;
    seen.add(doc.id);
    candidates.push({
      documentId: doc.id,
      versionId: doc.currentVersionId,
      name: doc.displayName,
      format: doc.format,
      source: "recent",
    });
  }
  const filtered = compareQuery.trim().length === 0
    ? candidates
    : candidates.filter((candidate) =>
        candidate.name.toLowerCase().includes(compareQuery.trim().toLowerCase()),
      );

  const startCompare = (candidate: CompareCandidate) => {
    setCompareTarget({ documentId: candidate.documentId, versionId: candidate.versionId });
    setComparePickerOpen(false);
    setCompareQuery("");
  };

  return (
    <div className="tab-strip" role="tablist" aria-label="打开的文档">
      {tabs.map((tab) => {
        const doc = byId.get(tab.documentId);
        const active = tab.documentId === currentDocumentId;
        return (
          <button
            key={tab.documentId}
            type="button"
            role="tab"
            aria-selected={active}
            className={`doc-tab${active ? " active" : ""}`}
            title={doc?.displayName ?? "文档"}
            onClick={() => openDocument(tab.documentId, tab.versionId)}
            onAuxClick={(event) => {
              // 中键关闭(浏览器习惯)。
              if (event.button === 1) {
                event.preventDefault();
                closeTab(tab.documentId);
              }
            }}
          >
            <FormatBadge format={doc?.format ?? "pdf"} />
            <span className="doc-tab-title">{doc?.displayName ?? "文档"}</span>
            {doc?.fileState === "missing" ? (
              <span className="tab-file-warning" title="原文件已移动或删除" aria-hidden>
                !
              </span>
            ) : null}
            <span
              className={`doc-tab-close${active ? "" : " soft"}`}
              role="button"
              tabIndex={-1}
              aria-label={`关闭 ${doc?.displayName ?? "文档"}`}
              title="关闭标签页"
              onClick={(event) => {
                event.stopPropagation();
                closeTab(tab.documentId);
              }}
            >
              <X className="w-3 h-3" />
            </span>
          </button>
        );
      })}
      {candidates.length > 0 || compareTarget !== null ? (
        <div className="tab-compare-wrap" ref={pickerRef}>
          <button
            ref={compareButtonRef}
            type="button"
            className={`tab-compare${compareTarget !== null ? " active" : ""}`}
            aria-pressed={compareTarget !== null}
            aria-haspopup="dialog"
            aria-expanded={comparePickerOpen}
            aria-label={compareTarget !== null ? "退出对照" : "选择双文档对照"}
            title={
              compareTarget !== null
                ? "退出双文档对照"
                : "双文档对照:选择要在右侧并排阅读的文档"
            }
            onClick={() => {
              if (compareTarget !== null) {
                setCompareTarget(null);
                return;
              }
              if (comparePickerOpen) {
                setComparePickerOpen(false);
              } else {
                openComparePicker();
              }
            }}
          >
            <Columns2 className="w-3.5 h-3.5" />
          </button>
          {comparePickerOpen && menuPos
            ? createPortal(
                <div
                  ref={menuRef}
                  className="tab-compare-menu"
                  style={{ top: menuPos.top, left: menuPos.left }}
                  role="dialog"
                  aria-label="选择对照文档"
                >
              <label className="tab-compare-search">
                <span className="visually-hidden">搜索文档</span>
                <input
                  type="search"
                  placeholder="搜索文档…"
                  value={compareQuery}
                  onChange={(event) => setCompareQuery(event.target.value)}
                  autoFocus
                />
              </label>
              <p className="tab-compare-hint">选择在右侧并排阅读的文档</p>
              <ul className="tab-compare-list" role="listbox" aria-label="候选文档">
                {filtered.length === 0 ? (
                  <li className="tab-compare-empty" role="status">
                    没有匹配的文档
                  </li>
                ) : (
                  filtered.map((candidate) => (
                    <li key={candidate.documentId} role="none">
                      <button
                        type="button"
                        role="option"
                        aria-selected={false}
                        className="tab-compare-item"
                        onClick={() => startCompare(candidate)}
                      >
                        <FormatBadge format={candidate.format} />
                        <span className="tab-compare-item-name">{candidate.name}</span>
                        <span className="tab-compare-item-source">
                          {candidate.source === "tab" ? "已打开" : "最近"}
                        </span>
                      </button>
                    </li>
                  ))
                )}
              </ul>
                </div>,
                document.body,
              )
            : null}
        </div>
      ) : null}
      <button
        type="button"
        className="tab-new"
        onClick={onOpenDocument}
        aria-label="打开新文档"
        title="打开文档 (Ctrl+O)"
      >
        <Plus className="w-3.5 h-3.5" />
      </button>
    </div>
  );
}
