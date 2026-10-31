// 面板拖拽调宽/调高手柄:left/right 调左右面板宽度,top 调底部面板高度。
// 按住拖动实时回调(宽度钳制在 store setter 中);键盘 ←/→(↑/↓)微调,
// 双击恢复默认宽度 —— 让"能伸缩"既看得见也能用键盘做到。
//
// 实现说明:使用 mousedown + window 级 mousemove/mouseup,而不是
// pointer capture —— WebView2 里 setPointerCapture 在部分鼠标事件注入/嵌套
// 窗口场景不可靠(会中途丢失 move,拖拽只动一下),窗口级鼠标监听在桌面
// 壳里稳定,且不依赖真实指针 id。
import { useCallback, useEffect, useRef, type ReactNode } from "react";

export function PanelResizer({
  side,
  onResize,
  label,
  value,
  min = 200,
  max = 1200,
  step = 16,
  onReset,
}: {
  /** 手柄语义:left/right 调宽度,top 调高度(底部面板上缘)。 */
  side: "left" | "right" | "top";
  onResize: (size: number) => void;
  /** 无障碍名称,例如「调整右侧面板宽度」。 */
  label?: string;
  /** 当前尺寸(仅用于 aria-valuenow 播报)。 */
  value?: number;
  min?: number;
  max?: number;
  step?: number;
  /** 双击恢复默认尺寸。 */
  onReset?: () => void;
}): ReactNode {
  const horizontal = side === "top";
  const dragRef = useRef<{
    move: (event: MouseEvent) => void;
    up: () => void;
  } | null>(null);
  const onResizeRef = useRef(onResize);
  onResizeRef.current = onResize;

  // 卸载时清理:拖拽中切组件/切文档不留下全局监听与光标样式。
  useEffect(() => {
    return () => {
      const drag = dragRef.current;
      if (drag !== null) {
        window.removeEventListener("mousemove", drag.move);
        window.removeEventListener("mouseup", drag.up);
        dragRef.current = null;
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      }
    };
  }, []);

  const onMouseDown = useCallback(
    (event: React.MouseEvent<HTMLDivElement>) => {
      if (event.button !== 0) return;
      event.preventDefault();
      document.body.style.cursor = horizontal ? "row-resize" : "col-resize";
      document.body.style.userSelect = "none";

      const move = (moveEvent: MouseEvent) => {
        const size = horizontal
          ? window.innerHeight - moveEvent.clientY
          : side === "left"
            ? moveEvent.clientX
            : window.innerWidth - moveEvent.clientX;
        onResizeRef.current(size);
      };
      const up = () => {
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
        dragRef.current = null;
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      };
      dragRef.current = { move, up };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
      // 立即按按下点计算一次:点击手柄本身也会给出接近当前位置的尺寸,
      // 避免"点一下不动"的错觉。
      move(event.nativeEvent);
    },
    [horizontal, side],
  );

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      const current = value ?? min;
      const forward = side === "top" ? "ArrowUp" : side === "left" ? "ArrowRight" : "ArrowLeft";
      const backward = side === "top" ? "ArrowDown" : side === "left" ? "ArrowLeft" : "ArrowRight";
      if (event.key === forward) {
        event.preventDefault();
        onResizeRef.current(Math.min(max, current + step));
      } else if (event.key === backward) {
        event.preventDefault();
        onResizeRef.current(Math.max(min, current - step));
      } else if (event.key === "Home" || event.key === "End") {
        event.preventDefault();
        onResizeRef.current(event.key === "Home" ? min : max);
      }
    },
    [max, min, side, step, value],
  );

  return (
    <div
      className={`panel-resizer${horizontal ? " horizontal" : ""}`}
      role="separator"
      aria-orientation={horizontal ? "horizontal" : "vertical"}
      aria-label={label ?? (horizontal ? "调整面板高度" : "调整面板宽度")}
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuenow={value}
      tabIndex={0}
      onMouseDown={onMouseDown}
      onDoubleClick={onReset}
      onKeyDown={onKeyDown}
    />
  );
}
