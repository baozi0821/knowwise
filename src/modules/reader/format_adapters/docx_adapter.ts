// M03/M04 DOCX 纯格式层(《M03》§5、《M04》§6)。
// Mammoth 输出受控 HTML → DOMPurify 白名单清洗 → 正文块(docxOrdinal)。
// 不追求 Word 分页保真;受限对象(文本框/修订/批注/复杂表格)必须可见。
import DOMPurify from "dompurify";
import type { OutlineNode, RenderBlock } from "../types";

/** Mammoth 转换结果的投影(测试可构造)。 */
export type MammothResult = {
  value: string;
  messages: Array<{ type: string; message: string }>;
};

/** 白名单标签;raw HTML 一律剔除(默认不解析,KEEP_CONTENT=false)。 */
const ALLOWED_TAGS = [
  // 覆盖 ALLOWED_TAGS 时必须显式放行文本节点,否则 KEEP_CONTENT:false
  // 会连同正文一起剔除(DOMPurify 行为)。
  "#text",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "p",
  "ul",
  "ol",
  "li",
  "blockquote",
  "table",
  "tr",
  "td",
  "th",
  "pre",
  "code",
  "strong",
  "em",
];

export type DocxUnsupportedRegion = {
  regionKey: string;
  objectType: string;
  reasonCode: string;
};

export type DocxAdapted = {
  blocks: RenderBlock[];
  unsupported: DocxUnsupportedRegion[];
};

/**
 * 受限对象识别(《M03》§5):Mammoth 对文本框、批注、修订等输出 warning
 * 消息;每条 warning 投影为受限区域,不得显示成"无内容"。
 */
function unsupportedFromMessages(
  messages: MammothResult["messages"],
): DocxUnsupportedRegion[] {
  const out: DocxUnsupportedRegion[] = [];
  messages
    .filter((m) => m.type === "warning")
    .forEach((m, n) => {
      const objectType = /text box|文本框/i.test(m.message)
        ? "textbox"
        : /comment|批注/i.test(m.message)
          ? "comment"
          : /revision|修订|track change/i.test(m.message)
            ? "revision"
            : /image|图片/i.test(m.message)
              ? "image"
              : "complex_object";
      out.push({
        regionKey: `docx:unsupported:${n}`,
        objectType,
        reasonCode: "UNSUPPORTED_OBJECT",
      });
    });
  return out;
}

/**
 * DOCX 适配主入口。每个正文块获得连续 docxOrdinal,与 M04 的 OOXML
 * 顺序匹配(《M03》§5)。DOM 解析环境由运行时提供(主窗口或 jsdom)。
 */
export function adaptDocxHtml(result: MammothResult): DocxAdapted {
  const clean = DOMPurify.sanitize(result.value, {
    ALLOWED_TAGS,
    ALLOWED_ATTR: [],
    KEEP_CONTENT: false,
  });
  const unsupported = unsupportedFromMessages(result.messages);
  const doc = new DOMParser().parseFromString(clean, "text/html");
  const blocks: RenderBlock[] = [];
  const headingSeen = new Map<string, number>();
  const stack: Array<{ title: string; level: number; seen: number }> = [];
  let ordinal = 0;

  const headingPath = () =>
    stack.map((h) => (h.seen > 1 ? `${h.title}#${h.seen}` : h.title));

  const locatorOf = (): RenderBlock["locator"] => ({
    kind: "docx",
    bodyBlockStart: ordinal,
    bodyBlockEnd: ordinal,
    // w14:paraId 需 OOXML 预检匹配;Mammoth 路径不伪造段落 ID(《01》§5)。
    paragraphIds: [],
    headingPath: headingPath(),
    locatorVersion: 1,
  });

  const push = (kind: RenderBlock["kind"], text: string) => {
    const trimmed = text.replace(/\s+/g, " ").trim();
    if (!trimmed) return;
    blocks.push({
      renderKey: `docx@${ordinal}`,
      blockOrder: ordinal,
      kind,
      text: trimmed,
      startOffset: ordinal,
      endOffset: ordinal + 1,
      headingPath: headingPath(),
      locator: locatorOf(),
    });
    ordinal += 1;
  };

  const pushHeading = (text: string, level: number) => {
    const trimmed = text.replace(/\s+/g, " ").trim() || "(空标题)";
    const seen = (headingSeen.get(trimmed) ?? 0) + 1;
    headingSeen.set(trimmed, seen);
    while (stack.length > 0 && stack[stack.length - 1]!.level >= level) {
      stack.pop();
    }
    stack.push({ title: trimmed, level, seen });
    push("heading", trimmed);
  };

  const walk = (el: Element) => {
    for (const child of Array.from(el.children)) {
      const tag = child.tagName.toLowerCase();
      switch (tag) {
        case "h1":
        case "h2":
        case "h3":
        case "h4":
        case "h5":
        case "h6":
          pushHeading(child.textContent ?? "", Number(tag[1]));
          break;
        case "p":
          push("paragraph", child.textContent ?? "");
          break;
        case "blockquote":
          push("quote", child.textContent ?? "");
          break;
        case "pre":
          push("code", child.textContent ?? "");
          break;
        case "ul":
        case "ol":
          for (const li of Array.from(child.children)) {
            if (li.tagName.toLowerCase() === "li") {
              push("list_item", li.textContent ?? "");
            }
          }
          break;
        case "table": {
          // 表格保留行列:行内单元格以 | 分隔;合并单元格保持明确分隔。
          const rows = Array.from(child.querySelectorAll("tr"));
          const text = rows
            .map((tr) =>
              Array.from(tr.children)
                .map((td) => (td.textContent ?? "").replace(/\s+/g, " ").trim())
                .join("|"),
            )
            .join("\n");
          push("table", text);
          break;
        }
        default:
          // 白名单内的行内容器(strong/em/code)下钻;其余被清洗剔除。
          if (child.children.length > 0) walk(child);
          break;
      }
    }
  };
  walk(doc.body);

  return { blocks, unsupported };
}

/** DOCX 标题大纲(标题取 headingPath 末段,重复标题带序号)。 */
export function docxOutline(blocks: RenderBlock[]): OutlineNode[] {
  const roots: OutlineNode[] = [];
  const stack: OutlineNode[] = [];
  for (const b of blocks) {
    if (b.kind !== "heading") continue;
    const level = b.headingPath.length || 1;
    const node: OutlineNode = {
      key: b.renderKey,
      title: b.headingPath[b.headingPath.length - 1] ?? b.text,
      level,
      locator: b.locator,
      children: [],
    };
    while (stack.length > 0 && stack[stack.length - 1]!.level >= level) {
      stack.pop();
    }
    if (stack.length === 0) roots.push(node);
    else stack[stack.length - 1]!.children.push(node);
    stack.push(node);
  }
  return roots;
}
