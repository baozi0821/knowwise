// M03 统一 RawSelection 构造(《M03》§6):阅读器输出,不是可引用 Anchor。
// 跨页选择按页拆分并保持页序;解析顺序不确定时给出 order_uncertain 警告。
import type { RawSelection, ReaderFormat, SelectionWarning } from "./types";
import type { SourceLocator } from "../../shared/generated/SourceLocator";

export type SelectionBlockEntry = {
  renderKey: string;
  text: string;
  blockOrder: number;
  locator: SourceLocator;
};

export type BuildSelectionInput = {
  documentId: string;
  documentVersionId: string;
  format: ReaderFormat;
  blocks: SelectionBlockEntry[];
  selectionKind?: RawSelection["selectionKind"];
  extraWarnings?: SelectionWarning[];
};

/** 块序是否连续(跨块选择的顺序确定性)。 */
function orderIsCertain(orders: number[]): boolean {
  const sorted = [...orders].sort((a, b) => a - b);
  return sorted.every((v, i) => (i === 0 ? true : v === sorted[i - 1]! + 1));
}

/** 是否跨页(定位主页面号不同)或跨栏(composite 多 part)。 */
function collectWarnings(blocks: SelectionBlockEntry[]): SelectionWarning[] {
  const warnings = new Set<SelectionWarning>();
  const pages = blocks
    .map((b) => b.locator)
    .map((l) => (l.kind === "pdf" ? l.pageNumber : null))
    .filter((p): p is number => p !== null);
  if (new Set(pages).size > 1) warnings.add("cross_page");
  const composites = blocks.filter((b) => b.locator.kind === "composite");
  if (composites.length > 0) warnings.add("cross_column");
  if (!orderIsCertain(blocks.map((b) => b.blockOrder))) {
    warnings.add("order_uncertain");
  }
  return [...warnings];
}

export function buildRawSelection(
  input: BuildSelectionInput,
): RawSelection {
  if (input.blocks.length === 0) {
    throw new Error("RawSelection requires at least one block");
  }
  const ordered = [...input.blocks].sort((a, b) => a.blockOrder - b.blockOrder);
  const warnings = new Set<SelectionWarning>(collectWarnings(ordered));
  for (const w of input.extraWarnings ?? []) warnings.add(w);
  return {
    documentId: input.documentId,
    documentVersionId: input.documentVersionId,
    format: input.format,
    text: ordered.map((b) => b.text).join("\n"),
    blocks: ordered.map((b) => ({
      renderKey: b.renderKey,
      text: b.text,
      blockOrder: b.blockOrder,
      locator: b.locator,
    })),
    primaryLocator: ordered[0]!.locator,
    selectionKind: input.selectionKind ?? "text",
    warnings: [...warnings],
  };
}

// —— 划词「复制为引用」:Markdown 引用块(引文 + 《文档名》· 页码/章节)——

export type CitationInput = {
  /** 选中的原文(可多行)。 */
  quote: string;
  /** 文档名(displayName);为空时用「未命名文档」。 */
  documentTitle?: string | null;
  /** PDF 页码(1 基);与 section 同时给出时优先页码。 */
  pageNumber?: number | null;
  /** Markdown/DOCX 章节名(选区所在标题路径末项)。 */
  section?: string | null;
};

/** 出处定位文案:PDF 用「第 N 页」,MD/DOCX 用章节名;都没有则省略。 */
function citationSourceLabel(input: CitationInput): string {
  if (typeof input.pageNumber === "number" && Number.isFinite(input.pageNumber)) {
    return `第 ${Math.max(1, Math.floor(input.pageNumber))} 页`;
  }
  const section = input.section?.trim();
  return section !== undefined && section.length > 0 ? section : "";
}

/**
 * 生成 Markdown 引用块:
 * `> 引文原文` + 空行 + `— 《文档名》· 第 86 页`(或章节名)。
 */
export function buildCitationMarkdown(input: CitationInput): string {
  const quoteLines = input.quote
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  const quoted = (quoteLines.length > 0 ? quoteLines : [""])
    .map((line) => (line.length > 0 ? `> ${line}` : ">"))
    .join("\n");
  const title = input.documentTitle?.trim() ?? "";
  const attribution = title.length > 0 ? `《${title}》` : "《未命名文档》";
  const source = citationSourceLabel(input);
  return source.length > 0
    ? `${quoted}\n\n— ${attribution}· ${source}`
    : `${quoted}\n\n— ${attribution}`;
}

/** 跨页 PDF 选区按页拆分(保持 PDF 页序,《M03》§3)。 */
export function splitSelectionByPage(  selection: RawSelection,
): Map<number, RawSelection> {
  const byPage = new Map<number, SelectionBlockEntry[]>();
  for (const b of selection.blocks) {
    const page =
      b.locator.kind === "pdf" ? b.locator.pageNumber : 0;
    const list = byPage.get(page) ?? [];
    list.push({
      renderKey: b.renderKey,
      text: b.text,
      blockOrder: b.blockOrder,
      locator: b.locator,
    });
    byPage.set(page, list);
  }
  const out = new Map<number, RawSelection>();
  for (const [page, blocks] of byPage) {
    out.set(
      page,
      buildRawSelection({
        documentId: selection.documentId,
        documentVersionId: selection.documentVersionId,
        format: selection.format,
        blocks,
        extraWarnings: [...selection.warnings, "cross_page"],
      }),
    );
  }
  return out;
}
