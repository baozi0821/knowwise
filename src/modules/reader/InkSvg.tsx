// 笔迹 SVG 渲染:自由笔与几何形状(矩形/椭圆/直线/箭头)统一渲染。
// 供 PDF 页面、富文本段落、笔记 kwink 围栏三处复用。
import type { ReactNode } from "react";
import type { InkStroke } from "./annotations";
import { smoothStrokePath } from "./inkGeometry";

export function inkStrokeElements(
  strokes: InkStroke[],
  width: number,
  height: number,
): ReactNode {
  return strokes.map((s, i) => {
    const strokeColor = s.color;
    const strokeWidth = Math.max(1.2, s.width * width);
    const first = s.points[0] ?? { x: 0, y: 0 };
    const last = s.points[s.points.length - 1] ?? first;
    const x1 = first.x * width;
    const y1 = first.y * height;
    const x2 = last.x * width;
    const y2 = last.y * height;
    const common = {
      key: i,
      fill: "none" as const,
      stroke: strokeColor,
      strokeWidth,
      strokeLinecap: "round" as const,
      strokeLinejoin: "round" as const,
    };
    switch (s.t) {
      case "rect": {
        const rx = Math.min(x1, x2);
        const ry = Math.min(y1, y2);
        return (
          <rect
            {...common}
            x={rx}
            y={ry}
            width={Math.abs(x2 - x1)}
            height={Math.abs(y2 - y1)}
          />
        );
      }
      case "ellipse":
        return (
          <ellipse
            {...common}
            cx={(x1 + x2) / 2}
            cy={(y1 + y2) / 2}
            rx={Math.abs(x2 - x1) / 2}
            ry={Math.abs(y2 - y1) / 2}
          />
        );
      case "line":
        return <line {...common} x1={x1} y1={y1} x2={x2} y2={y2} />;
      case "arrow": {
        const angle = Math.atan2(y2 - y1, x2 - x1);
        const head = Math.max(8, strokeWidth * 3.5);
        const ax1 = x2 - head * Math.cos(angle - Math.PI / 7);
        const ay1 = y2 - head * Math.sin(angle - Math.PI / 7);
        const ax2 = x2 - head * Math.cos(angle + Math.PI / 7);
        const ay2 = y2 - head * Math.sin(angle + Math.PI / 7);
        return (
          <g key={i}>
            <line {...common} x1={x1} y1={y1} x2={x2} y2={y2} />
            <polyline
              points={`${x2},${y2} ${ax1},${ay1}`}
              fill="none"
              stroke={strokeColor}
              strokeWidth={strokeWidth}
              strokeLinecap="round"
            />
            <polyline
              points={`${x2},${y2} ${ax2},${ay2}`}
              fill="none"
              stroke={strokeColor}
              strokeWidth={strokeWidth}
              strokeLinecap="round"
            />
          </g>
        );
      }
      default:
        return (
          <path
            {...common}
            d={smoothStrokePath(s.points, width, height)}
          />
        );
    }
  });
}

export function InkSvg({
  strokes,
  width,
  height,
  className,
  ariaLabel,
}: {
  strokes: InkStroke[];
  width: number;
  height: number;
  className?: string;
  ariaLabel?: string;
}): ReactNode {
  return (
    <svg
      className={className}
      width={width}
      height={height}
      viewBox={`0 0 ${Math.max(1, width)} ${Math.max(1, height)}`}
      role={ariaLabel !== undefined ? "img" : undefined}
      aria-label={ariaLabel}
    >
      {inkStrokeElements(strokes, width, height)}
    </svg>
  );
}
