// 屏幕框选层(capture 拥有):整屏截图底图 + 深色遮罩 + 十字光标 +
// 拖拽框选 + 尺寸读数 + 「取消 / 确定」工具条。
//
// 交互契约:
// - Esc 取消(返回 null);Enter 确认(返回压缩后 data-URI);
// - 选区边长 < 8px 视为误触,确认时按取消处理;
// - 确认后用 canvas 裁剪选区,JPEG 质量约 0.8、宽度上限 1600、
//   单张 data-URI 预算约 64KB(超预算依次降质量、再缩宽度)——
//   预算与笔记正文 10 万字符上限对齐,截图可直接嵌入笔记正文。
//
// 坐标换算见 crop.ts:底图按视口宽高 100% 铺满,视口 CSS 像素与截图
// 物理像素之间是 captureScale() 比例(单显示器约等于 devicePixelRatio)。

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  MIN_OUTPUT_WIDTH,
  clampRectToBounds,
  fitOutputSize,
  isSelectionTooSmall,
  isWithinBudget,
  jpegQualityLadder,
  normalizeRect,
  planCrop,
  shrinkOutputSize,
  type CropPlan,
  type Point,
  type Rect,
  type Size,
} from "./crop";
import "./capture.css";

export interface ScreenClipCapture {
  dataUrl: string;
  width: number;
  height: number;
}

export interface ScreenClipOverlayProps {
  capture: ScreenClipCapture;
  onCancel: () => void;
  onConfirm: (dataUrl: string) => void;
}

const TOOLBAR_WIDTH = 188;
const TOOLBAR_HEIGHT = 46;
const EDGE_GAP = 8;

function currentViewport(): Size {
  if (typeof window === "undefined") return { width: 1, height: 1 };
  return {
    width: Math.max(1, window.innerWidth),
    height: Math.max(1, window.innerHeight),
  };
}

/** 指针事件 → 视口坐标(模块级纯函数,避免 useCallback 依赖抖动)。 */
function pointFromEvent(event: { clientX: number; clientY: number }): Point {
  return { x: event.clientX, y: event.clientY };
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error("截图底图加载失败"));
    image.src = src;
  });
}

/** 把视口选区绘制成 JPEG data-URI(纯 DOM 操作,失败抛错由调用方兜底)。 */
function drawJpeg(
  image: HTMLImageElement,
  source: Rect,
  output: Size,
  quality: number,
): string {
  const canvas = document.createElement("canvas");
  canvas.width = output.width;
  canvas.height = output.height;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("canvas 2d 不可用");
  context.imageSmoothingEnabled = true;
  context.imageSmoothingQuality = "high";
  context.drawImage(
    image,
    source.x,
    source.y,
    Math.max(1, source.width),
    Math.max(1, source.height),
    0,
    0,
    output.width,
    output.height,
  );
  return canvas.toDataURL("image/jpeg", quality);
}

/** 按预算压缩:先降质量,仍超预算再逐步缩宽;返回最小可行结果。 */
async function encodeCrop(
  capture: ScreenClipCapture,
  plan: CropPlan,
): Promise<string | null> {
  const image = await loadImage(capture.dataUrl);
  const naturalWidth = image.naturalWidth || capture.width;
  const naturalHeight = image.naturalHeight || capture.height;
  // 声明尺寸与图片实际尺寸不一致时按比例再换算一次,避免裁到图外。
  const ratioX = naturalWidth / Math.max(1, capture.width);
  const ratioY = naturalHeight / Math.max(1, capture.height);
  const naturalRect: Rect = {
    x: Math.round(plan.source.x * ratioX),
    y: Math.round(plan.source.y * ratioY),
    width: Math.round(plan.source.width * ratioX),
    height: Math.round(plan.source.height * ratioY),
  };
  const source = clampRectToBounds(naturalRect, {
    width: naturalWidth,
    height: naturalHeight,
  });
  if (source.width < 1 || source.height < 1) return null;

  // 输出尺寸沿用计划值(planCrop 已按 MAX_OUTPUT_WIDTH 等比限制)。
  let output = fitOutputSize(plan.output);
  let last: string | null = null;
  for (const quality of jpegQualityLadder(plan.quality)) {
    last = drawJpeg(image, source, output, quality);
    if (isWithinBudget(last)) return last;
  }
  while (output.width > MIN_OUTPUT_WIDTH) {
    output = shrinkOutputSize(output);
    for (const quality of [0.72, 0.6, 0.5]) {
      last = drawJpeg(image, source, output, quality);
      if (isWithinBudget(last)) return last;
    }
  }
  return last;
}

export function ScreenClipOverlay({ capture, onCancel, onConfirm }: ScreenClipOverlayProps) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const dragStartRef = useRef<Point | null>(null);
  const busyRef = useRef(false);
  const [viewport, setViewport] = useState<Size>(currentViewport);
  const [selection, setSelection] = useState<Rect | null>(null);
  const [cursor, setCursor] = useState<Point | null>(null);
  const [dragging, setDragging] = useState(false);
  const [busy, setBusy] = useState(false);

  // 视口尺寸变化(窗口缩放/最大化切换)时重新换算。
  useEffect(() => {
    const onResize = () => setViewport(currentViewport());
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    rootRef.current?.focus({ preventScroll: true });
  }, []);

  const confirm = useCallback(async () => {
    if (busyRef.current) return;
    if (!selection || isSelectionTooSmall(selection)) {
      onCancel();
      return;
    }
    const plan = planCrop(capture, viewport, selection);
    if (!plan) {
      onCancel();
      return;
    }
    busyRef.current = true;
    setBusy(true);
    try {
      const dataUrl = await encodeCrop(capture, plan);
      if (dataUrl) onConfirm(dataUrl);
      else onCancel();
    } catch {
      onCancel();
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }, [capture, onCancel, onConfirm, selection, viewport]);

  // Esc 取消 / Enter 确认(捕获阶段监听,避免被面板快捷键吃掉)。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onCancel();
        return;
      }
      if (event.key === "Enter" && !event.isComposing) {
        event.preventDefault();
        event.stopPropagation();
        void confirm();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [confirm, onCancel]);

  const handlePointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (busyRef.current || event.button !== 0) return;
      // 工具条区域不开始新框选(按钮自己处理点击)。
      if ((event.target as HTMLElement).closest(".screen-clip-toolbar") !== null) return;
      event.currentTarget.setPointerCapture(event.pointerId);
      const point = pointFromEvent(event);
      dragStartRef.current = point;
      setCursor(point);
      setDragging(true);
      setSelection(normalizeRect(point, point));
    },
    [],
  );

  const handlePointerMove = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const point = pointFromEvent(event);
      setCursor(point);
      const start = dragStartRef.current;
      if (!start) return;
      setSelection(clampRectToBounds(normalizeRect(start, point), viewport));
    },
    [viewport],
  );

  const handlePointerUp = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const start = dragStartRef.current;
      dragStartRef.current = null;
      setDragging(false);
      if (!start) return;
      const rect = clampRectToBounds(normalizeRect(start, pointFromEvent(event)), viewport);
      setSelection(isSelectionTooSmall(rect) ? null : rect);
    },
    [viewport],
  );

  const handlePointerCancel = useCallback(() => {
    dragStartRef.current = null;
    setDragging(false);
  }, []);

  // 尺寸读数:视口 CSS 像素(与用户拖拽感知一致)。
  const readout = selection
    ? `${Math.round(selection.width)} × ${Math.round(selection.height)}`
    : "";

  const selectionStyle = useMemo<CSSProperties | null>(() => {
    if (!selection) return null;
    return {
      left: `${selection.x}px`,
      top: `${selection.y}px`,
      width: `${selection.width}px`,
      height: `${selection.height}px`,
    };
  }, [selection]);

  // 读数相对选区定位(选区顶部空间不足时放到框内),避免越出视口。
  const readoutStyle = useMemo<CSSProperties | null>(() => {
    if (!selection) return null;
    return selection.y >= 28 ? { left: 0, top: "-24px" } : { left: 0, top: "6px" };
  }, [selection]);

  const toolbarStyle = useMemo<CSSProperties | null>(() => {
    if (!selection) return null;
    const maxLeft = Math.max(EDGE_GAP, viewport.width - TOOLBAR_WIDTH - EDGE_GAP);
    const left = Math.min(
      Math.max(EDGE_GAP, selection.x + selection.width - TOOLBAR_WIDTH),
      maxLeft,
    );
    const below = selection.y + selection.height + 10;
    const top =
      below + TOOLBAR_HEIGHT <= viewport.height - EDGE_GAP
        ? below
        : Math.max(EDGE_GAP, selection.y - TOOLBAR_HEIGHT - 10);
    return { left: `${left}px`, top: `${top}px` };
  }, [selection, viewport]);

  return (
    <div
      ref={rootRef}
      className="screen-clip-root"
      role="dialog"
      aria-modal="true"
      aria-label="屏幕框选截图"
      tabIndex={-1}
      data-testid="screen-clip-overlay"
      style={{ backgroundImage: `url(${capture.dataUrl})` }}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      onPointerCancel={handlePointerCancel}
    >
      {selection === null ? <div className="screen-clip-shade" aria-hidden="true" /> : null}

      {cursor !== null && !dragging && !busy ? (
        <>
          <div
            className="screen-clip-hairline screen-clip-hairline-v"
            style={{ left: `${cursor.x}px` }}
            aria-hidden="true"
          />
          <div
            className="screen-clip-hairline screen-clip-hairline-h"
            style={{ top: `${cursor.y}px` }}
            aria-hidden="true"
          />
        </>
      ) : null}

      {selection !== null && selectionStyle !== null ? (
        <div className="screen-clip-selection" style={selectionStyle} aria-hidden="true">
          {readout !== "" && readoutStyle !== null ? (
            <span className="screen-clip-readout" style={readoutStyle}>
              {readout}
            </span>
          ) : null}
        </div>
      ) : null}

      {!busy ? (
        <div className="screen-clip-hint" aria-hidden="true">
          拖拽选择截图区域 · Esc 取消 · Enter 确认
        </div>
      ) : (
        <div className="screen-clip-hint" role="status">
          正在生成截图…
        </div>
      )}

      {toolbarStyle !== null ? (
        <div
          className="screen-clip-toolbar"
          style={toolbarStyle}
          role="toolbar"
          aria-label="截图操作"
          onPointerDown={(event) => event.stopPropagation()}
        >
          <span className="screen-clip-toolbar-size">{readout}</span>
          <button
            type="button"
            className="screen-clip-btn"
            onClick={onCancel}
            title="取消(Esc)"
            aria-label="取消截图"
          >
            取消
          </button>
          <button
            type="button"
            className="screen-clip-btn screen-clip-btn-primary"
            onClick={() => void confirm()}
            disabled={busy}
            title="确定(Enter)"
            aria-label="确认截图选区"
            autoFocus
          >
            {busy ? "生成中…" : "确定"}
          </button>
        </div>
      ) : null}
    </div>
  );
}
