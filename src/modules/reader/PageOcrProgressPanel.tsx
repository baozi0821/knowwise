// 全文 OCR 进度/暂停面板:只显示真实 page_ocr 状态,不估算、不伪造进度。
// - 汇总:已识别/总页、当前识别页、排队、失败、低置信;
// - 逐页状态可展开查看并点击跳页;
// - 暂停/继续经 page_ocr_pause/page_ocr_resume 持久化,当前页完成后生效。
import { useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronDown, ChevronRight, Pause, Play } from "lucide-react";
import type { CommandClient } from "../../app/api/client";
import { ApiError } from "../../app/api/client";
import type { PageOcrControlView } from "../../shared/generated/PageOcrControlView";
import type { PageOcrStatusView } from "../../shared/generated/PageOcrStatusView";
import type { ParseState } from "../../shared/generated/ParseState";
import { cancelOcrRun, ocrRunKey, useOcrRuntime } from "./pageOcrRuntime";
import "./pageOcr.css";

const STATUS_LABELS: Record<PageOcrStatusView["status"], string> = {
  pending: "排队",
  rendering: "渲染中",
  ocr_running: "识别中",
  completed: "已识别",
  low_confidence: "低置信",
  failed: "失败",
  skipped: "跳过",
};

function errorCodeOf(error: unknown): string {
  if (error instanceof ApiError) return error.appError.code;
  return "OCR_CONTROL_FAILED";
}

function jumpToPage(pageNumber: number): void {
  window.dispatchEvent(
    new CustomEvent("knowwise-goto", {
      detail: {
        locator: {
          kind: "pdf",
          pageNumber,
          rects: [],
          pageRotation: "0",
          coordinateSpace: "pdf_user_space_v1",
        },
      },
    }),
  );
}

export function PageOcrProgressPanel({
  client,
  documentId,
  versionId,
  parseState,
  ocr,
  pageCount,
  currentGenerationId,
}: {
  client: CommandClient;
  documentId: string;
  versionId: string;
  parseState: ParseState | undefined;
  ocr: readonly PageOcrStatusView[];
  /** PDF 真实页数（来自阅读器 viewport）；用于把“没有 row 的页”算作缺失。 */
  pageCount?: number;
  /** 当前解析代次；旧代次的 completed 不能算当前已识别。 */
  currentGenerationId?: string;
}): ReactNode {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<"pause" | "resume" | null>(null);
  const [notice, setNotice] = useState<{ kind: "info" | "error"; text: string } | null>(null);
  const runKey = ocrRunKey(documentId, versionId);
  const incrementalRunning = useOcrRuntime((state) => state.runs[runKey] === true);
  const liveProgress = useOcrRuntime((state) => state.progress[runKey]);

  const control = useQuery({
    queryKey: ["page-ocr-control", documentId, versionId],
    queryFn: () =>
      client.call<PageOcrControlView>("page_ocr_control_get", { documentId, versionId }),
    enabled: documentId !== "" && versionId !== "",
    staleTime: 1_000,
    refetchInterval: (query) => {
      const paused = query.state.data?.paused === true;
      return parseState === "in_progress" || paused || incrementalRunning ? 1_500 : false;
    },
  });

  // 只把当前代次的状态算作已覆盖；旧代次行保留但显示为“需重跑”。
  const currentRows = ocr.filter(
    (row) =>
      currentGenerationId === undefined ||
      row.generationId === undefined ||
      row.generationId === currentGenerationId,
  );
  const staleCount = ocr.length - currentRows.length;
  const rows = currentRows.filter((row) => row.status !== "skipped");
  const paused = control.data?.paused === true;
  const taskAlive = parseState === "in_progress";
  const runAlive = taskAlive || incrementalRunning;
  if (rows.length === 0 && staleCount === 0 && !taskAlive && !paused && !incrementalRunning) {
    return null;
  }
  const maxPage = currentRows.reduce((max, row) => Math.max(max, row.pageNumber), 0);
  const total = pageCount !== undefined && pageCount > 0 ? Math.max(pageCount, maxPage) : maxPage;
  const doneRows = currentRows.filter(
    (row) =>
      (row.status === "completed" || row.status === "low_confidence") && row.blockCount > 0,
  );
  const zeroBlockRows = currentRows.filter(
    (row) =>
      (row.status === "completed" || row.status === "low_confidence") && row.blockCount === 0,
  );
  const failedRows = currentRows.filter((row) => row.status === "failed").concat(zeroBlockRows);
  const low = doneRows.filter((row) => row.status === "low_confidence").length;
  const pendingRows = currentRows.filter(
    (row) =>
      row.status === "pending" || row.status === "rendering" || row.status === "ocr_running",
  );
  const knownPages = new Set(currentRows.map((row) => row.pageNumber));
  const missing = total > 0 ? Math.max(0, total - knownPages.size) : 0;
  const done = doneRows.length;
  const failed = failedRows.length;
  const skipped = currentRows.filter((row) => row.status === "skipped").length;
  const current =
    pendingRows.find((row) => row.status === "ocr_running") ??
    pendingRows.find((row) => row.status === "rendering") ??
    null;

  async function togglePause(next: boolean): Promise<void> {
    setBusy(next ? "pause" : "resume");
    setNotice(null);
    try {
      await client.call<PageOcrControlView>(
        next ? "page_ocr_pause" : "page_ocr_resume",
        { documentId, versionId },
      );
      await control.refetch();
      setNotice({
        kind: "info",
        text: next
          ? "已请求暂停:当前页完成后不再开始新页;已识别页保留。"
          : runAlive
            ? "已继续:从下一张待识别页开始。"
            : "已清除暂停信号;当前没有运行中的 OCR,需重新开始识别。",
      });
    } catch (error) {
      setNotice({ kind: "error", text: `暂停状态未保存(${errorCodeOf(error)})` });
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className={`ocr-progress${open ? " is-open" : ""}`} aria-label="OCR 进度">
      <div className="ocr-progress-head">
        <button
          type="button"
          className="ocr-progress-toggle"
          aria-expanded={open}
          aria-label={open ? "收起 OCR 逐页状态" : "展开 OCR 逐页状态"}
          onClick={() => setOpen((value) => !value)}
        >
          {open ? (
            <ChevronDown className="w-3.5 h-3.5" aria-hidden="true" />
          ) : (
            <ChevronRight className="w-3.5 h-3.5" aria-hidden="true" />
          )}
          <strong>OCR 进度</strong>
        </button>
        <span className="ocr-progress-summary" role="status">
          已识别 {done}/{total}
          {current !== null ? ` · 当前第 ${current.pageNumber} 页` : ""}
          {pendingRows.length > 0 && current === null ? ` · 排队 ${pendingRows.length}` : ""}
          {missing > 0 ? ` · 缺失 ${missing}` : ""}
          {failed > 0 ? ` · 失败 ${failed}` : ""}
          {low > 0 ? ` · 低置信 ${low}` : ""}
          {skipped > 0 ? ` · 跳过 ${skipped}` : ""}
          {staleCount > 0 ? ` · 旧代次 ${staleCount}` : ""}
        </span>
        {paused ? <span className="ocr-progress-paused">已暂停</span> : null}
        {runAlive ? (
          paused ? (
            <button
              type="button"
              className="btn tiny"
              disabled={busy !== null}
              aria-label="继续 OCR"
              onClick={() => void togglePause(false)}
            >
              <Play className="w-3 h-3" aria-hidden="true" />
              {busy === "resume" ? "继续中…" : "继续"}
            </button>
          ) : (
            <button
              type="button"
              className="btn tiny ghost"
              disabled={busy !== null}
              aria-label="暂停 OCR"
              title="暂停在当前页完成后生效;不取消任务、不丢弃已识别页"
              onClick={() => void togglePause(true)}
            >
              <Pause className="w-3 h-3" aria-hidden="true" />
              {busy === "pause" ? "请求中…" : "暂停"}
            </button>
          )
        ) : paused ? (
          <button
            type="button"
            className="btn tiny ghost"
            disabled={busy !== null}
            aria-label="清除 OCR 暂停状态"
            onClick={() => void togglePause(false)}
          >
            {busy === "resume" ? "清除中…" : "清除暂停"}
          </button>
        ) : null}
        {incrementalRunning && !taskAlive ? (
          <button
            type="button"
            className="btn tiny ghost"
            aria-label="停止自动 OCR"
            title="停止自动 OCR;已识别页仍会保存"
            onClick={() => cancelOcrRun(documentId, versionId)}
          >
            停止
          </button>
        ) : null}
      </div>

      {incrementalRunning && liveProgress ? (
        <p className="ocr-progress-notice" role="status">
          本次 OCR:第 {liveProgress.pageNumber} 页 ·{" "}
          {STATUS_LABELS[liveProgress.status as PageOcrStatusView["status"]] ??
            liveProgress.status}{" "}
          ({liveProgress.done}/{liveProgress.total})
        </p>
      ) : null}

      {notice !== null ? (
        <p
          className={`ocr-progress-notice ${notice.kind}`}
          role={notice.kind === "error" ? "alert" : "status"}
        >
          {notice.text}
        </p>
      ) : null}
      {paused && !runAlive ? (
        <p className="ocr-progress-notice warn" role="status">
          暂停信号仍在,但当前没有运行中的 OCR;已识别页保留。继续需要重新开始识别,
          不会把“重跑”当作续跑。
        </p>
      ) : null}

      {open ? (
        <div className="ocr-progress-pages" role="list" aria-label="OCR 逐页状态">
          {rows.length === 0 ? (
            <p className="ocr-progress-empty">等待解析器上报页状态…</p>
          ) : (
            rows.map((row) => (
              <button
                key={row.pageNumber}
                type="button"
                role="listitem"
                className={`ocr-page-chip status-${row.status}`}
                title={`第 ${row.pageNumber} 页 · ${STATUS_LABELS[row.status]}${
                  row.errorCode ? ` · ${row.errorCode}` : ""
                }${typeof row.confidence === "number" ? ` · ${Math.round(row.confidence * 100)}%` : ""}`}
                aria-label={`跳到第 ${row.pageNumber} 页(${STATUS_LABELS[row.status]})`}
                onClick={() => jumpToPage(row.pageNumber)}
              >
                {row.pageNumber}
                <span className="ocr-page-chip-dot" aria-hidden="true" />
              </button>
            ))
          )}
        </div>
      ) : null}
    </section>
  );
}
