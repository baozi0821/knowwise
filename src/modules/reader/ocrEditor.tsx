// 页级 OCR 文本编辑:逐段核对/修改真实识别文本,保存后重建该页 FTS。
// 实用能力:上/下页切换、合并到上段、删除段(软删除,旧引用仍可解析)、
// 撤销修改、复制全文、Ctrl+Enter 保存、未保存修改可见。
// 不提供“让模型补写”之类的假按钮;空扫描页可人工补录一段真实文字。
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { ChevronLeft, ChevronRight, Copy, Trash2, X } from "lucide-react";
import type { CommandClient } from "../../app/api/client";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import { clipboardWriteText } from "../../app/ipc/transport";
import type { PageOcrText } from "../../shared/generated/PageOcrText";
import type { PageOcrStatusView } from "../../shared/generated/PageOcrStatusView";

type EditableBlock = { blockId?: string; text: string };

function snapshot(blocks: EditableBlock[]): string {
  return JSON.stringify(blocks);
}

export function OcrTextEditor({
  client,
  documentId,
  versionId,
  pageNumber,
  pageCount,
  status,
  onClose,
  onSaved,
}: {
  client: CommandClient;
  documentId: string;
  versionId: string;
  pageNumber: number;
  /** 文档总页数;提供时可上一页/下一页。 */
  pageCount?: number;
  /** 当前页 OCR 状态(显示置信度/原因)。 */
  status?: PageOcrStatusView;
  onClose: () => void;
  onSaved: (statuses: PageOcrStatusView[]) => void;
}): ReactNode {
  const [page, setPage] = useState(pageNumber);
  const [blocks, setBlocks] = useState<EditableBlock[] | null>(null);
  const [initial, setInitial] = useState<string>("[]");
  const [removedIds, setRemovedIds] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const firstAreaRef = useRef<HTMLTextAreaElement | null>(null);

  const dirty = blocks !== null && (snapshot(blocks) !== initial || removedIds.length > 0);

  const load = useCallback(
    async (target: number) => {
      setBlocks(null);
      setRemovedIds([]);
      setError(null);
      setNotice(null);
      try {
        const pageText = await client.call<PageOcrText>("page_ocr_get_text", {
          documentId,
          versionId,
          pageNumber: target,
        });
        const loaded: EditableBlock[] =
          pageText.blocks.length > 0
            ? pageText.blocks.map((block) => ({ blockId: block.blockId, text: block.text }))
            : [{ text: "" }];
        setBlocks(loaded);
        setInitial(snapshot(loaded));
        window.setTimeout(() => firstAreaRef.current?.focus(), 0);
      } catch (caught) {
        setError(
          caught instanceof ApiError
            ? describeAppError(caught.appError).title
            : "读取 OCR 文本失败",
        );
        setBlocks([{ text: "" }]);
        setInitial(snapshot([{ text: "" }]));
      }
    },
    [client, documentId, versionId],
  );

  useEffect(() => {
    void load(page);
  }, [load, page]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onClose();
      if (event.key === "Enter" && (event.ctrlKey || event.metaKey) && !busy) {
        event.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  function patchBlock(index: number, text: string): void {
    setBlocks((previous) =>
      previous === null
        ? previous
        : previous.map((block, at) => (at === index ? { ...block, text } : block)),
    );
  }

  function removeBlock(index: number): void {
    const block = blocks?.[index];
    if (block === undefined) return;
    if (block.blockId !== undefined) {
      setRemovedIds((previous) =>
        previous.includes(block.blockId!) ? previous : [...previous, block.blockId!],
      );
    }
    setBlocks((previous) => previous?.filter((_, at) => at !== index) ?? previous);
    setNotice("已删除该段(保存后从索引移除;旧引用仍可解析)。");
  }

  function mergeIntoPrevious(index: number): void {
    if (blocks === null || index <= 0) return;
    const current = blocks[index]!;
    const previous = blocks[index - 1]!;
    const merged = `${previous.text.trimEnd()}\n${current.text.trimStart()}`.trim();
    if (current.blockId !== undefined) {
      setRemovedIds((ids) =>
        ids.includes(current.blockId!) ? ids : [...ids, current.blockId!],
      );
    }
    setBlocks([
      ...blocks.slice(0, index - 1),
      { ...previous, text: merged },
      ...blocks.slice(index + 1),
    ]);
    setNotice("已合并到上一段(保存后生效)。");
  }

  function undoChanges(): void {
    try {
      const restored = JSON.parse(initial) as EditableBlock[];
      setBlocks(restored);
      setRemovedIds([]);
      setError(null);
      setNotice("已撤销未保存的修改。");
    } catch {
      void load(page);
    }
  }

  async function copyAll(): Promise<void> {
    if (blocks === null) return;
    try {
      await clipboardWriteText(blocks.map((block) => block.text).join("\n\n").trim());
      setNotice("已复制本页识别文本。");
    } catch {
      setError("复制失败,请重试。");
    }
  }

  async function save(): Promise<void> {
    if (blocks === null || busy) return;
    const corrections = blocks
      .filter((block) => block.text.trim().length > 0)
      .map((block) => ({ blockId: block.blockId, text: block.text }));
    const emptiedExisting = blocks
      .filter((block) => block.blockId !== undefined && block.text.trim().length === 0)
      .map((block) => block.blockId!);
    const removals = Array.from(new Set([...removedIds, ...emptiedExisting]));
    if (corrections.length === 0) {
      setError("至少保留一段非空文本;如需整页清空,请使用「识别本页」重新识别。");
      return;
    }
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const statuses = await client.call<PageOcrStatusView[]>("page_ocr_apply_correction", {
        documentId,
        versionId,
        pageNumber: page,
        blocks: corrections,
        removeBlockIds: removals,
        idempotencyKey: client.newIdempotencyKey(),
      });
      onSaved(statuses);
      setInitial(snapshot(blocks));
      setRemovedIds([]);
      setNotice("已保存并重建本页索引。");
    } catch (caught) {
      if (caught instanceof ApiError) {
        const presentation = describeAppError(caught.appError);
        const details = presentation.detailLines.filter(Boolean).join(";");
        const hint = /parse_in_progress/.test(details)
          ? ";全文识别进行中:请先在页面徽标使用「识别本页」停止全文识别后再保存"
          : "";
        setError(
          [presentation.title, details].filter(Boolean).join(";") +
            `(错误码 ${caught.appError.code}${hint})`,
        );
      } else {
        setError("OCR 纠错保存失败,请重试");
      }
    } finally {
      setBusy(false);
    }
  }

  const canNavigate = !busy && !dirty;
  const totalChars = (blocks ?? []).reduce((sum, block) => sum + block.text.trim().length, 0);

  return createPortal(
    <div className="kw-confirm-overlay" role="presentation">
      <div
        className="kw-confirm-dialog ocr-editor-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="编辑 OCR 文本"
      >
        <div className="kw-confirm-head">
          <strong>
            编辑 OCR 文本 · 第 {page} 页
            {pageCount !== undefined ? ` / ${pageCount}` : ""}
          </strong>
          <span className="ocr-editor-head-actions">
            {dirty ? (
              <span className="ocr-editor-dirty" role="status">
                未保存修改
              </span>
            ) : null}
            <button
              type="button"
              className="icon-btn"
              aria-label="上一页识别文本"
              title={dirty ? "先保存或撤销修改再翻页" : "上一页"}
              disabled={!canNavigate || page <= 1}
              onClick={() => setPage(Math.max(1, page - 1))}
            >
              <ChevronLeft className="w-4 h-4" aria-hidden />
            </button>
            <button
              type="button"
              className="icon-btn"
              aria-label="下一页识别文本"
              title={dirty ? "先保存或撤销修改再翻页" : "下一页"}
              disabled={!canNavigate || (pageCount !== undefined && page >= pageCount)}
              onClick={() => setPage(pageCount !== undefined ? Math.min(pageCount, page + 1) : page + 1)}
            >
              <ChevronRight className="w-4 h-4" aria-hidden />
            </button>
            <button type="button" className="icon-btn" aria-label="关闭 OCR 编辑" onClick={onClose} disabled={busy}>
              <X className="w-4 h-4" aria-hidden />
            </button>
          </span>
        </div>
        <p className="kw-confirm-message">
          只修改真实识别文本;保存后会重建本页关键词索引(不调用模型、不补写内容)。
          {" "}删除/合并的段在保存后从索引移除,旧引用仍按快照解析。
        </p>
        <p className="ocr-editor-meta" role="status">
          <span>
            {status?.status === "low_confidence"
              ? `低置信${typeof status.confidence === "number" ? ` ${Math.round(status.confidence * 100)}%` : ""}`
              : status?.status === "completed"
                ? `识别完成${typeof status.confidence === "number" ? ` · 置信度 ${Math.round(status.confidence * 100)}%` : ""}`
                : status?.status === "failed"
                  ? `识别失败${status.errorCode ? ` · ${status.errorCode}` : ""}`
                  : "本页尚未识别"}
          </span>
          <span>
            {blocks?.length ?? 0} 段 · {totalChars} 字
          </span>
        </p>
        {blocks === null ? (
          <p role="status">正在读取 OCR 文本…</p>
        ) : (
          <div className="ocr-editor-blocks">
            {blocks.map((block, index) => (
              <div key={block.blockId ?? `new-${index}`} className="ocr-editor-block">
                <span className="ocr-editor-block-head">
                  <span>
                    {block.blockId === undefined ? "新增文本段" : `识别段 ${index + 1}`}
                  </span>
                  <span className="ocr-editor-block-actions">
                    <button
                      type="button"
                      className="btn tiny ghost"
                      disabled={busy || index === 0}
                      aria-label={`合并第 ${index + 1} 段到上一段`}
                      title="合并到上一段"
                      onClick={() => mergeIntoPrevious(index)}
                    >
                      合并上段
                    </button>
                    <button
                      type="button"
                      className="btn tiny ghost"
                      disabled={busy}
                      aria-label={`删除第 ${index + 1} 段`}
                      title="删除该段(保存后从索引移除)"
                      onClick={() => removeBlock(index)}
                    >
                      <Trash2 className="w-3 h-3" aria-hidden />
                    </button>
                  </span>
                </span>
                <textarea
                  ref={index === 0 ? firstAreaRef : undefined}
                  value={block.text}
                  rows={Math.min(10, Math.max(3, Math.ceil(block.text.length / 60)))}
                  aria-label={`OCR 文本段 ${index + 1}`}
                  onChange={(event) => patchBlock(index, event.target.value)}
                  spellCheck={false}
                />
              </div>
            ))}
            <div className="ocr-editor-add-row">
              <button
                type="button"
                className="btn small ghost"
                onClick={() => setBlocks((previous) => [...(previous ?? []), { text: "" }])}
                disabled={busy}
              >
                新增一段
              </button>
              <span className="ocr-editor-shortcut">Ctrl+Enter 保存</span>
            </div>
          </div>
        )}
        {notice !== null ? (
          <p className="ocr-editor-notice" role="status">
            {notice}
          </p>
        ) : null}
        {error !== null ? (
          <p className="settings-error" role="alert">
            {error}
          </p>
        ) : null}
        <div className="kw-confirm-actions">
          <button
            type="button"
            className="btn small ghost"
            onClick={() => void copyAll()}
            disabled={busy || blocks === null}
            aria-label="复制本页识别文本"
          >
            <Copy className="w-3.5 h-3.5" aria-hidden />
            复制全文
          </button>
          <button
            type="button"
            className="btn small ghost"
            onClick={undoChanges}
            disabled={busy || !dirty}
          >
            撤销修改
          </button>
          <button type="button" className="btn small ghost" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button
            type="button"
            className="btn small primary"
            onClick={() => void save()}
            disabled={busy || blocks === null}
          >
            {busy ? "保存中…" : "保存并重建索引"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
