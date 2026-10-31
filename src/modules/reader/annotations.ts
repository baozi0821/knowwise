// 阅读器标注共享层:颜色调色板、PDF user-space 矩形换算、涂鸦笔迹编解码,
// 以及 Markdown/DOCX 渲染文本偏移 ↔ DOM Range 的互转。
// 纯函数为主,视图层负责持久化调用与失效。
import type { AnnotationColor } from "../../shared/generated/AnnotationColor";
import type { PdfRect } from "../../shared/generated/PdfRect";

// —— 颜色 ——

export type PaletteEntry = {
  color: AnnotationColor;
  /** 主体色(色点/边框)。 */
  hex: string;
  /** 高亮填充(叠加在纸面上,multiply 混合)。 */
  fill: string;
  label: string;
};

export const ANNOTATION_PALETTE: readonly PaletteEntry[] = [
  { color: "yellow", hex: "#f2b705", fill: "rgba(250, 204, 21, 0.42)", label: "黄" },
  { color: "green", hex: "#31c48d", fill: "rgba(52, 211, 153, 0.40)", label: "绿" },
  { color: "blue", hex: "#3f83f8", fill: "rgba(96, 165, 250, 0.40)", label: "蓝" },
  { color: "pink", hex: "#f04f9e", fill: "rgba(244, 114, 182, 0.40)", label: "粉" },
  { color: "purple", hex: "#9061f9", fill: "rgba(167, 139, 250, 0.40)", label: "紫" },
];

export function paletteOf(color: AnnotationColor): PaletteEntry {
  return ANNOTATION_PALETTE.find((p) => p.color === color) ?? ANNOTATION_PALETTE[0]!;
}

/** 标注样式标记(noteText 前缀;与用户备注共存:样式在前、备注在后)。 */
export const STYLE_UNDERLINE_PREFIX = "style:underline:";

/** 涂鸦笔迹序列化前缀(noteText)。 */
export const INK_PREFIX = "ink:v1:";

export function encodeStyleNote(style: "highlight" | "underline", note: string): string {
  const body = note.trim();
  return style === "underline" ? `${STYLE_UNDERLINE_PREFIX}${body}` : body;
}

export function decodeStyleNote(noteText: string): {
  style: "highlight" | "underline";
  note: string;
} {
  if (noteText.startsWith(STYLE_UNDERLINE_PREFIX)) {
    return { style: "underline", note: noteText.slice(STYLE_UNDERLINE_PREFIX.length) };
  }
  return { style: "highlight", note: noteText };
}

// —— 涂鸦笔迹(页面归一化坐标 0..1,缩放无关)——

export type InkPoint = { x: number; y: number };
export type InkShapeType = "rect" | "ellipse" | "line" | "arrow";

export type InkStroke = {
  color: string;
  /** 相对页面宽度的线宽(0..1),随缩放等比变化。 */
  width: number;
  points: InkPoint[];
  /** 缺省自由笔;几何形状用首末两点定义包围盒。 */
  t?: InkShapeType;
};

/** 简单抽稀:丢弃与上一点距离不足 minDist 的点(保首尾)。 */
export function simplifyStroke(stroke: InkStroke, minDist: number): InkStroke {
  if (stroke.points.length <= 2) return stroke;
  const out: InkPoint[] = [stroke.points[0]!];
  for (let i = 1; i < stroke.points.length - 1; i += 1) {
    const p = stroke.points[i]!;
    const last = out[out.length - 1]!;
    if (Math.hypot(p.x - last.x, p.y - last.y) >= minDist) out.push(p);
  }
  out.push(stroke.points[stroke.points.length - 1]!);
  return { ...stroke, points: out };
}

const r4 = (v: number): number => Math.round(v * 10000) / 10000;

/**
 * 紧凑序列化:点为 [x,y] 数组、键名缩短(c/w/p)、坐标 4 位小数,并按
 * 阈值递增抽稀直到满足字符预算(后端 noteText 上限 2000,留余量)。
 * 旧版超限即保存失败的根因即在此。
 */
export function encodeInk(strokes: InkStroke[]): string {
  const BUDGET = 1900;
  const thresholds = [0, 0.004, 0.008, 0.015, 0.03, 0.05];
  for (const threshold of thresholds) {
    const compact = strokes.map((s) => {
      if (s.t !== undefined) {
        const a = s.points[0]!;
        const b = s.points[s.points.length - 1]!;
        return { c: s.color, w: r4(s.width), t: s.t, x1: r4(a.x), y1: r4(a.y), x2: r4(b.x), y2: r4(b.y) };
      }
      return {
        c: s.color,
        w: r4(s.width),
        p: (threshold === 0 ? s.points : simplifyStroke(s, threshold).points).map(
          (p) => [r4(p.x), r4(p.y)] as [number, number],
        ),
      };
    });
    const encoded = `${INK_PREFIX}${JSON.stringify(compact)}`;
    if (encoded.length <= BUDGET) return encoded;
  }
  // 极端长笔画:抽稀到最粗后仍超限,截取首尾保形态主干(宁存不丢)。
  const minimal = strokes.map((s) => {
    if (s.t !== undefined) {
      const a = s.points[0]!;
      const b = s.points[s.points.length - 1]!;
      return { c: s.color, w: r4(s.width), t: s.t, x1: r4(a.x), y1: r4(a.y), x2: r4(b.x), y2: r4(b.y) };
    }
    const pts = simplifyStroke(s, 0.05).points;
    const kept = pts.length > 40 ? [...pts.slice(0, 20), ...pts.slice(-20)] : pts;
    return {
      c: s.color,
      w: r4(s.width),
      p: kept.map((p) => [r4(p.x), r4(p.y)] as [number, number]),
    };
  });
  return `${INK_PREFIX}${JSON.stringify(minimal)}`;
}

/** 兼容两种点形态:旧 {x,y} 对象与新 [x,y] 数组。 */
function parsePoint(raw: unknown): InkPoint | null {
  if (Array.isArray(raw) && raw.length >= 2) {
    const [x, y] = raw as [unknown, unknown];
    if (typeof x === "number" && typeof y === "number") return { x, y };
  }
  if (typeof raw === "object" && raw !== null) {
    const p = raw as { x?: unknown; y?: unknown };
    if (typeof p.x === "number" && typeof p.y === "number") return { x: p.x, y: p.y };
  }
  return null;
}

export function decodeInk(noteText: string): InkStroke[] | null {
  if (!noteText.startsWith(INK_PREFIX)) return null;
  try {
    const parsed = JSON.parse(noteText.slice(INK_PREFIX.length));
    if (!Array.isArray(parsed)) return null;
    const strokes: InkStroke[] = [];
    for (const s of parsed) {
      const rawPoints = Array.isArray(s?.p) ? s.p : s?.points;
      const color = typeof s?.c === "string" ? s.c : s?.color;
      if (typeof color !== "string") continue;
      const width = typeof s?.w === "number" ? s.w : (s?.width ?? 0.0035);
      const shape = s?.t;
      if (
        (shape === "rect" || shape === "ellipse" || shape === "line" || shape === "arrow") &&
        typeof s?.x1 === "number" &&
        typeof s?.y1 === "number" &&
        typeof s?.x2 === "number" &&
        typeof s?.y2 === "number"
      ) {
        strokes.push({
          color,
          width,
          t: shape,
          points: [
            { x: s.x1, y: s.y1 },
            { x: s.x2, y: s.y2 },
          ],
        });
        continue;
      }
      if (!Array.isArray(rawPoints)) continue;
      const points = rawPoints
        .map(parsePoint)
        .filter((p: InkPoint | null): p is InkPoint => p !== null);
      if (points.length === 0) continue;
      strokes.push({ color, width, points });
    }
    return strokes.length > 0 ? strokes : null;
  } catch {
    return null;
  }
}

/** 笔记/标注的分类(笔记面板按类型分组)。 */
export type AnnotationKind = "ink" | "note" | "highlight" | "underline";

export function classifyAnnotation(noteText: string): AnnotationKind {
  if (noteText.startsWith(INK_PREFIX)) return "ink";
  const { style, note } = decodeStyleNote(noteText);
  if (note.trim().length > 0) return "note";
  return style === "underline" ? "underline" : "highlight";
}

// —— PDF 矩形换算(client rect → 未缩放 user space)——

/**
 * 选区/元素矩形 → PDF user space 矩形:client 坐标先减页面容器原点,
 * 再除以渲染缩放。旋转页(90/270)此处按 r0 近似——阅读器本身不旋转页面。
 */
export function clientRectsToPdfRects(
  rects: ArrayLike<DOMRect> | DOMRect[],
  pageEl: HTMLElement,
  scale: number,
): PdfRect[] {
  const base = pageEl.getBoundingClientRect();
  const safeScale = scale > 0 ? scale : 1;
  const out: PdfRect[] = [];
  for (const r of Array.from(rects)) {
    if (r.width <= 0 || r.height <= 0) continue;
    out.push({
      x: (r.left - base.left) / safeScale,
      y: (r.top - base.top) / safeScale,
      width: r.width / safeScale,
      height: r.height / safeScale,
    });
  }
  return out;
}

/** 笔迹包围盒(归一化)→ user space 矩形,供锚点 locator 使用。 */
export function inkBoundsToPdfRect(
  strokes: InkStroke[],
  pageWidth: number,
  pageHeight: number,
): PdfRect {
  let minX = 1;
  let minY = 1;
  let maxX = 0;
  let maxY = 0;
  for (const s of strokes) {
    for (const p of s.points) {
      minX = Math.min(minX, p.x);
      minY = Math.min(minY, p.y);
      maxX = Math.max(maxX, p.x);
      maxY = Math.max(maxY, p.y);
    }
  }
  const pad = 0.004;
  return {
    x: Math.max(0, minX - pad) * pageWidth,
    y: Math.max(0, minY - pad) * pageHeight,
    width: Math.max(0.01, maxX - minX + pad * 2) * pageWidth,
    height: Math.max(0.01, maxY - minY + pad * 2) * pageHeight,
  };
}

// —— 渲染文本偏移(Markdown/DOCX):UTF-16 code unit,与 DOM Range 互转 ——

/** 节点在 root.textContent 中的字符偏移(root 自身为 0)。 */
export function domOffsetOf(root: HTMLElement, node: Node, offset: number): number {
  let count = 0;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let current = walker.nextNode();
  while (current !== null) {
    if (current === node) return count + offset;
    count += current.textContent?.length ?? 0;
    current = walker.nextNode();
  }
  return -1;
}

/** 由字符偏移构造 Range;越界或找不到时返回 null。 */
export function rangeFromOffsets(
  root: HTMLElement,
  start: number,
  end: number,
): Range | null {
  if (start < 0 || end <= start) return null;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let count = 0;
  let startNode: Text | null = null;
  let startOffset = 0;
  let endNode: Text | null = null;
  let endOffset = 0;
  let node = walker.nextNode();
  while (node !== null) {
    const len = node.textContent?.length ?? 0;
    // 起点必须落在节点内部(严格大于):偏移正好等于上一个节点末尾时,
    // 应取下一个节点。否则会落到上一段的换行文本节点上,closest() 找不到
    // 真正段落/列表项,笔迹与标注就永远不渲染。
    if (startNode === null && count + len > start) {
      startNode = node as Text;
      startOffset = start - count;
    }
    if (count + len >= end) {
      endNode = node as Text;
      endOffset = end - count;
      break;
    }
    count += len;
    node = walker.nextNode();
  }
  if (startNode === null || endNode === null) return null;
  try {
    const range = document.createRange();
    range.setStart(startNode, startOffset);
    range.setEnd(endNode, endOffset);
    return range;
  } catch {
    return null;
  }
}

/** 选区最近的标题路径(h1..h6),供锚点 locator 的 headingPath。 */
export function headingPathOf(root: HTMLElement, node: Node | null): string[] {
  let el = node instanceof Element ? node : node?.parentElement ?? null;
  while (el !== null && el !== root.parentElement) {
    const path: string[] = [];
    // 向上收集祖先标题(最外层在前)。
    let scope: Element | null = el;
    while (scope !== null && root.contains(scope)) {
      const tag = scope.tagName;
      if (tag === "H1" || tag === "H2" || tag === "H3" || tag === "H4" || tag === "H5" || tag === "H6") {
        path.unshift((scope.textContent ?? "").trim().slice(0, 80));
      }
      scope = scope.parentElement;
    }
    if (path.length > 0) return path;
    el = el.parentElement;
  }
  return [];
}

// —— Markdown/DOCX 涂鸦:段落锚定 ——

/** 可作为笔迹锚定宿主的块级元素。 */
export const INK_BLOCK_SELECTOR =
  "p, h1, h2, h3, h4, h5, h6, li, blockquote, pre, td, th, img, figure";

/**
 * 命中点所在块级元素(限制在 root 内);找不到返回 null。
 * 叠加画布覆盖正文且接收指针事件,命中测试时临时穿透自身,
 * 否则 elementFromPoint 永远命中画布(Word/MD 涂鸦失效的根因)。
 */
export function inkBlockFromPoint(
  root: HTMLElement,
  x: number,
  y: number,
  overlay?: HTMLElement | null,
): HTMLElement | null {
  const restore = overlay?.style.pointerEvents ?? null;
  if (overlay != null) overlay.style.pointerEvents = "none";
  try {
    const el = document.elementFromPoint(x, y);
    if (!(el instanceof Element) || !root.contains(el)) return null;
    const block = el.closest(INK_BLOCK_SELECTOR);
    return block instanceof HTMLElement && root.contains(block) ? block : null;
  } finally {
    if (overlay != null) overlay.style.pointerEvents = restore ?? "";
  }
}

/** 块级元素在 root 文本中的字符区间(锚点 locator 偏移用)。 */export function inkBlockRange(
  root: HTMLElement,
  block: HTMLElement,
): { start: number; end: number } | null {
  const ranges = blockTextRanges(root, [block]);
  return ranges.get(block) ?? null;
}

/**
 * 圆形橡皮的局部擦除:只删圆内的点,并按连续段把自由笔拆成多条;
 * 几何形状(矩形/椭圆/直线/箭头)无法拆分,只能整条删除。
 * 返回 null 表示没有命中(调用方据此跳过写库)。
 */
export function eraseInkCircle(
  strokes: InkStroke[],
  hit: (p: InkPoint) => boolean,
): InkStroke[] | null {
  let changed = false;
  const out: InkStroke[] = [];
  for (const stroke of strokes) {
    if (stroke.t !== undefined) {
      if (stroke.points.some(hit)) {
        changed = true;
        continue;
      }
      out.push(stroke);
      continue;
    }
    let strokeChanged = false;
    let run: InkPoint[] = [];
    const runs: InkPoint[][] = [];
    for (const p of stroke.points) {
      if (hit(p)) {
        strokeChanged = true;
        if (run.length >= 2) runs.push(run);
        run = [];
      } else {
        run.push(p);
      }
    }
    if (run.length >= 2) runs.push(run);
    if (!strokeChanged) {
      out.push(stroke);
      continue;
    }
    changed = true;
    for (const points of runs) out.push({ ...stroke, points });
  }
  return changed ? out : null;
}

/**
 * 落笔点不在任何块内(段落间隙、列表空档)时的锚定块:取纵向最近的
 * 有文本块。旧实现退回 root.firstElementChild,在 DOCX 里常是包住全文
 * 的容器且算不出区间 → locator 0–0,笔迹保存后在重开时永远不渲染。
 */
export function nearestInkBlock(
  root: HTMLElement,
  x: number,
  y: number,
  overlay?: HTMLElement | null,
): HTMLElement | null {
  const direct = inkBlockFromPoint(root, x, y, overlay);
  if (direct !== null) return direct;
  const blocks = Array.from(root.querySelectorAll<HTMLElement>(INK_BLOCK_SELECTOR)).filter(
    (el) => root.contains(el) && (el.textContent ?? "").trim().length > 0,
  );
  let best: HTMLElement | null = null;
  let bestScore = Infinity;
  for (const el of blocks) {
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) continue;
    const dy = y < r.top ? r.top - y : y > r.bottom ? y - r.bottom : 0;
    const dx = x < r.left ? r.left - x : x > r.right ? x - r.right : 0;
    // 垂直距离优先:间隙落笔通常应归到上方/下方最近的段落。
    const score = dy * 4 + dx;
    if (score < bestScore) {
      bestScore = score;
      best = el;
    }
  }
  return best;
}

/**
 * 一次遍历文本节点,同时算出多个块的字符区间。
 *
 * 旧实现每块都调用两次 `domOffsetOf`(各自从 root 走一遍 TreeWalker):
 * 长文档滚动位置采集时是 O(块×文本节点),实测 721 块约 160ms 主线程。
 * 块内文本在文档序上连续,因此单遍累积偏移即可;块离开后再定稿。
 */
export function blockTextRanges(
  root: HTMLElement,
  blocks: readonly HTMLElement[],
): Map<HTMLElement, { start: number; end: number }> {
  const out = new Map<HTMLElement, { start: number; end: number }>();
  if (blocks.length === 0) return out;
  const pending = new Map<HTMLElement, { start: number; end: number }>();
  for (const block of blocks) {
    if (root.contains(block)) pending.set(block, { start: -1, end: -1 });
  }
  if (pending.size === 0) return out;
  const active = new Set<HTMLElement>();
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let offset = 0;
  let node = walker.nextNode();
  while (node !== null && pending.size > 0) {
    const text = node.textContent ?? "";
    active.clear();
    let el: HTMLElement | null = node.parentElement;
    while (el !== null) {
      if (pending.has(el)) {
        active.add(el);
        const entry = pending.get(el)!;
        if (text.length > 0) {
          if (entry.start < 0) entry.start = offset;
          entry.end = offset + text.length;
        }
      }
      if (el === root) break;
      el = el.parentElement;
    }
    // 本节点不再覆盖的块:其文本已结束,定稿。
    // 注意:尚未遇到第一个文本节点的块(entry.start < 0)必须保留——
    // 旧实现在这里一并删除,导致只有文档首个块能算出区间,后续段落的
    // 笔迹/标注锚点全部落成 0–0,重开后不再渲染。
    for (const [block, entry] of [...pending]) {
      if (active.has(block)) continue;
      if (entry.start < 0) continue;
      if (entry.end > entry.start) {
        out.set(block, { start: entry.start, end: entry.end });
      }
      pending.delete(block);
    }
    offset += text.length;
    node = walker.nextNode();
  }
  // 走到了文档末尾(块的后续文本不存在)时也定稿。
  for (const [block, entry] of pending) {
    if (entry.start >= 0 && entry.end > entry.start) {
      out.set(block, { start: entry.start, end: entry.end });
    }
  }
  return out;
}

// —— 文字注释框(text box)——
//
// 复用标注存储(noteText 前缀区分),因此自然获得:版本绑定、定位回跳、
// 隐私清除、导出与删除。样式(文字色/加粗/字号)随框保存。

export const TEXTBOX_PREFIX = "textbox:v1:";

export type TextBoxStyle = {
  /** 文字颜色(#rrggbb)。 */
  color: string;
  bold: boolean;
  /** 字号(px,12–28)。 */
  fontSize: number;
};

export type TextBoxPayload = {
  /** 页面/块归一化坐标(0..1),与缩放无关。 */
  x: number;
  y: number;
  width: number;
  text: string;
  style: TextBoxStyle;
};

export const DEFAULT_TEXTBOX_STYLE: TextBoxStyle = {
  color: "#f2b705",
  bold: false,
  fontSize: 14,
};

export function encodeTextBox(payload: TextBoxPayload): string {
  const compact = {
    x: Math.round(payload.x * 1e4) / 1e4,
    y: Math.round(payload.y * 1e4) / 1e4,
    w: Math.round(payload.width * 1e4) / 1e4,
    t: payload.text.slice(0, 1200),
    c: payload.style.color,
    b: payload.style.bold ? 1 : 0,
    s: Math.round(payload.style.fontSize),
  };
  return `${TEXTBOX_PREFIX}${JSON.stringify(compact)}`;
}

export function decodeTextBox(noteText: string): TextBoxPayload | null {
  if (!noteText.startsWith(TEXTBOX_PREFIX)) return null;
  try {
    const raw = JSON.parse(noteText.slice(TEXTBOX_PREFIX.length)) as {
      x?: number; y?: number; w?: number; t?: string; c?: string; b?: number; s?: number;
    };
    if (typeof raw.x !== "number" || typeof raw.y !== "number") return null;
    return {
      x: raw.x,
      y: raw.y,
      width: typeof raw.w === "number" ? raw.w : 0.28,
      text: typeof raw.t === "string" ? raw.t : "",
      style: {
        color: typeof raw.c === "string" ? raw.c : DEFAULT_TEXTBOX_STYLE.color,
        bold: raw.b === 1,
        fontSize:
          typeof raw.s === "number"
            ? Math.min(28, Math.max(12, raw.s))
            : DEFAULT_TEXTBOX_STYLE.fontSize,
      },
    };
  } catch {
    return null;
  }
}
