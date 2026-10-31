// 2D PDF 页级 OCR 编排:质量检测 → 仅对不合格页渲染/识别 → 页状态上报。
// 页图只在内存中停留;进度/状态经 page_ocr_report 提交后才算事实。
import type { PdfTextItem } from "../../modules/reader/format_adapters/pdf_text";
import { assessPdfPageQuality, type PdfPageQuality } from "../parser/pdf_quality";
import {
  createOcrSession,
  hashImage,
  OcrEngineError,
  preprocessForOcr,
  renderPageImage,
  type OcrSession,
} from "./engine";
import {
  OCR_ENGINE_VERSION,
  OCR_LOW_CONFIDENCE,
  type OcrPageResult,
  type PageOcrReportEntry,
} from "./types";

export type PdfPageForOcr = {
  pageNumber: number;
  items: PdfTextItem[];
  width?: number;
  height?: number;
};

export type OcrReporter = (entries: PageOcrReportEntry[]) => Promise<void>;

function reportEntry(
  page: PdfPageForOcr,
  quality: PdfPageQuality,
  status: PageOcrReportEntry["status"],
  extra: Partial<PageOcrReportEntry> = {},
): PageOcrReportEntry {
  return {
    pageNumber: page.pageNumber,
    status,
    quality,
    reasons: quality.reasons,
    blockCount: 0,
    ...extra,
  };
}

async function reportChunked(
  reporter: OcrReporter,
  entries: PageOcrReportEntry[],
): Promise<void> {
  for (let i = 0; i < entries.length; i += 200) {
    await reporter(entries.slice(i, i + 200));
  }
}

export async function runPdfOcr({
  pdfDoc,
  pages,
  report,
  priorityPage,
  shouldStop,
  shouldPause,
  results: externalResults,
}: {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  pdfDoc: any;
  pages: PdfPageForOcr[];
  report: OcrReporter;
  priorityPage?: number;
  /** 指定页 OCR 的取消钩子:在页间检查,已识别页的结果保留。 */
  shouldStop?: () => boolean;
  /**
   * 全文 OCR 的暂停钩子:每页开始前检查;暂停在当前页完成后生效,
   * 不取消任务、不丢弃已识别页;返回 false 后从下一张 pending 页继续。
   */
  shouldPause?: () => Promise<boolean> | boolean;
  /** 调用方持有的结果表:即使中途上报失败退出,已完成页仍保留供落库。 */
  results?: Map<number, OcrPageResult>;
}): Promise<Map<number, OcrPageResult>> {
  const results = externalResults ?? new Map<number, OcrPageResult>();
  const quality = new Map<number, PdfPageQuality>();
  const initial: PageOcrReportEntry[] = [];
  for (const page of pages) {
    const assessed = assessPdfPageQuality(page);
    quality.set(page.pageNumber, assessed);
    if (!assessed.needsOcr) {
      results.set(page.pageNumber, {
        status: "skipped",
        attempted: false,
        blocks: [],
        reasons: ["text_layer_ok"],
      });
      initial.push(reportEntry(page, assessed, "skipped", { reasons: ["text_layer_ok"] }));
    } else {
      initial.push(reportEntry(page, assessed, "pending"));
    }
  }
  if (initial.length > 0) await reportChunked(report, initial);
  const pending = pages
    .filter((page) => quality.get(page.pageNumber)?.needsOcr)
    .sort((a, b) => {
      if (a.pageNumber === priorityPage) return -1;
      if (b.pageNumber === priorityPage) return 1;
      return a.pageNumber - b.pageNumber;
    });
  if (pending.length === 0) return results;

  let session: OcrSession | null = null;
  let engineFailure: string | null = null;
  try {
    for (const page of pending) {
      if (shouldStop?.()) break;
      if (shouldPause !== undefined) {
        let paused = await shouldPause();
        while (paused && !shouldStop?.()) {
          await new Promise((resolve) => setTimeout(resolve, 700));
          if (shouldStop?.()) break;
          paused = await shouldPause();
        }
      }
      if (shouldStop?.()) break;
      const assessed = quality.get(page.pageNumber)!;
      if (engineFailure !== null) {
        results.set(page.pageNumber, {
          status: "failed",
          attempted: true,
          blocks: [],
          reasons: [...assessed.reasons, engineFailure],
          errorCode: engineFailure,
        });
        await report([reportEntry(page, assessed, "failed", {
          errorCode: engineFailure,
          engineVersion: OCR_ENGINE_VERSION,
          reasons: [...assessed.reasons, engineFailure],
        })]);
        continue;
      }

      await report([reportEntry(page, assessed, "rendering", {
        engineVersion: OCR_ENGINE_VERSION,
      })]);

      let rendered;
      try {
        rendered = await renderPageImage(pdfDoc, page.pageNumber);
      } catch {
        const code = "OCR_RENDER_FAILED";
        results.set(page.pageNumber, {
          status: "failed",
          attempted: true,
          blocks: [],
          reasons: [...assessed.reasons, code],
          errorCode: code,
        });
        await report([reportEntry(page, assessed, "failed", {
          errorCode: code,
          engineVersion: OCR_ENGINE_VERSION,
          reasons: [...assessed.reasons, code],
        })]);
        continue;
      }

      const image = preprocessForOcr(rendered.image);
      const renderHash = await hashImage(image);
      await report([reportEntry(page, assessed, "ocr_running", {
        engineVersion: OCR_ENGINE_VERSION,
        renderHash,
      })]);

      // 识别与上报分开:任务被取消时上报会失败,但已识别结果必须保留在内存中
      // 供取消方落库(salvage),不能把“上报失败”误记成 OCR 失败。
      let recognizedBlocks;
      let recognizedConfidence = 0;
      try {
        if (session === null) session = await createOcrSession();
        const recognized = await session.recognize(image, rendered.viewport);
        recognizedBlocks = recognized.blocks;
        recognizedConfidence = recognized.confidence;
      } catch (error) {
        const code = error instanceof OcrEngineError ? error.code : "OCR_FAILED";
        if (code === "OCR_ENGINE_UNAVAILABLE") engineFailure = code;
        results.set(page.pageNumber, {
          status: "failed",
          attempted: true,
          blocks: [],
          reasons: [...assessed.reasons, code],
          errorCode: code,
        });
        await report([reportEntry(page, assessed, "failed", {
          reasons: [...assessed.reasons, code],
          errorCode: code,
          renderHash,
          engineVersion: OCR_ENGINE_VERSION,
        })]);
        continue;
      }
      const blocks = recognizedBlocks;
      const low = recognizedConfidence < OCR_LOW_CONFIDENCE;
      const reasons = blocks.length === 0
        ? [...assessed.reasons, "no_text_recognized"]
        : low
          ? [...assessed.reasons, "low_confidence"]
          : assessed.reasons;
      const status = low && blocks.length > 0 ? "low_confidence" : "completed";
      results.set(page.pageNumber, {
        status,
        attempted: true,
        blocks,
        confidence: recognizedConfidence,
        reasons,
      });
      await report([reportEntry(page, assessed, status, {
        reasons,
        confidence: recognizedConfidence,
        blockCount: blocks.length,
        renderHash,
        engineVersion: OCR_ENGINE_VERSION,
      })]);
    }
  } finally {
    await session?.dispose();
  }
  return results;
}
