// 双文档对照的中间分隔条:真实指针拖动 + 键盘 ←/→ 微调 + 双击恢复 50/50。
// 比例由宿主写入 uiStore(持久化);两侧最小宽度 240px,容器过窄时按 50/50 夹取。
//
// 与 PanelResizer 相同,用 window 级 pointermove/pointerup 而不是
// setPointerCapture:WebView2 在嵌套/注入鼠标事件下会丢 capture。
import { useCallback, useEffect, useRef, type ReactNode } from "react";

export const COMPARE_SPLITTER_PX = 12;
/** 与 reading.css 的容器查询阈值一致:更窄时上下堆叠,不显示分隔条。 */
export const COMPARE_STACK_BREAKPOINT_PX = 520;
const MIN_PANE_PX = 240;
const KEY_STEP = 0.02;

function clampForContainer(ratio: number, containerWidth: number): number {
  const usable = containerWidth - COMPARE_SPLITTER_PX;
  if (!Number.isFinite(ratio) || usable <= 0) return 0.5;
  const minRatio = Math.min(0.5, MIN_PANE_PX / usable);
  return Math.min(1 - minRatio, Math.max(minRatio, ratio));
}

export function CompareSplitter({
  containerRef,
  ratio,
  onChange,
  onReset,
}: {
  containerRef: React.RefObject<HTMLDivElement | null>;
  ratio: number;
  onChange: (ratio: number) => void;
  onReset: () => void;
}): ReactNode {
  const dragRef = useRef<{
    move: (event: PointerEvent) => void;
    up: () => void;
    cancel: () => void;
  } | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  const ratioAt = useCallback(
    (clientX: number): number | null => {
      const container = containerRef.current;
      if (container === null) return null;
      const rect = container.getBoundingClientRect();
      const usable = rect.width - COMPARE_SPLITTER_PX;
      if (usable <= 0) return null;
      return clampForContainer((clientX - rect.left) / usable, rect.width);
    },
    [containerRef],
  );

  const cleanup = useCallback(() => {
    const drag = dragRef.current;
    if (drag === null) return;
    window.removeEventListener("pointermove", drag.move);
    window.removeEventListener("pointerup", drag.up);
    window.removeEventListener("pointercancel", drag.cancel);
    dragRef.current = null;
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
  }, []);

  useEffect(() => cleanup, [cleanup]);

  // 窗口/右栏变化后,持久化的比例可能不再满足两侧最小宽度:
  // 主区宽度变化时把比例收敛回可满足的范围(堆叠模式不改比例)。
  useEffect(() => {
    const container = containerRef.current;
    if (container === null) return;
    const fix = () => {
      const width = container.getBoundingClientRect().width;
      if (width <= COMPARE_STACK_BREAKPOINT_PX) return;
      const clamped = clampForContainer(ratio, width);
      if (Math.abs(clamped - ratio) > 0.001) onChangeRef.current(clamped);
    };
    fix();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(fix);
    observer.observe(container);
    return () => observer.disconnect();
  }, [containerRef, ratio]);

  const onPointerDown = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      if (event.button !== 0) return;
      // preventDefault 会阻止浏览器默认聚焦:显式聚焦,保证点击后键盘可用。
      event.currentTarget.focus();
      event.preventDefault();
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      const move = (moveEvent: PointerEvent) => {
        const next = ratioAt(moveEvent.clientX);
        if (next !== null) onChangeRef.current(next);
      };
      const up = () => cleanup();
      const cancel = () => cleanup();
      dragRef.current = { move, up, cancel };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
      window.addEventListener("pointercancel", cancel);
      const initial = ratioAt(event.clientX);
      if (initial !== null) onChangeRef.current(initial);
    },
    [cleanup, ratioAt],
  );

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      event.preventDefault();
      const container = containerRef.current;
      if (container === null) return;
      const width = container.getBoundingClientRect().width;
      const delta = event.key === "ArrowLeft" ? -KEY_STEP : KEY_STEP;
      onChangeRef.current(clampForContainer(ratio + delta, width));
    },
    [containerRef, ratio],
  );

  return (
    <div
      className="compare-splitter"
      role="separator"
      aria-orientation="vertical"
      aria-label="调整双文档分栏宽度"
      aria-valuemin={25}
      aria-valuemax={75}
      aria-valuenow={Math.round(ratio * 100)}
      tabIndex={0}
      data-ratio={ratio.toFixed(3)}
      onPointerDown={onPointerDown}
      onDoubleClick={onReset}
      onKeyDown={onKeyDown}
    >
      <span className="compare-splitter-grip" aria-hidden="true" />
    </div>
  );
}
