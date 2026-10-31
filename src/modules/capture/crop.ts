// 屏幕框选截图:裁剪参数 / 缩放换算纯函数(capture 拥有)。
//
// 坐标系约定:
// - 「视口坐标」= WebView 窗口 CSS 像素(clientX/clientY、window.innerWidth/innerHeight);
// - 「截图坐标」= `capture_screen` 返回的虚拟屏幕物理像素(PNG 原始宽度/高度)。
//
// 框选层把截图底图按「100% 100%」铺满视口,因此两套坐标只差一个比例:
// captureScale()(单显示器且窗口等于屏幕时约等于 window.devicePixelRatio)。
// 本文件不触碰 DOM/Canvas,便于 vitest 直接单测;真正的绘制在 ScreenClipOverlay。

export interface Point {
  x: number;
  y: number;
}

export interface Size {
  width: number;
  height: number;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** 视口 → 截图的 x/y 方向比例(不小于 0)。 */
export interface Scale {
  x: number;
  y: number;
}

/** 小于该边长(视口 CSS 像素)的选区视为误触。 */
export const MIN_SELECTION_PX = 8;

/** 压缩输出宽度上限(物理像素)。 */
export const MAX_OUTPUT_WIDTH = 1600;

/** 初始 JPEG 质量。 */
export const INITIAL_JPEG_QUALITY = 0.8;

/** 单张 data-URI 的字节预算(约 64KB,base64 后约 6.4 万字符)。
 *  与「笔记正文 100000 字符」上限对齐:截图要能直接嵌进笔记正文,
 *  过大的 data-URI 会让 note_save 因超长失败。 */
export const MAX_DATA_URL_BYTES = 64 * 1024;

/** 缩到该宽度仍超预算时接受最后结果(避免无限循环)。 */
export const MIN_OUTPUT_WIDTH = 240;

function clamp(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(Math.max(value, min), max);
}

/** 由拖拽起止点构造非负矩形(支持任意拖动方向)。 */
export function normalizeRect(start: Point, end: Point): Rect {
  const x = Math.min(start.x, end.x);
  const y = Math.min(start.y, end.y);
  return {
    x,
    y,
    width: Math.abs(end.x - start.x),
    height: Math.abs(end.y - start.y),
  };
}

/** 把矩形裁剪到 [0, 0, bounds] 内;完全越界时得到零尺寸矩形。 */
export function clampRectToBounds(rect: Rect, bounds: Size): Rect {
  const left = clamp(rect.x, 0, bounds.width);
  const top = clamp(rect.y, 0, bounds.height);
  const right = clamp(rect.x + Math.max(0, rect.width), 0, bounds.width);
  const bottom = clamp(rect.y + Math.max(0, rect.height), 0, bounds.height);
  return {
    x: left,
    y: top,
    width: Math.max(0, right - left),
    height: Math.max(0, bottom - top),
  };
}

/** 选区是否过小(任一方向低于阈值即无效)。 */
export function isSelectionTooSmall(rect: Rect, minSide = MIN_SELECTION_PX): boolean {
  return rect.width < minSide || rect.height < minSide;
}

/** 视口 CSS 像素 ↔ 截图物理像素的比例;尺寸非法时退回 1(不缩放)。 */
export function captureScale(capture: Size, viewport: Size): Scale {
  return {
    x: capture.width > 0 && viewport.width > 0 ? capture.width / viewport.width : 1,
    y: capture.height > 0 && viewport.height > 0 ? capture.height / viewport.height : 1,
  };
}

/** 视口点 → 截图点(不取整)。 */
export function viewportToImagePoint(point: Point, scale: Scale): Point {
  return { x: point.x * scale.x, y: point.y * scale.y };
}

/** 视口选区 → 截图物理像素选区:取整并裁剪到截图边界。 */
export function viewportRectToImageRect(
  rect: Rect,
  scale: Scale,
  capture: Size,
): Rect {
  const raw: Rect = {
    x: Math.round(rect.x * scale.x),
    y: Math.round(rect.y * scale.y),
    width: Math.round(rect.width * scale.x),
    height: Math.round(rect.height * scale.y),
  };
  const bounded = clampRectToBounds(raw, capture);
  return {
    x: Math.round(bounded.x),
    y: Math.round(bounded.y),
    width: Math.round(bounded.width),
    height: Math.round(bounded.height),
  };
}

/** 按最大宽度等比缩小(不放大)。 */
export function fitOutputSize(size: Size, maxWidth = MAX_OUTPUT_WIDTH): Size {
  const width = Math.max(1, Math.round(size.width));
  const height = Math.max(1, Math.round(size.height));
  if (maxWidth <= 0 || width <= maxWidth) return { width, height };
  const scaledHeight = Math.max(1, Math.round((height * maxWidth) / width));
  return { width: maxWidth, height: scaledHeight };
}

/** 输出宽度按比例收缩(质量已到下限时继续降预算)。 */
export function shrinkOutputSize(size: Size, factor = 0.75): Size {
  return {
    width: Math.max(1, Math.round(size.width * factor)),
    height: Math.max(1, Math.round(size.height * factor)),
  };
}

/** 依次尝试的 JPEG 质量阶梯(从初始质量严格递减到 0.4)。 */
export function jpegQualityLadder(initial = INITIAL_JPEG_QUALITY): number[] {
  const ladder: number[] = [];
  for (const q of [initial, 0.72, 0.64, 0.56, 0.48, 0.4]) {
    const rounded = Math.round(q * 100) / 100;
    if (rounded <= 0 || rounded > 1) continue;
    if (ladder.length > 0 && rounded >= ladder[ladder.length - 1]!) continue;
    ladder.push(rounded);
  }
  return ladder;
}

/** data-URI 的近似字节数:去掉 `data:<mime>;base64,` 头后按 base64 3/4 估算。 */
export function dataUrlByteSize(dataUrl: string): number {
  if (!dataUrl || !dataUrl.startsWith("data:")) return 0;
  const comma = dataUrl.indexOf(",");
  if (comma < 0) return 0;
  const payload = dataUrl.slice(comma + 1);
  let padding = 0;
  if (payload.endsWith("==")) padding = 2;
  else if (payload.endsWith("=")) padding = 1;
  const bytes = Math.floor((payload.length * 3) / 4) - padding;
  return Math.max(0, bytes);
}

/** 压缩结果是否满足大小预算。 */
export function isWithinBudget(dataUrl: string, budget = MAX_DATA_URL_BYTES): boolean {
  return dataUrlByteSize(dataUrl) <= budget;
}

/** 一次裁剪所需的全部参数:源矩形(截图物理像素)+ 输出尺寸 + 初始质量。 */
export interface CropPlan {
  source: Rect;
  output: Size;
  quality: number;
}

export interface PlanCropOptions {
  minSide?: number;
  maxWidth?: number;
  quality?: number;
}

/**
 * 视口选区 → 裁剪计划。选区过小/越界/截图尺寸非法时返回 null(调用方按取消处理)。
 */
export function planCrop(
  capture: Size,
  viewport: Size,
  selection: Rect,
  options: PlanCropOptions = {},
): CropPlan | null {
  if (capture.width <= 0 || capture.height <= 0) return null;
  if (viewport.width <= 0 || viewport.height <= 0) return null;
  if (isSelectionTooSmall(selection, options.minSide ?? MIN_SELECTION_PX)) return null;
  const scale = captureScale(capture, viewport);
  const source = viewportRectToImageRect(selection, scale, capture);
  if (source.width < 1 || source.height < 1) return null;
  return {
    source,
    output: fitOutputSize(source, options.maxWidth ?? MAX_OUTPUT_WIDTH),
    quality: options.quality ?? INITIAL_JPEG_QUALITY,
  };
}
