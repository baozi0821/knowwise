// M03/M04 Markdown 纯格式层(《M03》§4、《M04》§5)。
// mdast + source positions:块按源文件顺序编号;代码中的 `#` 不是标题
// (mdast 语法保证);重复标题以位置区分,不依赖标题文本唯一。
// 软换行、代码块与表格单元格不得被合并到无法回溯的纯文本。
import { fromMarkdown } from "mdast-util-from-markdown";
import type {
  Code,
  Heading,
  List,
  Paragraph,
  Blockquote,
  Root,
  Table,
} from "mdast";
import { gfmFromMarkdown } from "mdast-util-gfm";
import { gfm } from "micromark-extension-gfm";
import type { OutlineNode, RenderBlock } from "../types";
import type { SourceLocator } from "../../../shared/generated/SourceLocator";

/** 提取节点的直接文本(行内节点取 value,其余为空;mdast 类型宽松匹配)。 */
const textOf = (nodes: ReadonlyArray<unknown>): string =>
  nodes
    .map((n) => {
      if (
        n !== null &&
        typeof n === "object" &&
        "value" in n &&
        typeof (n as { value: unknown }).value === "string"
      ) {
        return (n as { value: string }).value;
      }
      return "";
    })
    .join("");

const BLOCK_KINDS = new Set([
  "heading",
  "paragraph",
  "list",
  "blockquote",
  "code",
  "table",
]);

/** 解析选项:允许测试注入扩展;默认启用 GFM(表格/删除线/任务列表)。 */
export function parseMarkdown(md: string): Root {
  return fromMarkdown(md, {
    extensions: [gfm()],
    mdastExtensions: [gfmFromMarkdown()],
  }) as Root;
}

/** 重复标题消歧:相同文本按出现次序编号("第二章#2"),位置区分而非文本唯一。 */
export function headingPathOf(stack: Array<{ title: string; seen: number }>): string[] {
  return stack.map((h) =>
    h.seen > 1 ? `${h.title}#${h.seen}` : h.title,
  );
}

/**
 * 顶层可见块提取(《M04》§5):标题/段落/列表项/引用/代码块/表格成块;
 * blockKey = 标题路径 + 节点类型 + 源 offset 的规范化字符串。
 */
export function markdownBlocks(md: string): RenderBlock[] {
  const tree = parseMarkdown(md);
  const blocks: RenderBlock[] = [];
  const headingSeen = new Map<string, number>();
  const stack: Array<{ title: string; level: number; seen: number }> = [];

  const popTo = (level: number) => {
    while (stack.length > 0 && stack[stack.length - 1]!.level >= level) {
      stack.pop();
    }
  };

  let order = 0;
  const pushBlock = (
    node: Heading | Paragraph | Blockquote | Code | List | Table,
    kind: RenderBlock["kind"],
    text: string,
  ) => {
    const start = node.position?.start.offset ?? 0;
    const end = node.position?.end.offset ?? text.length + start;
    const headingPath = headingPathOf(stack);
    blocks.push({
      renderKey: `${headingPath.join("/")}|${kind}@${start}`,
      blockOrder: order,
      kind,
      text,
      startOffset: start,
      endOffset: end,
      headingPath,
      locator: {
        kind: "markdown",
        startOffset: start,
        endOffset: end,
        mdastNodeType: node.type,
        headingPath,
        locatorVersion: 1,
      },
    });
    order += 1;
  };

  const flattenListItem = (list: List): string =>
    list.children
      .map((item, i) => {
        const itemText = item.children
          .map((c) => textOf((c as { children?: unknown[] }).children ?? []))
          .join(" ")
          .trim();
        return `${i + 1}. ${itemText}`;
      })
      .join("\n");

  for (const node of tree.children) {
    if (!BLOCK_KINDS.has(node.type)) continue;
    switch (node.type) {
      case "heading": {
        const h = node as Heading;
        popTo(h.depth);
        const seen = (headingSeen.get(textOf(h.children)) ?? 0) + 1;
        const title = textOf(h.children).trim() || "(空标题)";
        headingSeen.set(title, seen);
        stack.push({ title, level: h.depth, seen });
        pushBlock(h, "heading", title);
        break;
      }
      case "paragraph":
        pushBlock(node as Paragraph, "paragraph", textOf((node as Paragraph).children));
        break;
      case "blockquote":
        pushBlock(node as Blockquote, "quote", textOf((node as Blockquote).children));
        break;
      case "code":
        // 代码块整块保存:内部 `#` 不识别为标题(mdast 语法层保证)。
        pushBlock(node as Code, "code", (node as Code).value);
        break;
      case "list":
        pushBlock(node as List, "list_item", flattenListItem(node as List));
        break;
      case "table": {
        const t = node as Table;
        // 表格保留行列结构:文本形态 `cell|cell` 行列分明,可回溯到单元格。
        const rows = t.children.map((row) =>
          row.children.map((cell) => textOf(cell.children)).join("|"),
        );
        pushBlock(t, "table", rows.join("\n"));
        break;
      }
      default:
        break;
    }
  }
  return blocks;
}

/** 标题路径大纲(M03 §4):重复标题带序号;level 嵌套由栈维护。 */
export function markdownOutline(md: string): OutlineNode[] {
  const tree = parseMarkdown(md);
  const seen = new Map<string, number>();
  const roots: OutlineNode[] = [];
  const stack: OutlineNode[] = [];
  for (const node of tree.children) {
    if (node.type !== "heading") continue;
    const h = node as Heading;
    const rawTitle =
      textOf(h.children).trim() ||
      "(空标题)";
    const count = (seen.get(rawTitle) ?? 0) + 1;
    seen.set(rawTitle, count);
    const title = count > 1 ? `${rawTitle}#${count}` : rawTitle;
    const start = h.position?.start.offset ?? 0;
    const end = h.position?.end.offset ?? start;
    const outline: OutlineNode = {
      key: `h@${start}`,
      title,
      level: h.depth,
      locator: {
        kind: "markdown",
        startOffset: start,
        endOffset: end,
        mdastNodeType: "heading",
        headingPath: [],
        locatorVersion: 1,
      },
      children: [],
    };
    while (stack.length > 0 && stack[stack.length - 1]!.level >= h.depth) {
      stack.pop();
    }
    if (stack.length === 0) {
      roots.push(outline);
    } else {
      stack[stack.length - 1]!.children.push(outline);
    }
    stack.push(outline);
  }
  return roots;
}

/** 当前章节候选:视口顶部最近的标题(阅读器只报告候选,不冻结上下文)。 */
export function currentHeadingCandidate(
  blocks: RenderBlock[],
  viewportTopOffset: number,
): RenderBlock | null {
  let candidate: RenderBlock | null = null;
  for (const b of blocks) {
    if (b.kind === "heading" && b.startOffset <= viewportTopOffset) {
      candidate = b;
    } else if (b.startOffset > viewportTopOffset) {
      break;
    }
  }
  return candidate;
}

/** 从渲染块集合查找包含偏移的块(位置恢复的精确路径)。 */
export function blockContainingOffset(
  blocks: RenderBlock[],
  offset: number,
): RenderBlock | null {
  return (
    blocks.find((b) => offset >= b.startOffset && offset <= b.endOffset) ?? null
  );
}

export function locatorOfBlock(b: RenderBlock): SourceLocator {
  return b.locator;
}
