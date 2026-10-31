// M03 阅读器类型:RawSelection、大纲与渲染块(《M03》§2/§4/§6)。
// RawSelection 是阅读器输出,不是可引用 Anchor;M05 负责映射与裁决。
import type { SourceLocator } from "../../shared/generated/SourceLocator";

export type ReaderFormat = "pdf" | "markdown" | "docx";

export type SelectionWarning =
  | "cross_page"
  | "cross_column"
  | "unsupported_region"
  | "order_uncertain";

/** 统一 RawSelection(《M03》§6,字段与设计文档逐字对齐)。 */
export type RawSelection = {
  documentId: string;
  documentVersionId: string;
  format: ReaderFormat;
  text: string;
  blocks: Array<{
    renderKey: string;
    text: string;
    blockOrder: number;
    locator: SourceLocator;
  }>;
  primaryLocator: SourceLocator;
  selectionKind: "text" | "page" | "section";
  warnings: Array<SelectionWarning>;
};

/** 顶层可见渲染块:DOM 选区 → 原文 offset 的映射载体。 */
export type RenderBlock = {
  renderKey: string;
  blockOrder: number;
  kind: "heading" | "paragraph" | "list_item" | "quote" | "code" | "table";
  text: string;
  /** 原文 UTF-16 code unit 偏移(Markdown)或 OOXML 正文序(DOCX)。 */
  startOffset: number;
  endOffset: number;
  headingPath: string[];
  locator: SourceLocator;
};

/** 显示大纲节点:原生/结构大纲,不是 AI 语义大纲。 */
export type OutlineNode = {
  key: string;
  title: string;
  level: number;
  /** 定位:markdown 偏移 / docx 序号 / pdf 页码。 */
  locator: SourceLocator;
  children: OutlineNode[];
};

/** M03 §8:阅读状态与 AI 状态分离的展示输入。 */
export type ReaderAiPhase =
  | "not_parsed"
  | "parsing"
  | "partial"
  | "ready"
  | "fts_only"
  | "failed"
  | "no_text_layer"
  /** 扫描件 OCR 已尝试但未识别到文字(不是通用解析失败)。 */
  | "ocr_no_text";

export type ReaderStateInput = {
  /** 文件可读且已渲染。 */
  readable: boolean;
  aiPhase: ReaderAiPhase;
  /** 文件缺失/损坏等原因描述(阅读错误,非 AI 状态)。 */
  fileError?: string;
};
