// 2D PDF 文本层质量检测(纯函数,不依赖 Canvas/网络)。
// 仅输出统计与触发原因;是否 OCR 由调用方按 reasons 决定,避免静默跳过。
import {
  groupTextItemsIntoLines,
  userSpaceBoundingRect,
  type PdfTextItem,
} from "../../modules/reader/format_adapters/pdf_text";

export type PdfPageQuality = {
  charCount: number;
  visibleChars: number;
  replacementChars: number;
  replacementRatio: number;
  whitespaceRatio: number;
  coverage: number;
  uniqueChars: number;
  lineCount: number;
  needsOcr: boolean;
  reasons: string[];
};

export const MIN_VISIBLE_CHARS = 20;
export const MAX_REPLACEMENT_RATIO = 0.02;
export const MIN_TEXT_COVERAGE = 0.001;
export const MIN_UNIQUE_CHARS = 4;

export function assessPdfPageQuality(input: {
  items: PdfTextItem[];
  width?: number;
  height?: number;
}): PdfPageQuality {
  const lines = groupTextItemsIntoLines(input.items);
  const rawText = lines.map((line) => line.text).join("");
  const visibleChars = rawText.replace(/\s+/g, "").length;
  const spacerChars = input.items.reduce(
    (sum, item) => sum + (item.str.match(/\s/g)?.length ?? 0),
    0,
  );
  const replacementChars = (rawText.match(/\uFFFD/g) ?? []).length;
  const uniqueChars = new Set(rawText.replace(/\s+/g, "")).size;

  let coverage = 0;
  if (input.width && input.height && input.width > 0 && input.height > 0) {
    const area = lines.reduce((sum, line) => {
      const rect = userSpaceBoundingRect([line]);
      return sum + Math.max(0, rect.width) * Math.max(0, rect.height);
    }, 0);
    coverage = Math.min(1, area / (input.width * input.height));
  }

  const reasons: string[] = [];
  if (lines.length === 0 || visibleChars === 0) {
    reasons.push("no_text_layer");
  } else {
    if (visibleChars < MIN_VISIBLE_CHARS) reasons.push("too_few_chars");
    if (replacementChars > 0 && replacementChars / Math.max(1, visibleChars) > MAX_REPLACEMENT_RATIO) {
      reasons.push("replacement_chars");
    }
    if (input.width && input.height && coverage < MIN_TEXT_COVERAGE && visibleChars < 200) {
      reasons.push("low_coverage");
    }
    if (visibleChars >= MIN_VISIBLE_CHARS && uniqueChars < MIN_UNIQUE_CHARS) {
      reasons.push("low_diversity");
    }
    if (visibleChars > 0 && spacerChars / Math.max(1, rawText.length) > 0.6) {
      reasons.push("excessive_whitespace");
    }
  }

  return {
    charCount: rawText.length,
    visibleChars,
    replacementChars,
    replacementRatio: replacementChars / Math.max(1, visibleChars),
    whitespaceRatio: spacerChars / Math.max(1, rawText.length),
    coverage,
    uniqueChars,
    lineCount: lines.length,
    needsOcr: reasons.length > 0,
    reasons,
  };
}
