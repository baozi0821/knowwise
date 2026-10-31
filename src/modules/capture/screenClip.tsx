// 屏幕框选截图:对外 API 与全局挂载点(capture 拥有,契约见 DESIGN §2)。
//
// - `useScreenClip()` 供 AI 输入框 / 笔记编辑器的「截图」按钮调用;
// - `ScreenClipProvider` 由组装根(bootstrap)挂载一次,内部负责:
//   1) 调原生 `capture_screen` 拿整屏 PNG;2) 挂载全屏框选层;3) 把压缩后的
//   data-URI resolve 给 `requestClip`。
//
// 无 Provider 或非 Tauri 环境(浏览器 / jsdom / 纯前端预览)时 supported=false、
// requestClip 恒返回 null——调用方无需额外判断环境。

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { captureScreen, type CaptureScreenResult } from "../../app/ipc/transport";
import { ScreenClipOverlay } from "./ScreenClipOverlay";

export interface ScreenClipApi {
  /** 原生捕获能力是否可用(浏览器 / jsdom 为 false)。 */
  supported: boolean;
  /** 框选屏幕区域 → 压缩后的 data-URI;取消 / 失败 / 不支持 → null。 */
  requestClip: () => Promise<string | null>;
}

/** 无 Provider 时的兜底:静默不可用,不抛错(调用方直接降级)。 */
const UNAVAILABLE: ScreenClipApi = {
  supported: false,
  requestClip: async () => null,
};

const ScreenClipContext = createContext<ScreenClipApi | null>(null);

/** 取截图能力;必须在 `ScreenClipProvider` 内才能拿到真实实现。 */
export function useScreenClip(): ScreenClipApi {
  return useContext(ScreenClipContext) ?? UNAVAILABLE;
}

/**
 * 原生能力探测:Tauri v2 在 WebView 中注入 `__TAURI_INTERNALS__`
 * (旧版为 `__TAURI__`)。浏览器 / jsdom 两者都没有 → 返回 false。
 */
function hasNativeCapture(): boolean {
  if (typeof window === "undefined") return false;
  const w = window as unknown as {
    __TAURI_INTERNALS__?: unknown;
    __TAURI__?: unknown;
  };
  return typeof w.__TAURI_INTERNALS__ === "object" || typeof w.__TAURI__ === "object";
}

type PendingResolve = (value: string | null) => void;

/**
 * 全局框选层宿主。children 原样渲染;框选过程中在 document.body 上以 Portal
 * 覆盖整个 WebView(避免被工作区容器的 overflow/transform 裁剪)。
 */
export function ScreenClipProvider({ children }: { children: ReactNode }) {
  const supported = useMemo(hasNativeCapture, []);
  const pendingRef = useRef<PendingResolve | null>(null);
  const [capture, setCapture] = useState<CaptureScreenResult | null>(null);

  /** 结束当前框选:清空层并把结果交给等待中的 requestClip。 */
  const finish = useCallback((value: string | null) => {
    const resolve = pendingRef.current;
    pendingRef.current = null;
    setCapture(null);
    resolve?.(value);
  }, []);

  const requestClip = useCallback(async (): Promise<string | null> => {
    if (!supported) return null;
    // 同一时刻只允许一层框选:重复点击直接视为取消本次请求。
    if (pendingRef.current !== null) return null;
    let shot: CaptureScreenResult;
    try {
      shot = await captureScreen();
    } catch {
      return null;
    }
    if (!shot || typeof shot.dataUrl !== "string" || shot.width <= 0 || shot.height <= 0) {
      return null;
    }
    const effective = shot;
    return new Promise<string | null>((resolve) => {
      pendingRef.current = resolve;
      setCapture(effective);
    });
  }, [supported]);

  // Provider 卸载(应用关闭)时,进行中的 requestClip 以取消收尾。
  useEffect(
    () => () => {
      pendingRef.current?.(null);
      pendingRef.current = null;
    },
    [],
  );

  const api = useMemo<ScreenClipApi>(() => ({ supported, requestClip }), [supported, requestClip]);

  return (
    <ScreenClipContext.Provider value={api}>
      {children}
      {capture !== null && typeof document !== "undefined"
        ? createPortal(
            <ScreenClipOverlay
              capture={capture}
              onCancel={() => finish(null)}
              onConfirm={(dataUrl) => finish(dataUrl)}
            />,
            document.body,
          )
        : null}
    </ScreenClipContext.Provider>
  );
}
