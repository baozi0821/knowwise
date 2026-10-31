// 笔迹几何:平滑渲染路径、套索命中、平移与几何形状构造。
// 命中/擦除始终使用原始点(见 annotations.eraseInkCircle);平滑只作用于渲染。
import type { AnnotationColor } from "../../shared/generated/AnnotationColor";
import type { InkPoint, InkShapeType, InkStroke } from "./annotations";

/** Catmull-Rom → 三次贝塞尔;首尾点保持不变。 */
export function smoothStrokePath(
  points: readonly InkPoint[],
  width: number,
  height: number,
): string {
  if (points.length === 0) return "";
  const px = (p: InkPoint) => p.x * width;
  const py = (p: InkPoint) => p.y * height;
  const first = points[0]!;
  if (points.length === 1) return `M ${px(first)} ${py(first)}`;
  if (points.length === 2) {
    const last = points[1]!;
    return `M ${px(first)} ${py(first)} L ${px(last)} ${py(last)}`;
  }
  let d = `M ${px(first)} ${py(first)}`;
  for (let i = 0; i < points.length - 1; i += 1) {
    const p0 = points[Math.max(0, i - 1)]!;
    const p1 = points[i]!;
    const p2 = points[i + 1]!;
    const p3 = points[Math.min(points.length - 1, i + 2)]!;
    const c1x = px(p1) + (px(p2) - px(p0)) / 6;
    const c1y = py(p1) + (py(p2) - py(p0)) / 6;
    const c2x = px(p2) - (px(p3) - px(p1)) / 6;
    const c2y = py(p2) - (py(p3) - py(p1)) / 6;
    d += ` C ${c1x} ${c1y}, ${c2x} ${c2y}, ${px(p2)} ${py(p2)}`;
  }
  return d;
}

/** 几何形状采样点(用于套索命中与包围盒);自由笔返回原始点。 */
export function strokeSamplePoints(stroke: InkStroke): InkPoint[] {
  const a = stroke.points[0];
  const b = stroke.points[stroke.points.length - 1];
  if (a === undefined || b === undefined || stroke.t === undefined) return stroke.points;
  switch (stroke.t) {
    case "line":
    case "arrow": {
      const out: InkPoint[] = [];
      for (let i = 0; i <= 12; i += 1) {
        out.push({ x: a.x + (b.x - a.x) * (i / 12), y: a.y + (b.y - a.y) * (i / 12) });
      }
      return out;
    }
    case "rect": {
      const minX = Math.min(a.x, b.x);
      const maxX = Math.max(a.x, b.x);
      const minY = Math.min(a.y, b.y);
      const maxY = Math.max(a.y, b.y);
      const out: InkPoint[] = [];
      for (let i = 0; i <= 4; i += 1) {
        out.push({ x: minX + ((maxX - minX) * i) / 4, y: minY });
        out.push({ x: minX + ((maxX - minX) * i) / 4, y: maxY });
      }
      for (let i = 0; i <= 4; i += 1) {
        out.push({ x: minX, y: minY + ((maxY - minY) * i) / 4 });
        out.push({ x: maxX, y: minY + ((maxY - minY) * i) / 4 });
      }
      return out;
    }
    case "ellipse": {
      const cx = (a.x + b.x) / 2;
      const cy = (a.y + b.y) / 2;
      const rx = Math.abs(b.x - a.x) / 2;
      const ry = Math.abs(b.y - a.y) / 2;
      const out: InkPoint[] = [];
      for (let i = 0; i < 32; i += 1) {
        const angle = (i / 32) * Math.PI * 2;
        out.push({ x: cx + Math.cos(angle) * rx, y: cy + Math.sin(angle) * ry });
      }
      return out;
    }
  }
}

/** 射线法:点是否在多边形内(边界视为内)。 */
export function pointInPolygon(point: InkPoint, polygon: readonly InkPoint[]): boolean {
  if (polygon.length < 3) return false;
  let inside = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
    const a = polygon[i]!;
    const b = polygon[j]!;
    const intersect =
      a.y > point.y !== b.y > point.y &&
      point.x < ((b.x - a.x) * (point.y - a.y)) / (b.y - a.y || Number.EPSILON) + a.x;
    if (intersect) inside = !inside;
  }
  return inside;
}

/** 笔迹是否被套索命中:任一采样点落在多边形内。 */
export function strokeInPolygon(
  stroke: InkStroke,
  polygon: readonly InkPoint[],
): boolean {
  return strokeSamplePoints(stroke).some((p) => pointInPolygon(p, polygon));
}

/** 平移笔迹(归一化坐标),几何形状同样平移包围盒。 */
export function translateStrokes(
  strokes: readonly InkStroke[],
  dx: number,
  dy: number,
): InkStroke[] {
  return strokes.map((stroke) => ({
    ...stroke,
    points: stroke.points.map((p) => ({
      x: Math.min(1.2, Math.max(-0.2, p.x + dx)),
      y: Math.min(1.2, Math.max(-0.2, p.y + dy)),
    })),
  }));
}

/** 构造几何形状:首末两点定义包围盒/方向。 */
export function makeShapeStroke(
  type: InkShapeType,
  start: InkPoint,
  end: InkPoint,
  color: string,
  width: number,
): InkStroke {
  return { color, width, t: type, points: [start, end] };
}

/** 笔迹包围盒(归一化);空集合返回 null。 */
export function inkBoundsOf(
  strokes: readonly InkStroke[],
): { minX: number; minY: number; maxX: number; maxY: number } | null {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const stroke of strokes) {
    for (const p of stroke.points) {
      minX = Math.min(minX, p.x);
      minY = Math.min(minY, p.y);
      maxX = Math.max(maxX, p.x);
      maxY = Math.max(maxY, p.y);
    }
  }
  if (!Number.isFinite(minX)) return null;
  return { minX, minY, maxX, maxY };
}

/** 选择框(像素)四周留出线宽余量,避免细线被裁掉。 */
export function selectionBoxPx(
  strokes: readonly InkStroke[],
  width: number,
  height: number,
): { left: number; top: number; width: number; height: number } | null {
  const b = inkBoundsOf(strokes);
  if (b === null) return null;
  const pad = 8;
  return {
    left: b.minX * width - pad,
    top: b.minY * height - pad,
    width: (b.maxX - b.minX) * width + pad * 2,
    height: (b.maxY - b.minY) * height + pad * 2,
  };
}

export const SHAPE_TYPES: readonly InkShapeType[] = ["rect", "ellipse", "line", "arrow"];

export const SHAPE_LABELS: Record<InkShapeType, string> = {
  rect: "矩形",
  ellipse: "椭圆",
  line: "直线",
  arrow: "箭头",
};

export type InkColorName = AnnotationColor;
