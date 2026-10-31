// M04 DOCX 解析 Worker(《M04》§6):Mammoth HTML → 受控块。
// docxOrdinal 与 M03 渲染层的内部序号一致(同源计算);
// 受限对象(文本框/修订/批注)经 UnsupportedRegion 可见,不静默丢弃。
import { adaptDocxHtml } from "../../modules/reader/format_adapters/docx_adapter";
import type { BlockKind } from "../../shared/generated/BlockKind";
import type { ParserBlockInput } from "../../shared/generated/ParserBlockInput";
import type { ParserRegionInput } from "../../shared/generated/ParserRegionInput";
import type { ParserUnsupportedInput } from "../../shared/generated/ParserUnsupportedInput";

const KIND_MAP: Record<string, BlockKind> = {
  heading: "heading",
  paragraph: "paragraph",
  list_item: "list_item",
  quote: "quote",
  code: "code",
  table: "table",
};

export type DocxParseOutput = {
  blocks: ParserBlockInput[];
  regions: ParserRegionInput[];
  unsupported: ParserUnsupportedInput[];
};

export function parseDocxHtml(
  html: string,
  messages: Array<{ type: string; message: string }>,
): DocxParseOutput {
  const adapted = adaptDocxHtml({ value: html, messages });
  const blocks: ParserBlockInput[] = adapted.blocks.map((b) => ({
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
  const unsupported: ParserUnsupportedInput[] = adapted.unsupported.map((u) => ({
    regionKey: u.regionKey,
    objectType: u.objectType,
    locator: {
      kind: "docx",
      bodyBlockStart: -1,
      bodyBlockEnd: -1,
      paragraphIds: [],
      headingPath: [],
      locatorVersion: 1,
    },
    reasonCode: u.reasonCode,
  }));
  return {
    blocks,
    regions: [
      {
        regionKey: "body:1",
        regionKind: "body",
        status: "complete",
        blockCount: blocks.length,
        orderConfidence: "high",
        errorCode: undefined,
      },
    ],
    unsupported,
  };
}
