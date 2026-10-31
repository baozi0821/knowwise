// 阅读偏好(字号/行距/版心宽度)与 PDF 缩放记忆。
// 纯函数解析 + localStorage 持久化:损坏/未知值一律回退默认,不阻塞阅读。
// 阅读偏好经 CSS 变量作用于 Markdown/DOCX 正文容器(见 reading.css)。
import { useCallback, useState } from "react";
import type { CSSProperties } from "react";

/** 字号 3 档:舒适(大)/ 标准 / 紧凑(小)。 */
export type ReaderFontSize = "comfortable" | "standard" | "compact";
/** 行距 2 档:标准 / 宽松。 */
export type ReaderLineSpacing = "normal" | "relaxed";
/** 版心宽度 3 档:窄 / 中 / 宽。 */
export type ReaderMeasure = "narrow" | "medium" | "wide";

export type ReaderPrefs = {
  fontSize: ReaderFontSize;
  lineSpacing: ReaderLineSpacing;
  measure: ReaderMeasure;
};

export const READER_PREFS_KEY = "knowwise.reader.prefs";

export const DEFAULT_READER_PREFS: ReaderPrefs = {
  fontSize: "standard",
  lineSpacing: "normal",
  measure: "medium",
};

export const READER_FONT_SIZE_OPTIONS: ReadonlyArray<{
  value: ReaderFontSize;
  label: string;
}> = [
  { value: "comfortable", label: "舒适" },
  { value: "standard", label: "标准" },
  { value: "compact", label: "紧凑" },
];

export const READER_LINE_SPACING_OPTIONS: ReadonlyArray<{
  value: ReaderLineSpacing;
  label: string;
}> = [
  { value: "normal", label: "标准" },
  { value: "relaxed", label: "宽松" },
];

export const READER_MEASURE_OPTIONS: ReadonlyArray<{
  value: ReaderMeasure;
  label: string;
}> = [
  { value: "narrow", label: "窄" },
  { value: "medium", label: "中" },
  { value: "wide", label: "宽" },
];

/** 字号 → 正文 px(标准保持既有 14.5px,避免默认观感突变)。 */
export const READER_FONT_SIZE_PX: Record<ReaderFontSize, string> = {
  comfortable: "16.5px",
  standard: "14.5px",
  compact: "13px",
};

/** 行距 → 无单位 line-height。 */
export const READER_LINE_HEIGHT: Record<ReaderLineSpacing, string> = {
  normal: "1.8",
  relaxed: "2.05",
};

/** 版心宽度 → max-width。 */
export const READER_MEASURE_WIDTH: Record<ReaderMeasure, string> = {
  narrow: "38rem",
  medium: "46rem",
  wide: "56rem",
};

const FONT_SIZES: readonly ReaderFontSize[] = ["comfortable", "standard", "compact"];
const LINE_SPACINGS: readonly ReaderLineSpacing[] = ["normal", "relaxed"];
const MEASURES: readonly ReaderMeasure[] = ["narrow", "medium", "wide"];

function oneOf<T extends string>(value: unknown, allowed: readonly T[], fallback: T): T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value)
    ? (value as T)
    : fallback;
}

/**
 * 解析持久化的偏好 JSON:非法 JSON、缺字段、未知枚举值都按字段回退默认,
 * 保证旧版本/手改过的存储不会让阅读区失样。
 */
export function parseReaderPrefs(raw: string | null | undefined): ReaderPrefs {
  if (typeof raw !== "string" || raw.length === 0) return DEFAULT_READER_PREFS;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return DEFAULT_READER_PREFS;
  }
  if (typeof parsed !== "object" || parsed === null) return DEFAULT_READER_PREFS;
  const record = parsed as Record<string, unknown>;
  return {
    fontSize: oneOf(record.fontSize, FONT_SIZES, DEFAULT_READER_PREFS.fontSize),
    lineSpacing: oneOf(record.lineSpacing, LINE_SPACINGS, DEFAULT_READER_PREFS.lineSpacing),
    measure: oneOf(record.measure, MEASURES, DEFAULT_READER_PREFS.measure),
  };
}

/** 偏好 → 正文容器 CSS 变量(reading.css 消费)。 */
export function readerPrefsCssVars(prefs: ReaderPrefs): Record<string, string> {
  return {
    "--kw-reader-font-size": READER_FONT_SIZE_PX[prefs.fontSize],
    "--kw-reader-line-height": READER_LINE_HEIGHT[prefs.lineSpacing],
    "--kw-reader-measure": READER_MEASURE_WIDTH[prefs.measure],
  };
}

/** CSSProperties 类型不支持自定义属性,这里在边界处断言一次。 */
export function readerPrefsStyle(prefs: ReaderPrefs): CSSProperties {
  return readerPrefsCssVars(prefs) as CSSProperties;
}

/** 读取偏好(存储不可用时回退默认)。 */
export function loadReaderPrefs(): ReaderPrefs {
  try {
    return parseReaderPrefs(window.localStorage.getItem(READER_PREFS_KEY));
  } catch {
    return DEFAULT_READER_PREFS;
  }
}

/** 写入偏好(隐私模式等存储异常静默忽略,不影响本次会话)。 */
export function saveReaderPrefs(prefs: ReaderPrefs): void {
  try {
    window.localStorage.setItem(READER_PREFS_KEY, JSON.stringify(prefs));
  } catch {
    // 忽略存储不可用。
  }
}

/** 阅读偏好状态:ReaderRegion 持有,经 props 下发正文视图。 */
export function useReaderPrefs(): {
  prefs: ReaderPrefs;
  setPrefs: (patch: Partial<ReaderPrefs>) => void;
  resetPrefs: () => void;
} {
  const [prefs, setPrefsState] = useState<ReaderPrefs>(() => loadReaderPrefs());

  const setPrefs = useCallback((patch: Partial<ReaderPrefs>) => {
    setPrefsState((prev) => {
      const next: ReaderPrefs = {
        fontSize: patch.fontSize ?? prev.fontSize,
        lineSpacing: patch.lineSpacing ?? prev.lineSpacing,
        measure: patch.measure ?? prev.measure,
      };
      saveReaderPrefs(next);
      return next;
    });
  }, []);

  const resetPrefs = useCallback(() => {
    setPrefsState(DEFAULT_READER_PREFS);
    saveReaderPrefs(DEFAULT_READER_PREFS);
  }, []);

  return { prefs, setPrefs, resetPrefs };
}

// —— PDF 缩放与「适应宽度」记忆(按文档保存) ——

export type PdfZoomState = {
  scale: number;
  fitMode: boolean;
};

export const PDF_ZOOM_KEY_PREFIX = "knowwise.reader.pdfZoom.";

/** 缩放合法区间与 PDF 阅读器按钮一致(50%–300%)。 */
export const PDF_SCALE_MIN = 0.5;
export const PDF_SCALE_MAX = 3;

export function pdfZoomStorageKey(documentId: string): string {
  return `${PDF_ZOOM_KEY_PREFIX}${documentId}`;
}

/** 解析缩放记忆:非法/越界值 → null(调用方回退默认 125%)。 */
export function parsePdfZoom(raw: string | null | undefined): PdfZoomState | null {
  if (typeof raw !== "string" || raw.length === 0) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const record = parsed as Record<string, unknown>;
  if (typeof record.scale !== "number" || !Number.isFinite(record.scale)) return null;
  if (record.scale < PDF_SCALE_MIN || record.scale > PDF_SCALE_MAX) return null;
  return { scale: record.scale, fitMode: record.fitMode === true };
}

/** 读取某文档的缩放记忆;无文档/无记忆/存储不可用 → null。 */
export function loadPdfZoom(documentId: string | null | undefined): PdfZoomState | null {
  if (documentId === null || documentId === undefined || documentId.length === 0) return null;
  try {
    return parsePdfZoom(window.localStorage.getItem(pdfZoomStorageKey(documentId)));
  } catch {
    return null;
  }
}

/** 保存某文档的缩放记忆(同一文档重开保持)。 */
export function savePdfZoom(documentId: string | null | undefined, state: PdfZoomState): void {
  if (documentId === null || documentId === undefined || documentId.length === 0) return;
  try {
    window.localStorage.setItem(pdfZoomStorageKey(documentId), JSON.stringify(state));
  } catch {
    // 忽略存储不可用。
  }
}
