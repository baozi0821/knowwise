import type { PdfRect } from "../../shared/generated/PdfRect";
import type { PageOcrStatus } from "../../shared/generated/PageOcrStatus";

export const OCR_ENGINE_VERSION = "tesseract.js/6.0.1+core/6.1.2";
export const OCR_DPI = 300;
export const OCR_LOW_CONFIDENCE = 0.6;

export type OcrBlock = {
  text: string;
  bbox: PdfRect;
  confidence: number;
  kind: "heading" | "paragraph";
};

export type OcrPageResult = {
  status: Extract<PageOcrStatus, "completed" | "low_confidence" | "failed" | "skipped">;
  attempted: boolean;
  blocks: OcrBlock[];
  confidence?: number;
  reasons: string[];
  errorCode?: string;
};

export type PageOcrReportEntry = {
  pageNumber: number;
  status: PageOcrStatus;
  quality?: unknown;
  reasons: string[];
  renderHash?: string;
  engineVersion?: string;
  confidence?: number;
  blockCount: number;
  errorCode?: string;
};
