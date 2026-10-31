// 指定页 OCR 对话框:选择页码 → (必要时先停止全文识别) → 本地 tesseract 识别 → 落库。
// 所有状态来自真实任务/识别结果;取消只停止未开始的页,已识别页仍保存。
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";
import type { CommandClient } from "../../app/api/client";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import type { TaskView } from "../../shared/generated/TaskView";
import type { PageOcrStatusView } from "../../shared/generated/PageOcrStatusView";
import {
  MAX_PAGES_PER_OCR_RUN,
  findActiveParseTask,
  formatPageSelection,
  hasActiveSelectedPageOcr,
  parsePageSpec,
  recognizePages,
  stopActiveParse,
} from "./pageOcrRecognition";
import { registerOcrRun, updateOcrProgress } from "./pageOcrRuntime";

const STATUS_LABELS: Record<string, string> = {
  pending: "排队",
  rendering: "渲染页图",
  ocr_running: "识别中",
  completed: "完成",
  low_confidence: "低置信",
  failed: "未识别到文字",
  skipped: "有文本层,跳过",
};

export function PageOcrRecognizeDialog({
  client,
  documentId,
  versionId,
  pageCount,
  currentPage,
  initialPages,
  bytes,
  onClose,
  onSaved,
  onBackgroundNotice,
}: {
  client: CommandClient;
  documentId: string;
  versionId: string;
  pageCount: number;
  currentPage: number;
  initialPages?: readonly number[];
  bytes: Uint8Array | null;
  onClose: () => void;
  onSaved: (statuses: PageOcrStatusView[]) => void;
  /** 关闭后后台识别失败/无结果时仍要给用户可见反馈(不吞错)。 */
  onBackgroundNotice?: (message: string, kind: "info" | "error") => void;
}): ReactNode {
  const seed = initialPages !== undefined && initialPages.length > 0 ? initialPages : [currentPage];
  const [spec, setSpec] = useState(() => formatPageSelection(seed));
  const [error, setError] = useState<string | null>(null);
  const [activeParse, setActiveParse] = useState<TaskView | null>(null);
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [progress, setProgress] = useState<{
    pageNumber: number;
    status: string;
    done: number;
    total: number;
  } | null>(null);
  const [summary, setSummary] = useState<string | null>(null);
  const [force, setForce] = useState(false);
  const stopRef = useRef(false);
  const closedRef = useRef(false);
  const runningRef = useRef(false);

  const parsed = useMemo(() => parsePageSpec(spec, pageCount), [spec, pageCount]);

  useEffect(() => {
    let cancelled = false;
    void findActiveParseTask(client, documentId)
      .then((task) => {
        if (!cancelled) setActiveParse(task);
      })
      .catch(() => {
        if (!cancelled) setActiveParse(null);
      });
    return () => {
      cancelled = true;
    };
  }, [client, documentId]);

  const requestClose = useCallback(() => {
    if (runningRef.current) {
      // 关闭不等于取消已写入内容:当前页完成后停止,已识别页仍保存。
      stopRef.current = true;
      if (!closedRef.current) {
        closedRef.current = true;
        onBackgroundNotice?.(
          "识别已转到后台:当前页完成后停止,已识别页会保存。",
          "info",
        );
      }
    }
    onClose();
  }, [onBackgroundNotice, onClose, running]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") requestClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [requestClose]);

  async function start(pagesOverride?: readonly number[]): Promise<void> {
    if (bytes === null) {
      setError("正文尚未加载完成,请稍后再试。");
      return;
    }
    if (hasActiveSelectedPageOcr()) {
      setError("上一次识别仍在后台收尾,请稍候再开始");
      return;
    }
    const check =
      pagesOverride !== undefined
        ? parsePageSpec(formatPageSelection(pagesOverride), pageCount)
        : parsePageSpec(spec, pageCount);
    if (check.error !== null) {
      setError(check.error);
      return;
    }
    setError(null);
    setSummary(null);
    setRunning(true);
    runningRef.current = true;
    setProgress(null);
    stopRef.current = false;
    closedRef.current = false;
    let unregisterRun: (() => void) | null = null;
    try {
      let stoppedTask: string | null = null;
      // 打开对话框后全文识别可能才开始:开始识别前再查一次,避免同代次并发写入。
      const taskNow = activeParse ?? (await findActiveParseTask(client, documentId).catch(() => null));
      if (taskNow !== null) {
        setStopping(true);
        stoppedTask = await stopActiveParse(client, documentId);
        setStopping(false);
        setActiveParse(null);
      }
      // 新的显式识别从“未暂停”开始，避免上一次暂停信号让本次空等。
      await client
        .call("page_ocr_resume", { documentId, versionId })
        .catch(() => undefined);
      // 只把当前代次视为已覆盖;旧代次的 completed 需要重跑。
      const parseStatus = await client
        .call<{ generationId?: string }>("parse_get_status", { documentId })
        .catch(() => null);
      unregisterRun = registerOcrRun(documentId, versionId, () => {
        stopRef.current = true;
      });
      const result = await recognizePages({
        client,
        documentId,
        versionId,
        bytes,
        pageNumbers: check.pages,
        force,
        currentGenerationId: parseStatus?.generationId,
        shouldStop: () => stopRef.current,
        shouldPause: async () => {
          try {
            const control = await client.call<{ paused: boolean }>("page_ocr_control_get", {
              documentId,
              versionId,
            });
            return control.paused;
          } catch {
            return false;
          }
        },
        onProgress: (entry) => {
          updateOcrProgress(documentId, versionId, entry);
          setProgress(entry);
        },
      });
      if (result.saved.length > 0) {
        onSaved(result.saved);
      } else if (closedRef.current) {
        onBackgroundNotice?.("后台识别已停止,没有新的识别页需要保存。", "info");
      }
      const failed = result.saved.filter((status) => status.status === "failed").length;
      setSummary(
        [
          result.recognized.length > 0
            ? `本次识别并保存 ${result.recognized.length} 页`
            : result.skipped.length > 0
              ? "所选页均已有文本层,无需 OCR"
              : "没有需要识别的新页",
          result.alreadyDone.length > 0
            ? `${result.alreadyDone.length} 页已识别,已幂等跳过`
            : null,
          result.skipped.length > 0 ? `${result.skipped.length} 页有文本层已跳过` : null,
          failed > 0 ? `${failed} 页未识别到文字(未伪造文本)` : null,
          result.notProcessed.length > 0 ? `${result.notProcessed.length} 页因停止未处理` : null,
          stoppedTask !== null ? "全文识别已停止" : null,
        ]
          .filter(Boolean)
          .join(";") + "。",
      );
    } catch (caught) {
      const message =
        caught instanceof ApiError
          ? [
              describeAppError(caught.appError).title,
              ...describeAppError(caught.appError).detailLines,
            ]
              .filter(Boolean)
              .join(";") + `(错误码 ${caught.appError.code})`
          : caught instanceof Error
            ? caught.message
            : "识别失败,请重试";
      if (closedRef.current) {
        onBackgroundNotice?.(`后台识别未完成:${message}`, "error");
      } else if (caught instanceof ApiError) {
        const presentation = describeAppError(caught.appError);
        setError(
          [presentation.title, ...presentation.detailLines].filter(Boolean).join(";") +
            `(错误码 ${caught.appError.code})`,
        );
      } else {
        setError(caught instanceof Error ? caught.message : "识别失败,请重试");
      }
    } finally {
      unregisterRun?.();
      runningRef.current = false;
      setRunning(false);
      setStopping(false);
      setProgress(null);
      void findActiveParseTask(client, documentId)
        .then(setActiveParse)
        .catch(() => undefined);
    }
  }

  const specPages = parsed.pages;
  const pageLabel = progress !== null ? `第 ${progress.pageNumber} 页` : "";
  const aroundPages = Array.from({ length: 5 }, (_, index) => currentPage - 2 + index).filter(
    (page) => page >= 1 && (pageCount === 0 || page <= pageCount),
  );
  const firstTenPages = Array.from(
    { length: Math.min(10, pageCount || 10) },
    (_, index) => index + 1,
  );
  function quickStart(pages: number[]): void {
    setSpec(formatPageSelection(pages));
    void start(pages);
  }

  return createPortal(
    <div className="kw-confirm-overlay" role="presentation">
      <div
        className="kw-confirm-dialog ocr-recognize-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="识别指定页 OCR"
      >
        <div className="kw-confirm-head">
          <strong>识别指定页 OCR</strong>
          <button
            type="button"
            className="icon-btn"
            aria-label="关闭指定页识别"
            onClick={requestClose}
          >
            <X className="w-4 h-4" aria-hidden />
          </button>
        </div>
        <p className="kw-confirm-message">
          只在本机渲染所选页并用本地 tesseract 识别(不调用模型、不联网、页图不落库)。
          识别文字会写入当前文档索引,可用于全文问答与引用。
        </p>
        <div className="ocr-recognize-spec-row">
          <label className="ocr-recognize-spec">
            <span>页码</span>
            <input
              type="text"
              value={spec}
              placeholder="例如 1-5,8,10"
              aria-label="OCR 页码"
              disabled={running}
              onChange={(event) => {
                setSpec(event.target.value);
                setError(null);
                setSummary(null);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !running) void start();
              }}
            />
          </label>
          <div className="ocr-recognize-quick" role="group" aria-label="快速选择页码">
            <button
              type="button"
              className="btn tiny ghost"
              disabled={running}
              onClick={() => quickStart([currentPage])}
            >
              识别当前页
            </button>
            <button
              type="button"
              className="btn tiny ghost"
              disabled={running}
              onClick={() => quickStart(aroundPages)}
            >
              识别当前页±2
            </button>
            <button
              type="button"
              className="btn tiny ghost"
              disabled={running}
              onClick={() => quickStart(firstTenPages)}
            >
              识别前 10 页
            </button>
          </div>
        </div>
        <p className="ocr-recognize-hint">
          {parsed.error !== null ? (
            <span className="ocr-recognize-invalid" role="alert">
              {parsed.error}
            </span>
          ) : (
            <span role="status">
              已选 {specPages.length} 页;一次最多 {MAX_PAGES_PER_OCR_RUN} 页。
            </span>
          )}
        </p>
        <label className="settings-check ocr-recognize-force">
          <input
            type="checkbox"
            checked={force}
            disabled={running}
            onChange={(event) => {
              setForce(event.target.checked);
              setSummary(null);
            }}
          />
          重新识别已识别页(默认只补缺失/失败页;不会产生重复块)
        </label>
        {activeParse !== null ? (          <p className="ocr-recognize-warning" role="alert">
            全文识别仍在进行。识别所选页会先停止全文识别;已完成识别的页会先保存,
            只有停止瞬间尚未识别完成的当前页需要重试。
          </p>
        ) : null}
        {stopping ? (
          <p className="ocr-recognize-progress" role="status">
            正在停止全文识别…
          </p>
        ) : null}
        {running && !stopping ? (
          <div className="ocr-recognize-progress" role="status">
            <span>
              {progress !== null
                ? `正在识别${pageLabel}:${STATUS_LABELS[progress.status] ?? progress.status}(${progress.done}/${progress.total})`
                : "正在准备页图与本地识别引擎…"}
            </span>
            <button
              type="button"
              className="btn tiny ghost"
              onClick={() => {
                stopRef.current = true;
              }}
            >
              停止(已识别页仍保存)
            </button>
            <span className="ocr-recognize-hint">也可直接关闭窗口,当前页完成后自动停止并保存。</span>
          </div>
        ) : null}
        {summary !== null ? (
          <p className="ocr-recognize-summary" role="status">
            {summary}
          </p>
        ) : null}
        {error !== null ? (
          <p className="settings-error" role="alert">
            {error}
          </p>
        ) : null}
        <div className="kw-confirm-actions">
          <button type="button" className="btn small ghost" onClick={requestClose}>
            {running ? "停止并关闭" : summary !== null ? "关闭" : "取消"}
          </button>
          <button
            type="button"
            className="btn small primary"
            onClick={() => void start()}
            disabled={running || parsed.error !== null || parsed.pages.length === 0}
          >
            {running ? "识别中…" : "识别所选页"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
