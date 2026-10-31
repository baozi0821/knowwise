// 翻译的三种范围(与 inlineTranslation 的逐块对照互补):
// - 单句:划词工具条的「翻译」按钮(就地浮层,不建任务);
// - 整页:PDF 当前页 → 一整段译文面板(不是逐块贴条);
// - 整篇:走既有产物流水线 translation_generate(任务 + 资料里的 translation.v1)。
//
// 三者共用同一 targetLanguage(readerTools),全部经 EgressGateway;
// 结果里 engine 字段如实标注 local / model,绝不把远程结果标成本地。
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, ReactNode } from "react";
import { RotateCcw } from "lucide-react";
import type { CommandClient } from "../../app/api/client";
import { useWorkspaceUi, type TranslationPanelRect } from "../workspace/uiStore";

export const TRANSLATE_LANGUAGES: Array<{ value: string; label: string }> = [
  { value: "zh-CN", label: "中文" },
  { value: "en-US", label: "English" },
  { value: "ja-JP", label: "日本語" },
  { value: "ko-KR", label: "한국어" },
  { value: "de-DE", label: "Deutsch" },
  { value: "fr-FR", label: "Français" },
  { value: "es-ES", label: "Español" },
];

const CHUNK_CHARS = 1800;
const MAX_TOTAL_CHARS = 12_000;

/**
 * 翻译类 IPC 的前端等待上限。
 * 提供方请求超时为 120s(设置可调至 600s),而默认 IPC 超时只有 30s:
 * 同步翻译请求在慢模型/长段落上会被前端误判为 IPC_TIMEOUT,后台其实仍在执行。
 * 这里给足 180s(120s 提供方时限 + 连接级重试余量),失败仍如实显示错误码。
 */
export const TRANSLATION_IPC_TIMEOUT_MS = 180_000;

/** 把一页的行合并成不超过 12k 字符的片段(按行边界切,尽量不切断句子)。 */
export function chunkPageLines(lines: string[]): { chunks: string[]; truncated: boolean } {
  const text = lines.join(" ").replace(/\s+/g, " ").trim();
  if (text.length === 0) return { chunks: [], truncated: false };
  const truncated = text.length > MAX_TOTAL_CHARS;
  const source = truncated ? text.slice(0, MAX_TOTAL_CHARS) : text;
  const chunks: string[] = [];
  let rest = source;
  while (rest.length > CHUNK_CHARS) {
    let cut = rest.lastIndexOf(". ", CHUNK_CHARS);
    if (cut < CHUNK_CHARS / 3) cut = rest.lastIndexOf(" ", CHUNK_CHARS);
    if (cut < CHUNK_CHARS / 3) cut = CHUNK_CHARS;
    chunks.push(rest.slice(0, cut + 1).trim());
    rest = rest.slice(cut + 1).trim();
  }
  if (rest.length > 0) chunks.push(rest);
  return { chunks, truncated };
}

export type PageTranslationState = {
  loading: boolean;
  text: string | null;
  errorCode: string | null;
  engine: string | null;
  target: string | null;
  pageNumber: number | null;
  truncated: boolean;
};

const EMPTY_PAGE_TRANSLATION: PageTranslationState = {
  loading: false,
  text: null,
  errorCode: null,
  engine: null,
  target: null,
  pageNumber: null,
  truncated: false,
};

function errorCodeOf(error: unknown): string {
  if (typeof error === "object" && error !== null && "appError" in error) {
    return String((error as { appError: { code?: string } }).appError.code ?? "translation_failed");
  }
  return "translation_failed";
}

/**
 * 整页翻译:当前 PDF 页 → 一整段译文。
 * 按 (page, target) 缓存,滚动来回不会重复调用模型。
 */
export function usePdfPageTranslation(
  client: CommandClient | undefined,
  documentId: string | undefined,
  lines: string[] | null,
  pageNumber: number,
  enabled: boolean,
  target: string,
): PageTranslationState & { retry: () => void } {
  const [state, setState] = useState<PageTranslationState>(EMPTY_PAGE_TRANSLATION);
  const cacheRef = useRef(new Map<string, { text: string; engine: string; truncated: boolean }>());
  const runIdRef = useRef(0);
  const [retryTick, setRetryTick] = useState(0);

  useEffect(() => {
    if (!enabled || client === undefined || documentId === undefined || lines === null) {
      setState(EMPTY_PAGE_TRANSLATION);
      return;
    }
    const { chunks, truncated } = chunkPageLines(lines);
    if (chunks.length === 0) {
      setState({ ...EMPTY_PAGE_TRANSLATION, target, pageNumber });
      return;
    }
    const cacheKey = `${pageNumber}\u0000${target}`;
    const cached = cacheRef.current.get(cacheKey);
    if (cached !== undefined) {
      setState({
        loading: false,
        text: cached.text,
        errorCode: null,
        engine: cached.engine,
        target,
        pageNumber,
        truncated: cached.truncated,
      });
      return;
    }
    const runId = ++runIdRef.current;
    setState({
      loading: true,
      text: null,
      errorCode: null,
      engine: null,
      target,
      pageNumber,
      truncated,
    });
    void (async () => {
      try {
        const result = await client.call<{
          targetLanguage: string;
          engine: string;
          segments: Array<{ source: string; translated: string | null; errorCode: string | null }>;
        }>("translation_inline", { documentId, targetLanguage: target, segments: chunks }, { timeoutMs: TRANSLATION_IPC_TIMEOUT_MS });
        if (runIdRef.current !== runId) return;
        const failed = result.segments.find((s) => s.translated === null);
        const text = result.segments
          .map((s) => s.translated ?? "")
          .filter((t) => t.trim() !== "")
          .join("\n\n");
        if (text === "") {
          setState({
            loading: false,
            text: null,
            errorCode: failed?.errorCode ?? "translation_failed",
            engine: result.engine,
            target,
            pageNumber,
            truncated,
          });
          return;
        }
        cacheRef.current.set(cacheKey, { text, engine: result.engine, truncated });
        setState({
          loading: false,
          text,
          errorCode: failed?.errorCode ?? null,
          engine: result.engine,
          target,
          pageNumber,
          truncated,
        });
      } catch (error) {
        if (runIdRef.current !== runId) return;
        setState({
          loading: false,
          text: null,
          errorCode: errorCodeOf(error),
          engine: null,
          target,
          pageNumber,
          truncated,
        });
      }
    })();
    return () => {
      runIdRef.current += 1;
    };
  }, [client, documentId, lines, pageNumber, enabled, target, retryTick]);

  const retry = useCallback(() => {
    cacheRef.current.delete(`${pageNumber}\u0000${target}`);
    setRetryTick((n) => n + 1);
  }, [pageNumber, target]);

  return { ...state, retry };
}

const ENGINE_LABELS: Record<string, string> = {
  local: "本地引擎",
  model: "AI 模型",
};

const PANEL_MARGIN = 8;
const MIN_PANEL_W = 260;
const MIN_PANEL_H = 150;
const DEFAULT_PANEL_W = 380;

function clamp(value: number, lo: number, hi: number): number {
  return Math.min(Math.max(value, lo), Math.max(lo, hi));
}

function defaultPanelRect(container: DOMRect, topOffset = 52): TranslationPanelRect {
  const availableW = Math.max(120, container.width - PANEL_MARGIN * 2);
  const availableH = Math.max(120, container.height - PANEL_MARGIN * 2);
  const w = Math.min(Math.max(320, DEFAULT_PANEL_W), availableW);
  const h = Math.min(Math.max(240, Math.round(window.innerHeight * 0.46)), availableH);
  return {
    w,
    h,
    x: Math.max(PANEL_MARGIN, container.width - w - 14),
    y: Math.max(
      PANEL_MARGIN,
      Math.min(topOffset, container.height - h - PANEL_MARGIN),
    ),
  };
}

function toolbarBottomOffset(host: HTMLElement, bounds: DOMRect): number {
  const toolbar = host.querySelector(".reader-toolbar");
  if (toolbar === null) return 52;
  const toolbarRect = toolbar.getBoundingClientRect();
  return Math.max(PANEL_MARGIN + 4, Math.round(toolbarRect.bottom - bounds.top + 6));
}

function clampPanelRect(rect: TranslationPanelRect, container: DOMRect): TranslationPanelRect {
  const maxW = Math.max(80, container.width - PANEL_MARGIN * 2);
  const maxH = Math.max(80, container.height - PANEL_MARGIN * 2);
  const w = clamp(rect.w, Math.min(MIN_PANEL_W, maxW), maxW);
  const h = clamp(rect.h, Math.min(MIN_PANEL_H, maxH), maxH);
  return {
    w,
    h,
    x: clamp(rect.x, PANEL_MARGIN, container.width - w - PANEL_MARGIN),
    y: clamp(rect.y, PANEL_MARGIN, container.height - h - PANEL_MARGIN),
  };
}

function applyPanelGesture(
  start: TranslationPanelRect,
  container: DOMRect,
  mode: "move" | "resize",
  dir: string,
  dx: number,
  dy: number,
): TranslationPanelRect {
  const maxRight = container.width - PANEL_MARGIN;
  const maxBottom = container.height - PANEL_MARGIN;
  if (mode === "move") {
    return {
      ...start,
      x: clamp(start.x + dx, PANEL_MARGIN, maxRight - start.w),
      y: clamp(start.y + dy, PANEL_MARGIN, maxBottom - start.h),
    };
  }
  let { x, y, w, h } = start;
  // 窄阅读区(min dock)时允许小于常规最小值,但绝不超过容器;状态与视觉一致。
  const maxW = Math.max(40, maxRight - start.x);
  const maxH = Math.max(40, maxBottom - start.y);
  const minW = Math.min(MIN_PANEL_W, maxW);
  const minH = Math.min(MIN_PANEL_H, maxH);
  if (dir.includes("e")) w = clamp(start.w + dx, minW, maxW);
  if (dir.includes("s")) h = clamp(start.h + dy, minH, maxH);
  if (dir.includes("w")) {
    x = clamp(start.x + dx, PANEL_MARGIN, start.x + start.w - minW);
    w = start.w + (start.x - x);
  }
  if (dir.includes("n")) {
    y = clamp(start.y + dy, PANEL_MARGIN, start.y + start.h - minH);
    h = start.h + (start.y - y);
  }
  return { x, y, w, h };
}

const RESIZE_DIRECTIONS = ["n", "ne", "e", "se", "s", "sw", "w", "nw"] as const;

/**
 * 整页译文浮层:可拖动标题栏移动、8 向调整大小、内容区独立滚动。
 * 位置/尺寸按容器边界收敛并落 uiStore(localStorage);
 * 容器尺寸变化时自动收回到阅读区内;可一键恢复默认。
 */
export function PageTranslationPanel({
  state,
  onClose,
}: {
  state: PageTranslationState & { retry: () => void };
  onClose: () => void;
}): ReactNode {
  const persistedLayout = useWorkspaceUi((s) => s.pageTranslationLayout);
  const setPersistedLayout = useWorkspaceUi((s) => s.setPageTranslationLayout);
  const [rect, setRect] = useState<TranslationPanelRect | null>(null);
  const rectRef = useRef<TranslationPanelRect | null>(null);
  const panelRef = useRef<HTMLElement | null>(null);
  const initializedRef = useRef(false);

  const visible = state.loading || state.text !== null || state.errorCode !== null;

  const commitRect = useCallback((next: TranslationPanelRect) => {
    rectRef.current = next;
    setRect(next);
  }, []);

  useLayoutEffect(() => {
    if (!visible) {
      initializedRef.current = false;
      return;
    }
    if (initializedRef.current) return;
    const host = panelRef.current?.parentElement;
    if (!host) return;
    const bounds = host.getBoundingClientRect();
    commitRect(
      clampPanelRect(
        persistedLayout ?? defaultPanelRect(bounds, toolbarBottomOffset(host, bounds)),
        bounds,
      ),
    );
    initializedRef.current = true;
  }, [visible, persistedLayout, commitRect]);

  useEffect(() => {
    if (!visible) return;
    const host = panelRef.current?.parentElement;
    if (!host || typeof ResizeObserver === "undefined") return;
    let frame = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const bounds = host.getBoundingClientRect();
        const current = rectRef.current;
        if (current !== null) commitRect(clampPanelRect(current, bounds));
      });
    });
    observer.observe(host);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [visible, commitRect]);

  const beginGesture = (
    event: ReactPointerEvent,
    mode: "move" | "resize",
    dir = "",
  ): void => {
    if (event.button !== 0) return;
    const host = panelRef.current?.parentElement;
    const start = rectRef.current;
    if (!host || start === null) return;
    if (mode === "move" && (event.target as HTMLElement).closest("button") !== null) return;
    event.preventDefault();
    const bounds = host.getBoundingClientRect();
    const originX = event.clientX;
    const originY = event.clientY;
    const onMove = (ev: PointerEvent): void => {
      commitRect(
        applyPanelGesture(start, bounds, mode, dir, ev.clientX - originX, ev.clientY - originY),
      );
    };
    const onEnd = (): void => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onEnd);
      window.removeEventListener("pointercancel", onEnd);
      if (rectRef.current !== null) setPersistedLayout(rectRef.current);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onEnd);
    window.addEventListener("pointercancel", onEnd);
  };

  const restoreDefault = (): void => {
    const host = panelRef.current?.parentElement;
    if (!host) return;
    const bounds = host.getBoundingClientRect();
    commitRect(defaultPanelRect(bounds, toolbarBottomOffset(host, bounds)));
    setPersistedLayout(null);
  };

  if (!visible) return null;
  return (
    <aside
      ref={panelRef}
      className="pdf-translation-panel"
      aria-label="整页译文"
      role="complementary"
      style={
        rect === null
          ? undefined
          : { left: rect.x, top: rect.y, width: rect.w, height: rect.h }
      }
    >
      <header
        className="pdf-translation-head"
        title="拖动移动;拖动边缘或角调整大小"
        onPointerDown={(event) => beginGesture(event, "move")}
      >
        <span className="pdf-translation-title">
          整页译文 · 第 {state.pageNumber ?? "-"} 页
          {state.target !== null ? ` · ${state.target}` : ""}
        </span>
        <span className="pdf-translation-actions">
          {state.engine !== null ? (
            <span className="pdf-translation-engine" title="翻译引擎(本地/模型如实标注)">
              {ENGINE_LABELS[state.engine] ?? state.engine}
            </span>
          ) : null}
          <button
            type="button"
            className="pdf-translation-reset"
            aria-label="恢复默认位置和大小"
            title="恢复默认位置和大小"
            onClick={restoreDefault}
          >
            <RotateCcw className="w-3 h-3" aria-hidden="true" />
          </button>
          <button type="button" className="pdf-translation-close" aria-label="关闭整页译文" onClick={onClose}>
            ×
          </button>
        </span>
      </header>
      {state.loading ? (
        <p className="pdf-translation-loading" role="status">
          正在翻译第 {state.pageNumber ?? ""} 页…
        </p>
      ) : state.text !== null ? (
        <div className="pdf-translation-body">
          {state.text.split(/\n{2,}/).map((paragraph, i) => (
            <p key={i}>{paragraph}</p>
          ))}
          {state.truncated ? (
            <p className="pdf-translation-note" role="note">
              本页文本超过 12000 字符,仅翻译了前 12000 字符。
            </p>
          ) : null}
          {state.errorCode !== null ? (
            <p className="pdf-translation-note" role="alert">
              部分片段未译出({state.errorCode}),其余照常显示。
            </p>
          ) : null}
          <button type="button" className="btn small" onClick={state.retry}>
            重新翻译本页
          </button>
        </div>
      ) : (
        <div className="pdf-translation-body">
          <p className="pdf-translation-error" role="alert">
            整页翻译未完成({state.errorCode ?? "translation_failed"})。原文未改动,可重试。
          </p>
          <button type="button" className="btn small" onClick={state.retry}>
            重试
          </button>
        </div>
      )}
      {RESIZE_DIRECTIONS.map((dir) => (
        <span
          key={dir}
          className={`pdf-translation-resize ${dir}`}
          aria-hidden="true"
          data-dir={dir}
          onPointerDown={(event) => beginGesture(event, "resize", dir)}
        />
      ))}
    </aside>
  );
}

/**
 * 整篇翻译:走产物流水线(冻结全文快照 → translation_generate → 任务中心/资料)。
 * 只提交任务并如实返回 taskId;完成与否由任务中心与资料页呈现。
 * 目标语言来自阅读区下拉;术语表是全应用唯一来源(设置),读取失败按空表提交。
 */
export async function loadTranslationGlossary(
  client: CommandClient,
): Promise<Array<{ source: string; target: string }>> {
  try {
    const view = await client.call<{
      settings?: { translationGlossary?: Array<{ source: string; target: string }> };
    }>("settings_get", {});
    return (view.settings?.translationGlossary ?? [])
      .filter((entry) => entry.source.trim() !== "" && entry.target.trim() !== "")
      .map((entry) => ({ source: entry.source, target: entry.target }));
  } catch {
    return [];
  }
}

export async function submitWholeDocumentTranslation(
  client: CommandClient,
  documentId: string,
  targetLanguage: string,
  title: string,
  glossary: Array<{ source: string; target: string }> = [],
): Promise<{ taskId: string }> {
  const snapshot = await client.call<{ id: string }>("context_freeze", {
    documentId,
    scopeDraft: { kind: "full_document", confirmed: true },
    idempotencyKey: `${client.newIdempotencyKey()}:translate-full`,
  });
  const task = await client.call<{ id: string }>("translation_generate", {
    documentId,
    snapshotId: snapshot.id,
    targetLanguage,
    style: "faithful",
    glossary,
    title,
    idempotencyKey: client.newIdempotencyKey(),
  });
  return { taskId: task.id };
}
