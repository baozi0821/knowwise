// M04 PDF 解析 Worker(《M04》§4):页面文本项 → 行 → 段落块。
// 几何算法在 M03 纯格式层(pdf_text);2D 页级 OCR 结果在此合并:
// OCR 页产出 origin=ocr 的块并保留 bbox/置信度,失败页可见且不伪造文本。
import {
  detectColumns,
  groupTextItemsIntoLines,
  isHeadingCandidate,
  mergeLinesIntoBlocks,
  userSpaceBoundingRect,
  normalizeRotation,
} from "../../modules/reader/format_adapters/pdf_text";
import type { PdfTextItem } from "../../modules/reader/format_adapters/pdf_text";
import type { ParserBlockInput } from "../../shared/generated/ParserBlockInput";
import type { ParserRegionInput } from "../../shared/generated/ParserRegionInput";
import type { OcrPageResult } from "../ocr/types";

export type PdfPageInput = {
  pageNumber: number;
  items: PdfTextItem[];
  rotation: number;
  /** scale=1 页面尺寸(user space 点);用于文本覆盖率检测。 */
  width?: number;
  height?: number;
  /** 2D 页级 OCR 结果;skipped 表示文本层合格。 */
  ocr?: OcrPageResult;
};

export type PdfParseOutput = {
  blocks: ParserBlockInput[];
  regions: ParserRegionInput[];
  orderConfidence: "high" | "low";
};

let globalOrdinal = 0;
/** 当前章节标题路径(跨页延续):PDF 标题识别后供后续块继承。 */
let headingStack: string[] = [];

function currentHeadingPath(): string[] {
  return [...headingStack];
}

function observeHeading(kind: string, text: string): string[] {
  if (kind !== "heading") return currentHeadingPath();
  const title = text.replace(/\s+/g, " ").trim();
  if (title) headingStack = [title];
  return currentHeadingPath();
}

function pageRegion(
  pageNumber: number,
  status: ParserRegionInput["status"],
  blockCount: number,
  orderConfidence: "high" | "low",
  errorCode?: string,
): ParserRegionInput {
  return {
    regionKey: `page:${pageNumber}`,
    regionKind: "page",
    status,
    blockCount,
    orderConfidence,
    errorCode,
  };
}

function textLayerBlocks(
  page: PdfPageInput,
  lines: ReturnType<typeof groupTextItemsIntoLines>,
): ParserBlockInput[] {
  const blocks = mergeLinesIntoBlocks(lines);
  const bodyFont =
    blocks
      .map((b) => b.fontSize)
      .sort((a, b) => a - b)[Math.floor(blocks.length / 2)] ?? 10;
  return blocks.map((b) => {
    const rect = b.rect;
    const ordinal = globalOrdinal;
    globalOrdinal += 1;
    const kind = isHeadingCandidate(b, bodyFont) ? "heading" : "paragraph";
    const headingPath = observeHeading(kind, b.text);
    return {
      blockKey: `pdf:p${page.pageNumber}@${Math.round(rect.y)}x${Math.round(rect.x)}`,
      ordinal,
      parentBlockKey: undefined,
      kind,
      text: b.text,
      headingPath,
      locator: {
        kind: "pdf",
        pageNumber: page.pageNumber,
        rects: [rect],
        pageRotation: normalizeRotation(page.rotation),
        coordinateSpace: "pdf_user_space_v1",
      },
      completeness: "complete",
      indexable: true,
      origin: "parser",
    };
  });
}

function ocrBlocks(page: PdfPageInput, ocr: OcrPageResult): ParserBlockInput[] {
  return ocr.blocks.map((block, index) => {
    const ordinal = globalOrdinal;
    globalOrdinal += 1;
    const headingPath = observeHeading(block.kind, block.text);
    return {
      blockKey: `ocr:p${page.pageNumber}:l${index}`,
      ordinal,
      parentBlockKey: undefined,
      kind: block.kind,
      text: block.text,
      headingPath,
      locator: {
        kind: "pdf",
        pageNumber: page.pageNumber,
        rects: [block.bbox],
        pageRotation: normalizeRotation(page.rotation),
        coordinateSpace: "pdf_user_space_v1",
      },
      completeness: "complete",
      indexable: true,
      origin: "ocr",
      ocrConfidence: block.confidence,
    };
  });
}

/**
 * 解析一页:文本层优先;质量不合格且 OCR 成功时使用 OCR 块。
 * OCR 失败但文本层仍有内容 → partial + 可见错误;两者都无 → failed/no_text_layer。
 */
export function parsePdfPage(page: PdfPageInput): PdfParseOutput {
  const lines = groupTextItemsIntoLines(page.items);
  const { orderConfidence } = detectColumns(lines);
  const ocr = page.ocr;
  const useOcr = ocr?.attempted === true && ocr.blocks.length > 0;

  if (useOcr) {
    const parsed = ocrBlocks(page, ocr);
    return {
      blocks: parsed,
      regions: [
        pageRegion(
          page.pageNumber,
          "complete",
          parsed.length,
          "high",
          ocr.errorCode,
        ),
      ],
      orderConfidence: "high",
    };
  }

  if (lines.length === 0) {
    const failedOcr = ocr?.attempted === true;
    return {
      blocks: [],
      regions: [
        failedOcr
          ? pageRegion(
              page.pageNumber,
              "failed",
              0,
              "high",
              ocr?.errorCode ?? "OCR_FAILED",
            )
          : pageRegion(page.pageNumber, "no_text_layer", 0, "high", "PARSE_NO_TEXT_LAYER"),
      ],
      orderConfidence: "high",
    };
  }

  const parsed = textLayerBlocks(page, lines);
  const ocrFailed = ocr?.attempted === true;
  return {
    blocks: parsed,
    regions: [
      pageRegion(
        page.pageNumber,
        ocrFailed ? "partial" : "complete",
        parsed.length,
        orderConfidence,
        ocrFailed ? (ocr?.errorCode ?? "OCR_FAILED") : undefined,
      ),
    ],
    orderConfidence,
  };
}

/** 重置序号(新代次/新 Worker 运行)。 */
export function resetPdfOrdinal(): void {
  globalOrdinal = 0;
  headingStack = [];
}

export { userSpaceBoundingRect };
