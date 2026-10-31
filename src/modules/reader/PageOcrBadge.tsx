// 2D 页角 OCR 徽标:状态/置信度可见;失败与低置信度提供真实“重新识别”。
// 不显示未实现的“视觉读页/手动粘贴”假按钮。
import { useState, type ReactNode } from "react";
import type { PageOcrStatusView } from "../../shared/generated/PageOcrStatusView";
import type { CommandClient } from "../../app/api/client";
import type { PageOcrStatusView as PageOcrStatus } from "../../shared/generated/PageOcrStatusView";
import { OcrTextEditor } from "./ocrEditor";
import "./pageOcr.css";

function statusLabel(status: PageOcrStatusView): string {
  switch (status.status) {
    case "pending":
      return "OCR 排队";
    case "rendering":
      return "OCR 渲染";
    case "ocr_running":
      return "OCR 识别中";
    case "completed":
      return status.blockCount === 0 ? "OCR 无文字" : "OCR";
    case "low_confidence":
      return "OCR 低置信";
    case "failed":
      return "OCR 失败";
    default:
      return "";
  }
}

function confidenceText(status: PageOcrStatusView): string | null {
  if (typeof status.confidence !== "number") return null;
  return `${Math.round(status.confidence * 100)}%`;
}

export function PageOcrBadge({
  status,
  currentGenerationId,
  onReprocess,
  onRecognizePages,
  pageCount,
  busy = false,
  client,
  documentId,
  versionId,
  onCorrected,
}: {
  status?: PageOcrStatusView;
  /** 当前解析代次；旧代次的 completed 不能显示成当前已识别。 */
  currentGenerationId?: string;
  onReprocess?: () => void;
  /** 指定页 OCR:只识别本页(不重建整篇)。 */
  onRecognizePages?: (pages: number[]) => void;
  pageCount?: number;
  busy?: boolean;
  client?: CommandClient;
  documentId?: string;
  versionId?: string;
  onCorrected?: (statuses: PageOcrStatus[]) => void;
}): ReactNode {
  const [editorOpen, setEditorOpen] = useState(false);
  if (!status || status.status === "skipped") return null;
  const stale =
    status.generationId !== undefined &&
    currentGenerationId !== undefined &&
    status.generationId !== currentGenerationId;
  const label = stale ? "OCR 需重跑" : statusLabel(status);
  const confidence = stale ? null : confidenceText(status);
  const low = !stale && status.status === "low_confidence";
  const failed = !stale && status.status === "failed";
  const running =
    !stale &&
    (status.status === "pending" ||
      status.status === "rendering" ||
      status.status === "ocr_running");
  const actionable =
    (stale ||
      failed ||
      low ||
      (status.status === "completed" && status.blockCount === 0)) &&
    onReprocess;
  const detail = [
    `第 ${status.pageNumber} 页`,
    label,
    confidence ? `置信度 ${confidence}` : null,
    stale ? "旧解析代次,需重新识别" : null,
    low ? "请核对原文" : null,
    status.errorCode ? `错误码 ${status.errorCode}` : null,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <div
      className={`pdf-ocr-badge status-${status.status}`}
      role="status"
      aria-live="polite"
      aria-label={detail}
      title={detail}
    >
      <span className="pdf-ocr-label">{label}</span>
      {confidence ? <span className="pdf-ocr-confidence">{confidence}</span> : null}
      {low ? <span className="pdf-ocr-hint">请核对原文</span> : null}
      {running ? <span className="pdf-ocr-dot" aria-hidden="true" /> : null}
      {actionable ? (
        <button
          type="button"
          className="pdf-ocr-retry"
          disabled={busy}
          aria-label={`逐页重新识别全文(${status.pageNumber} 页入口)`}
          title="逐页重新识别全文(保留其它页;较慢);只识别本页请用「识别本页」"
          onClick={(event) => {
            event.stopPropagation();
            onReprocess?.();
          }}
        >
          {busy ? "识别中…" : "重新识别全文(逐页)"}
        </button>
      ) : null}
      {onRecognizePages !== undefined && !running ? (
        <button
          type="button"
          className="pdf-ocr-recognize"
          aria-label={`识别第 ${status.pageNumber} 页(仅本页 OCR)`}
          title="仅识别本页:本机 tesseract,不调用模型;可先停止全文识别"
          onClick={(event) => {
            event.stopPropagation();
            onRecognizePages([status.pageNumber]);
          }}
        >
          {status.status === "completed" || status.status === "low_confidence"
            ? "重新识别本页"
            : "识别本页"}
        </button>
      ) : null}
      {!stale && client !== undefined && documentId !== undefined && versionId !== undefined ? (
        <button
          type="button"
          className="pdf-ocr-edit"
          aria-label={`编辑第 ${status.pageNumber} 页识别文本`}
          onClick={(event) => {
            event.stopPropagation();
            setEditorOpen(true);
          }}
        >
          编辑识别文本
        </button>
      ) : null}
      {editorOpen && !stale && client !== undefined && documentId !== undefined && versionId !== undefined ? (
        <OcrTextEditor
          client={client}
          documentId={documentId}
          versionId={versionId}
          pageNumber={status.pageNumber}
          pageCount={pageCount}
          status={status}
          onClose={() => setEditorOpen(false)}
          onSaved={(statuses) => {
            onCorrected?.(statuses);
            setEditorOpen(false);
          }}
        />
      ) : null}
    </div>
  );
}
