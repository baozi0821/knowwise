// M04 Markdown 解析 Worker(《M04》§5):复用 M03 纯格式层。
// 顶层标题/段落/列表项/引用/代码块/表格成块;代码中的 # 不是标题;
// 块 ordinal 按源文件顺序;blockKey = 标题路径 + 类型 + offset。
import { markdownBlocks } from "../../modules/reader/format_adapters/markdown_adapter";
import type { BlockKind } from "../../shared/generated/BlockKind";
import type { ParserBlockInput } from "../../shared/generated/ParserBlockInput";
import type { ParserRegionInput } from "../../shared/generated/ParserRegionInput";

const KIND_MAP: Record<string, BlockKind> = {
  heading: "heading",
  paragraph: "paragraph",
  list_item: "list_item",
  quote: "quote",
  code: "code",
  table: "table",
};

export function parseMarkdownToBlocks(md: string): ParserBlockInput[] {
  return markdownBlocks(md).map((b) => ({
    blockKey: b.renderKey,
    ordinal: b.blockOrder,
    parentBlockKey: undefined,
    kind: KIND_MAP[b.kind] ?? "paragraph",
    text: b.text,
    headingPath: b.headingPath,
    locator: b.locator,
    completeness: "complete",
    indexable: true,
    origin: "parser",
  }));
}

/** Markdown 覆盖区域:单一正文区(《M04》§8)。 */
export function markdownRegion(blockCount: number): ParserRegionInput {
  return {
    regionKey: "body:1",
    regionKind: "body",
    status: "complete",
    blockCount,
    orderConfidence: "high",
    errorCode: undefined,
  };
}
