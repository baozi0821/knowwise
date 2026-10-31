// M03/M04 PDF 文本几何纯函数层(《M03》§3、《M04》§4)。
// 输入为 PDF.js TextItem 投影:{str, transform, width, height, dir};
// 全部基于 user space 坐标,不依赖 Canvas、缩放或 DPR。
import type { PdfRect } from "../../../shared/generated/PdfRect";
import type { PageRotation } from "../../../shared/generated/PageRotation";

/** PDF.js TextItem 的最小投影(测试可构造,无需运行 PDF.js)。 */
export type PdfTextItem = {
  str: string;
  /** transform = [a,b,c,d,e,f];e/f 为基线起点 user space 坐标。 */
  transform: [number, number, number, number, number, number];
  width: number;
  height: number;
  dir: "ltr" | "rtl";
};

export type PdfLine = {
  items: PdfTextItem[];
  y: number;
  x0: number;
  x1: number;
  fontSize: number;
  text: string;
};

export type PdfTextBlock = {
  text: string;
  lines: PdfLine[];
  rect: PdfRect;
  /** 标题候选评分(字体大小/短行等综合),由段落合并阶段填写。 */
  fontSize: number;
};

/** 文本项 → 基线 y、x 区间与有效字号。 */
function itemGeometry(item: PdfTextItem) {
  const [, , , , x, y] = item.transform;
  // transform[3] (d) 近似字体缩放;PDF.js 报告的 height 亦可作行高。
  const fontSize = Math.abs(item.transform[3]) || item.height || 10;
  return { x, y, fontSize };
}

/**
 * 文本项归行(《M04》§4):按 y 容差分桶,行内按 x 排序保序;
 * 同基线上的大水平间隙(栏间距)切分为独立行,保留左右栏结构。
 */
export function groupTextItemsIntoLines(
  items: PdfTextItem[],
  opts: { tolerance?: number; columnGap?: number } = {},
): PdfLine[] {
  const tolerance = opts.tolerance ?? 2.5;
  const columnGap = opts.columnGap ?? 60;
  const rows: PdfLine[] = [];
  for (const item of items) {
    if (!item.str.trim()) continue;
    const { x, y, fontSize } = itemGeometry(item);
    const row = rows.find(
      (r) => Math.abs(r.y - y) <= Math.max(tolerance, fontSize * 0.35),
    );
    if (row) {
      row.items.push(item);
      row.x0 = Math.min(row.x0, x);
      row.x1 = Math.max(row.x1, x + item.width);
      row.fontSize = Math.max(row.fontSize, fontSize);
    } else {
      rows.push({
        items: [item],
        y,
        x0: x,
        x1: x + item.width,
        fontSize,
        text: "",
      });
    }
  }
  // 行内排序 + 栏间隙切分:同基线、水平间隙超过阈值 → 分栏两行。
  const out: PdfLine[] = [];
  for (const row of rows) {
    row.items.sort((a, b) => a.transform[4] - b.transform[4]);
    let segment: PdfTextItem[] = [];
    const flushSeg = () => {
      if (segment.length === 0) return;
      const xs = segment.flatMap((i) => [
        i.transform[4],
        i.transform[4] + i.width,
      ]);
      // PDF.js item 自带必要空格;直接拼接(中文项目间不注入空格)。
      const text = segment
        .map((i) => i.str)
        .join("")
        .replace(/\s+/g, " ")
        .trim();
      if (text.length > 0) {
        out.push({
          items: segment,
          y: row.y,
          x0: Math.min(...xs),
          x1: Math.max(...xs),
          fontSize: row.fontSize,
          text,
        });
      }
      segment = [];
    };
    for (const item of row.items) {
      if (segment.length > 0) {
        const prev = segment[segment.length - 1]!;
        const gap = item.transform[4] - (prev.transform[4] + prev.width);
        if (gap > columnGap) flushSeg();
      }
      segment.push(item);
    }
    flushSeg();
  }
  // 行序自上而下(y 递减);同 y 按栏序(x 升序,左栏优先)。
  out.sort((a, b) => (b.y - a.y) || (a.x0 - b.x0));
  return out.filter((r) => r.text.length > 0);
}

/**
 * 双栏检测(《M04》§4):按行 x 区间聚类为列;
 * 存在跨栏行(宽度覆盖多列)或列间距不足 → order_confidence=low。
 */
export function detectColumns(lines: PdfLine[]): {
  columns: number;
  orderConfidence: "high" | "low";
  reason: "single" | "two_columns" | "cross_column_uncertain";
} {
  if (lines.length < 4) {
    return { columns: 1, orderConfidence: "high", reason: "single" };
  }
  const xs = lines.flatMap((l) => [l.x0, l.x1]);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  const mid = (minX + maxX) / 2;
  let leftOnly = 0;
  let rightOnly = 0;
  let spanning = 0;
  const gap = (maxX - minX) * 0.04;
  for (const line of lines) {
    if (line.x1 < mid - gap) leftOnly += 1;
    else if (line.x0 > mid + gap) rightOnly += 1;
    else spanning += 1;
  }
  const twoSided = leftOnly >= 3 && rightOnly >= 3;
  if (!twoSided) {
    return { columns: 1, orderConfidence: "high", reason: "single" };
  }
  // 双栏且存在跨栏行(标题/表格/页码栏):阅读顺序不确定,
  // 输出低置信与警告,不强行声称精确阅读顺序(《M04》§4)。
  if (spanning > 0) {
    return {
      columns: 2,
      orderConfidence: "low",
      reason: "cross_column_uncertain",
    };
  }
  return { columns: 2, orderConfidence: "high", reason: "two_columns" };
}

/**
 * 行 → 段落块合并:垂直间距超过行高 1.6 倍即分段。
 * 标题候选:字号显著大于正文(≥1.2 倍)或短行。
 */
export function mergeLinesIntoBlocks(lines: PdfLine[]): PdfTextBlock[] {
  if (lines.length === 0) return [];
  const blocks: PdfTextBlock[] = [];
  let current: PdfLine[] = [];
  const flush = () => {
    if (current.length === 0) return;
    const maxFont = Math.max(...current.map((l) => l.fontSize));
    blocks.push({
      text: current.map((l) => l.text).join("\n"),
      lines: current,
      fontSize: maxFont,
      rect: userSpaceBoundingRect(current),
    });
    current = [];
  };
  for (const line of lines) {
    if (current.length > 0) {
      const prev = current[current.length - 1]!;
      const gap = prev.y - line.y; // 行序自上而下,y 递减
      const expected = Math.max(prev.fontSize, line.fontSize);
      if (gap > expected * 1.6) flush();
    }
    current.push(line);
  }
  flush();
  return blocks;
}

/** 块是否为标题候选(字体大小 + 短行综合评分,《M04》§4)。 */
export function isHeadingCandidate(
  block: PdfTextBlock,
  bodyFontSize: number,
): boolean {
  const bigFont = block.fontSize >= bodyFontSize * 1.2;
  const shortLine =
    block.lines.length === 1 && block.text.length <= 40;
  const numbered = /^\s*(第?[0-9一二三四五六七八九十百]+[章节篇部.、]|[0-9]+(\.[0-9]+)+)\s*\S/.test(
    block.text,
  );
  return (bigFont && shortLine) || (numbered && shortLine);
}

/** 行集合的 user space 包围矩形(y 取基线上下各扩 0.8 行高)。 */
export function userSpaceBoundingRect(lines: PdfLine[]): PdfRect {
  const xs = lines.flatMap((l) => [l.x0, l.x1]);
  const x = Math.min(...xs);
  const width = Math.max(...xs) - x;
  const topY = Math.max(...lines.map((l) => l.y));
  const bottomY = Math.min(...lines.map((l) => l.y));
  const font = Math.max(...lines.map((l) => l.fontSize));
  const y = bottomY - font * 0.25;
  const height = topY - bottomY + font * 1.05;
  return { x, y, width, height };
}

/**
 * PDF.js viewport 仿射变换的逆变换(《M03》§3):
 * 屏幕矩形 → user space 矩形。缩放只改变 Viewport,不改数据库 Locator;
 * 禁止把缩放比例、Canvas 像素或 DPR 写入持久定位。
 */
export function invertAffineTransform(
  t: [number, number, number, number, number, number],
): [number, number, number, number, number, number] {
  const [a, b, c, d, e, f] = t;
  const det = a * d - b * c;
  if (Math.abs(det) < 1e-12) {
    throw new Error("viewport transform is not invertible");
  }
  // 逆矩阵 [a',b';c',d'] 后平移。
  const ia = d / det;
  const ib = -b / det;
  const ic = -c / det;
  const id = a / det;
  return [ia, ib, ic, id, -(ia * e + ic * f), -(ib * e + id * f)];
}

export function pointToUserSpace(
  px: number,
  py: number,
  inverse: [number, number, number, number, number, number],
): { x: number; y: number } {
  // 仿射映射:x' = a·px + c·py + e;y' = b·px + d·py + f。
  const [a, b, c, d, e, f] = inverse;
  return { x: a * px + c * py + e, y: b * px + d * py + f };
}

/** 屏幕矩形(CSS 像素)→ user space 矩形;跨角映射取包围盒。 */
export function screenRectToUserSpace(
  rect: { x: number; y: number; width: number; height: number },
  viewportTransform: [number, number, number, number, number, number],
): PdfRect {
  const inv = invertAffineTransform(viewportTransform);
  const corners = [
    pointToUserSpace(rect.x, rect.y, inv),
    pointToUserSpace(rect.x + rect.width, rect.y, inv),
    pointToUserSpace(rect.x, rect.y + rect.height, inv),
    pointToUserSpace(rect.x + rect.width, rect.y + rect.height, inv),
  ];
  const xs = corners.map((p) => p.x);
  const ys = corners.map((p) => p.y);
  const x = Math.min(...xs);
  const y = Math.min(...ys);
  return {
    x,
    y,
    width: Math.max(...xs) - x,
    height: Math.max(...ys) - y,
  };
}

export function normalizeRotation(deg: number): PageRotation {
  switch (deg) {
    case 90:
      return "90";
    case 180:
      return "180";
    case 270:
      return "270";
    default:
      return "0";
  }
}
