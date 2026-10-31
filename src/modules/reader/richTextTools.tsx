// Markdown/DOCX 视图的交互层:工具栏(画笔/橡皮/大纲/源码模式/字数)、
// 段落锚定涂鸦(保存为真实标注,重开不丢)、大纲弹出面板、笔记跳转监听。
// 涂鸦笔迹归一化到所在段落盒;锚点 locator 记录该段落的文本区间,
// 渲染时按区间找回段落并覆盖 SVG(缩放/换行变化按比例重排)。
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  PenLine,
  Highlighter,
  Eraser,
  ListTree,
  FileCode2,
  Search,
  ZoomIn,
  ZoomOut,
  Type,
  Languages,
  Undo2,
  Redo2,
  Lasso,
  Square,
  Circle,
  Minus,
  MoveUpRight,
  Layers,
} from "lucide-react";
import { ReaderSearch } from "./ReaderSearch";
import { ReaderToolbarExtras } from "./readerToolbarExtras";
import type { AnchorView } from "../../shared/generated/AnchorView";
import type { AnnotationView } from "../../shared/generated/AnnotationView";
import type { RawSelectionInput } from "../../shared/generated/RawSelectionInput";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import type { CommandClient } from "../../app/api/client";
import { useReaderTools, PEN_COLORS } from "./readerTools";
import { TextBoxEditor, TextBoxBubble, type TextBoxEditorState } from "./textBox";
import { decodeTextBox, DEFAULT_TEXTBOX_STYLE } from "./annotations";
import {
  TRANSLATE_LANGUAGES,
  loadTranslationGlossary,
  submitWholeDocumentTranslation,
} from "./translationScopes";

/** 线宽比例 ↔ 滑杆(2–10)。 */
function richPenSlider(width: number): number {
  return Math.round(Math.min(10, Math.max(2, width * 1000)));
}
function richSliderToWidth(value: number): number {
  return Math.min(0.012, Math.max(0.0015, value / 1000));
}
/** #rrggbb + 透明度;非 hex 原样返回。 */
function richWithAlpha(color: string, alpha: number): string {
  const m = /^#([0-9a-f]{6})$/i.exec(color.trim());
  if (m === null) return color;
  const v = parseInt(m[1]!, 16);
  return `rgba(${(v >> 16) & 0xff}, ${(v >> 8) & 0xff}, ${v & 0xff}, ${alpha})`;
}
import { broadcastAnnotationChange, type AnnotationChangeDetail } from "./annotationEvents";
import { inkStrokeElements } from "./InkSvg";
import {
  decodeInk,
  encodeInk,
  inkBlockRange,
  nearestInkBlock,
  eraseInkCircle,
  type InkPoint,
  type InkStroke,
} from "./annotations";
import {
  InkGestureRecorder,
  canRedoInk,
  canUndoInk,
  clearInkHistory,
  pushInkHistory,
  redoInk,
  undoInk,
  useInkHistoryHotkeys,
  useInkHistoryVersion,
} from "./inkHistory";
import {
  makeShapeStroke,
  pointInPolygon,
  strokeSamplePoints,
  translateStrokes,
} from "./inkGeometry";
import { ConfirmDialog, type ConfirmRequest } from "./ConfirmDialog";
import { isRevisionConflict, useAnnotationConflict } from "./annotationConflict";
import { loadDocumentPen, saveDocumentPen } from "./readerTools";

// —— 工具栏 ——

export function RichTextToolbar({
  format,
  rootRef,
  disabled,
  client,
  documentId,
  documentTitle,
  onToast,
}: {
  format: "markdown" | "docx";
  rootRef: React.RefObject<HTMLElement | null>;
  disabled?: boolean;
  client?: CommandClient;
  documentId?: string;
  documentTitle?: string;
  onToast?: (text: string) => void;
}): ReactNode {
  const tool = useReaderTools((s) => s.tool);
  const penColor = useReaderTools((s) => s.penColor);
  const mdSource = useReaderTools((s) => s.mdSource);
  const toggleTool = useReaderTools((s) => s.toggleTool);
  const setPenColor = useReaderTools((s) => s.setPenColor);
  const penWidth = useReaderTools((s) => s.penWidth);
  const setPenWidth = useReaderTools((s) => s.setPenWidth);
  const eraserRadius = useReaderTools((s) => s.eraserRadius);
  const setEraserRadius = useReaderTools((s) => s.setEraserRadius);
  const toggleMdSource = useReaderTools((s) => s.toggleMdSource);
  const inlineTranslate = useReaderTools((s) => s.inlineTranslate);
  const setInlineTranslate = useReaderTools((s) => s.setInlineTranslate);
  const inlineTarget = useReaderTools((s) => s.inlineTranslateTarget);
  const setInlineTarget = useReaderTools((s) => s.setInlineTranslateTarget);
  const [outlineOpen, setOutlineOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const [charCount, setCharCount] = useState(0);
  const [zoom, setZoom] = useState(1);
  const [docTranslateBusy, setDocTranslateBusy] = useState(false);
  const [layersOpen, setLayersOpen] = useState(false);
  const layers = useReaderTools((s) => s.layers);
  const setLayer = useReaderTools((s) => s.setLayer);
  useInkHistoryVersion(documentId);

  const undoNow = useCallback(() => {
    if (!client || !documentId) return;
    void undoInk(client, documentId)
      .then((entry) => {
        if (entry !== null) onToast?.(`已撤销:${entry.label}`);
      })
      .catch(() => onToast?.("撤销未完成,请重试"));
  }, [client, documentId, onToast]);
  const redoNow = useCallback(() => {
    if (!client || !documentId) return;
    void redoInk(client, documentId)
      .then((entry) => {
        if (entry !== null) onToast?.(`已重做:${entry.label}`);
      })
      .catch(() => onToast?.("重做未完成,请重试"));
  }, [client, documentId, onToast]);

  const translateWholeDocument = useCallback(async () => {
    if (!client || !documentId) return;
    setDocTranslateBusy(true);
    try {
      const glossary = await loadTranslationGlossary(client);
      const { taskId } = await submitWholeDocumentTranslation(
        client,
        documentId,
        useReaderTools.getState().inlineTranslateTarget,
        `整篇翻译 · ${documentTitle ?? "文档"}`,
        glossary,
      );
      onToast?.(`整篇翻译已提交(task ${taskId.slice(0, 8)}),完成后在资料中打开`);
    } catch (error) {
      const code =
        typeof error === "object" && error !== null && "appError" in error
          ? String((error as { appError: { code?: string } }).appError.code ?? "translation_failed")
          : "translation_failed";
      onToast?.(`整篇翻译未提交(${code}),请重试`);
    } finally {
      setDocTranslateBusy(false);
    }
  }, [client, documentId, documentTitle, onToast]);

  useEffect(() => {
    if (rootRef.current) {
      (rootRef.current as HTMLElement).style.zoom = String(zoom);
    }
  }, [rootRef, zoom]);

  useEffect(() => {
    const onOpenSearch = () => {
      if (!disabled) setSearchOpen(true);
    };
    window.addEventListener("knowwise-open-search", onOpenSearch);
    return () => window.removeEventListener("knowwise-open-search", onOpenSearch);
  }, [disabled]);

  useEffect(() => {
    const root = rootRef.current;
    if (root === null) return;
    const update = () => setCharCount((root.textContent ?? "").trim().length);
    update();
    const observer = new MutationObserver(update);
    observer.observe(root, { childList: true, subtree: true, characterData: true });
    return () => observer.disconnect();
  }, [rootRef, mdSource]);

  return (
    <>
      <ReaderSearch
        open={searchOpen && !disabled}
        onClose={() => setSearchOpen(false)}
        mode="text"
        rootRef={rootRef}
      />
      <div className="reader-toolbar" role="toolbar" aria-label="阅读器工具栏">
      <div className="reader-toolbar-group">
        <span className="reader-page-indicator">{charCount} 字</span>
      </div>
      <div className="reader-toolbar-group">
        <button
          type="button"
          className="icon-btn"
          title="撤销笔迹 (Ctrl+Z)"
          aria-label="撤销笔迹"
          disabled={disabled || !canUndoInk(documentId)}
          onClick={undoNow}
        >
          <Undo2 className="w-4 h-4" />
        </button>
        <button
          type="button"
          className="icon-btn"
          title="重做笔迹 (Ctrl+Shift+Z / Ctrl+Y)"
          aria-label="重做笔迹"
          disabled={disabled || !canRedoInk(documentId)}
          onClick={redoNow}
        >
          <Redo2 className="w-4 h-4" />
        </button>
      </div>
      {format === "docx" ? (
        <div className="reader-toolbar-group flex items-center gap-1">
          <button
            type="button"
            className="icon-btn"
            title="缩小"
            onClick={() => setZoom((z) => Math.max(0.6, Math.round((z - 0.1) * 10) / 10))}
          >
            <ZoomOut className="w-3.5 h-3.5" />
          </button>
          <span
            className="reader-zoom-label text-xs cursor-pointer select-none font-medium text-slate-600 dark:text-slate-300 min-w-[42px] text-center"
            title="点击还原为 100%"
            onClick={() => setZoom(1)}
          >
            {Math.round(zoom * 100)}%
          </span>
          <button
            type="button"
            className="icon-btn"
            title="放大"
            onClick={() => setZoom((z) => Math.min(2.5, Math.round((z + 0.1) * 10) / 10))}
          >
            <ZoomIn className="w-3.5 h-3.5" />
          </button>
        </div>
      ) : null}
      <div className="reader-toolbar-group">
        <button
          type="button"
          className={`icon-btn${tool === "pen" ? " active" : ""}`}
          title="画笔(在段落上手绘)"
          aria-label="画笔"
          aria-pressed={tool === "pen"}
          disabled={disabled}
          onClick={() => toggleTool("pen")}
        >
          <PenLine className="w-4 h-4" />
        </button>
        <button
          type="button"
          className={`icon-btn${tool === "highlight" ? " active" : ""}`}
          title="高亮笔(半透明粗线,可调粗细)"
          aria-label="高亮笔"
          aria-pressed={tool === "highlight"}
          disabled={disabled}
          onClick={() => toggleTool("highlight")}
        >
          <Highlighter className="w-4 h-4" />
        </button>
        {(tool === "pen" || tool === "highlight") ? (
          <span className="pen-colors" aria-label="画笔颜色">
            {PEN_COLORS.map((c) => (
              <button
                key={c}
                type="button"
                className={`color-dot${penColor === c ? " selected" : ""}`}
                style={{ backgroundColor: c }}
                aria-label={`画笔颜色 ${c}`}
                onClick={() => setPenColor(c)}
              />
            ))}
          </span>
        ) : null}
        {(tool === "pen" || tool === "highlight") ? (
          <label className="pen-width" title={tool === "highlight" ? "高亮笔粗细" : "画笔粗细"}>
            <span className="visually-hidden">
              {tool === "highlight" ? "高亮笔粗细" : "画笔粗细"}
            </span>
            <input
              type="range"
              min={2}
              max={10}
              step={1}
              value={richPenSlider(penWidth)}
              aria-label={tool === "highlight" ? "高亮笔粗细" : "画笔粗细"}
              onChange={(e) => setPenWidth(richSliderToWidth(Number(e.target.value)))}
            />
          </label>
        ) : null}
        <button
          type="button"
          className={`icon-btn${tool === "lasso" ? " active" : ""}`}
          title="套索:框选笔迹后拖动移动或删除 (Esc 取消)"
          aria-label="套索"
          aria-pressed={tool === "lasso"}
          disabled={disabled}
          onClick={() => toggleTool("lasso")}
        >
          <Lasso className="w-4 h-4" />
        </button>
        {([
          ["rect", Square, "矩形"],
          ["ellipse", Circle, "椭圆"],
          ["line", Minus, "直线"],
          ["arrow", MoveUpRight, "箭头"],
        ] as const).map(([shape, Icon, label]) => (
          <button
            key={shape}
            type="button"
            className={`icon-btn${tool === shape ? " active" : ""}`}
            title={`${label}:拖拽绘制`}
            aria-label={label}
            aria-pressed={tool === shape}
            disabled={disabled}
            onClick={() => toggleTool(shape)}
          >
            <Icon className="w-4 h-4" />
          </button>
        ))}
        <span className="layer-menu-wrap">
          <button
            type="button"
            className={`icon-btn${layersOpen ? " active" : ""}`}
            title="标注图层显隐"
            aria-label="标注图层"
            aria-expanded={layersOpen}
            aria-haspopup="menu"
            onClick={() => setLayersOpen((open) => !open)}
          >
            <Layers className="w-4 h-4" />
          </button>
          {layersOpen ? (
            <div className="layer-menu" role="menu" aria-label="标注图层">
              {(
                [
                  ["highlight", "高亮/下划线"],
                  ["ink", "笔迹与图形"],
                  ["textbox", "文字注释"],
                  ["note", "便签"],
                ] as const
              ).map(([key, label]) => (
                <label key={key} className="layer-menu-item">
                  <input
                    type="checkbox"
                    checked={layers[key]}
                    onChange={(event) => setLayer(key, event.target.checked)}
                  />
                  <span>{label}</span>
                </label>
              ))}
            </div>
          ) : null}
        </span>
        <button
          type="button"
          className={`icon-btn${tool === "textbox" ? " active" : ""}`}
          title="文字注释:在正文任意位置插入可编辑文本框"
          aria-label="文字注释"
          aria-pressed={tool === "textbox"}
          disabled={disabled}
          onClick={() => toggleTool("textbox")}
        >
          <Type className="w-4 h-4" />
        </button>
        <button
          type="button"
          className={`icon-btn${tool === "eraser" ? " active" : ""}`}
          title="橡皮(圆形范围,可调大小)"
          aria-label="橡皮"
          aria-pressed={tool === "eraser"}
          disabled={disabled}
          onClick={() => toggleTool("eraser")}
        >
          <Eraser className="w-4 h-4" />
        </button>
        {/* 始终占位:切换橡皮不改变工具栏高度,避免正文/笔迹在选工具时位移。 */}
        <label
          className={`pen-width${tool === "eraser" ? "" : " is-inactive"}`}
          title="橡皮大小"
          aria-hidden={tool !== "eraser"}
        >
          <span className="visually-hidden">橡皮大小</span>
          <input
            type="range"
            min={1}
            max={8}
            step={1}
            value={Math.round(eraserRadius * 100)}
            aria-label="橡皮大小"
            disabled={tool !== "eraser"}
            tabIndex={tool === "eraser" ? 0 : -1}
            onChange={(e) => setEraserRadius(Number(e.target.value) / 100)}
          />
        </label>
      </div>
      {format === "markdown" ? (
        <div className="reader-toolbar-group">
          <button
            type="button"
            className={`icon-btn${mdSource ? " active" : ""}`}
            title="源码模式(Typora 式切换)"
            aria-label="源码模式"
            aria-pressed={mdSource}
            onClick={toggleMdSource}
          >
            <FileCode2 className="w-4 h-4" />
          </button>
        </div>
      ) : null}
      <div className="reader-toolbar-group reader-translate-group" aria-label="翻译">
        <button
          type="button"
          className={`icon-btn${inlineTranslate ? " active" : ""}`}
          title={
            inlineTranslate
              ? "关闭原文对照译文"
              : "逐句翻译:在原文下方显示译文(本地引擎,不联网)"
          }
          aria-label="对照翻译"
          aria-pressed={inlineTranslate}
          disabled={disabled}
          onClick={() => setInlineTranslate(!inlineTranslate)}
        >
          <Languages className="w-4 h-4" />
        </button>
        <select
          className="reader-translate-target"
          aria-label="翻译目标语言"
          value={inlineTarget}
          disabled={disabled}
          onChange={(event) => setInlineTarget(event.target.value)}
        >
          {TRANSLATE_LANGUAGES.map((lang) => (
            <option key={lang.value} value={lang.value}>
              {lang.label}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="reader-translate-all"
          title="整篇翻译:生成译文产物,可在任务中心/资料查看"
          disabled={disabled || !client || !documentId || docTranslateBusy}
          onClick={() => void translateWholeDocument()}
        >
          {docTranslateBusy ? "提交中…" : "整篇"}
        </button>
      </div>
      <div className="reader-toolbar-group">
        <button
          type="button"
          className={`icon-btn${searchOpen ? " active" : ""}`}
          title="文档内搜索 (Ctrl+F)"
          aria-label="文档内搜索"
          aria-pressed={searchOpen}
          disabled={disabled}
          onClick={() => {
            setOutlineOpen(false);
            setSearchOpen((v) => !v);
          }}
        >
          <Search className="w-4 h-4" />
        </button>
      </div>
      <div className="reader-toolbar-group outline-group">
        <button
          type="button"
          className={`icon-btn${outlineOpen ? " active" : ""}`}
          title="大纲"
          aria-label="大纲"
          aria-pressed={outlineOpen}
          disabled={disabled}
          onClick={() => setOutlineOpen((v) => !v)}
        >
          <ListTree className="w-4 h-4" />
        </button>
        {outlineOpen ? (
          <TextOutlinePanel rootRef={rootRef} onClose={() => setOutlineOpen(false)} />
        ) : null}
      </div>
      <ReaderToolbarExtras />
      </div>
    </>
  );
}

type Heading = { level: number; title: string; el: HTMLElement };

/** 大纲弹层:挂载时现取标题(打开即刷新)。 */
function TextOutlinePanel({
  rootRef,
  onClose,
}: {
  rootRef: React.RefObject<HTMLElement | null>;
  onClose: () => void;
}): ReactNode {
  const headings = extractHeadings(rootRef.current);
  const [activeIndex, setActiveIndex] = useState(-1);
  useEffect(() => {
    const root = rootRef.current;
    const wrap = root?.closest(".reader-body-wrap") as HTMLElement | null;
    if (root === null || wrap === null) return;
    const compute = () => {
      const wrapTop = wrap.getBoundingClientRect().top;
      let next = -1;
      headings.forEach((heading, index) => {
        if (heading.el.getBoundingClientRect().top - wrapTop <= 56) next = index;
      });
      setActiveIndex(next);
    };
    compute();
    wrap.addEventListener("scroll", compute, { passive: true });
    return () => wrap.removeEventListener("scroll", compute);
  });
  return (
    <div className="outline-popover" role="listbox" aria-label="文档大纲">
      {headings.length === 0 ? (
        <p className="outline-empty">未发现标题结构。</p>
      ) : (
        headings.map((h, i) => (
          <button
            key={i}
            type="button"
            role="option"
            aria-current={activeIndex === i ? "location" : undefined}
            className={`outline-item level-${Math.min(h.level, 3)}${
              activeIndex === i ? " current" : ""
            }`}
            onClick={() => {
              h.el.scrollIntoView({ behavior: "smooth", block: "center" });
              h.el.classList.add("outline-flash");
              setTimeout(() => h.el.classList.remove("outline-flash"), 1200);
              onClose();
            }}
          >
            {h.title}
          </button>
        ))
      )}
    </div>
  );
}


function extractHeadings(root: HTMLElement | null): Heading[] {
  if (root === null) return [];
  const out: Heading[] = [];
  root.querySelectorAll("h1,h2,h3").forEach((el) => {
    const title = (el.textContent ?? "").trim();
    if (title.length > 0) out.push({ level: Number(el.tagName[1]), title, el: el as HTMLElement });
  });
  return out;
}

// —— 跳转监听(笔记/引用 → 正文滚动定位)——

export function useTextGotoListener(rootRef: React.RefObject<HTMLElement | null>): void {
  useEffect(() => {
    const onGoto = (event: Event) => {
      const detail = (
        event as CustomEvent<{
          start?: number;
          end?: number;
        }>
      ).detail;
      const root = rootRef.current;
      const start = detail?.start;
      const end = detail?.end;
      if (root === null || start === undefined || end === undefined) return;
      // 延迟到本文件加载的动态 import,避免循环依赖。
      void (async () => {
        const { rangeFromOffsets } = await import("./annotations");
        const range = rangeFromOffsets(root, start, end);
        if (range === null) return;
        const sel = window.getSelection();
        sel?.removeAllRanges();
        sel?.addRange(range);
        const node = range.startContainer.parentElement;
        node?.scrollIntoView({ behavior: "smooth", block: "center" });
      })();
    };
    window.addEventListener("knowwise-goto-text", onGoto);
    return () => window.removeEventListener("knowwise-goto-text", onGoto);
  }, [rootRef]);
}

// —— 段落锚定涂鸦层 ——

type PositionedInk = {
  annotation: AnnotationView;
  strokes: InkStroke[];
  top: number;
  left: number;
  width: number;
  height: number;
};

export function RichTextInkLayer({
  wrapRef,
  rootRef,
  client,
  documentId,
  versionId,
  format,
}: {
  wrapRef: React.RefObject<HTMLDivElement | null>;
  rootRef: React.RefObject<HTMLElement | null>;
  client?: CommandClient;
  documentId?: string;
  versionId?: string;
  format: "markdown" | "docx";
}): ReactNode {
  const tool = useReaderTools((s) => s.tool);
  const penColor = useReaderTools((s) => s.penColor);
  const penWidth = useReaderTools((s) => s.penWidth);
  const eraserRadius = useReaderTools((s) => s.eraserRadius);
  const setPenColor = useReaderTools((s) => s.setPenColor);
  const setPenWidth = useReaderTools((s) => s.setPenWidth);
  const setEraserRadius = useReaderTools((s) => s.setEraserRadius);
  const layers = useReaderTools((s) => s.layers);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const liveStrokeRef = useRef<InkStroke | null>(null);
  /** 画布后备尺寸缓存:只有尺寸变化才重设 width/height(赋值会清空位图)。 */
  const inkCanvasSizeRef = useRef<{ w: number; h: number }>({ w: 0, h: 0 });
  /** 帧内合并待绘制点,以及当前画笔样式。 */
  const inkRafRef = useRef(0);
  const inkPendingRef = useRef<Array<{ x: number; y: number }>>([]);
  const livePenRef = useRef<{ color: string; width: number } | null>(null);
  const liveBlockRef = useRef<HTMLElement | null>(null);
  const liveRangeRef = useRef<{ start: number; end: number } | null>(null);
  const shapeBlockRef = useRef<HTMLElement | null>(null);
  const shapeRangeRef = useRef<{ start: number; end: number } | null>(null);
  const shapeStartRef = useRef<InkPoint | null>(null);
  const shapeDraftRef = useRef<InkStroke | null>(null);
  const lassoDrawRef = useRef<InkPoint[] | null>(null);
  const lassoMoveStartRef = useRef<InkPoint | null>(null);
  const [textBoxDraft, setTextBoxDraft] = useState<TextBoxEditorState | null>(null);
  // 橡皮圆形光标:位置/直径都按像素记,屏幕上是正圆,与实际命中半径一致。
  const [eraserRing, setEraserRing] = useState<{ x: number; y: number; d: number } | null>(null);
  const [annotations, setAnnotations] = useState<AnnotationView[] | null>(null);
  const [inks, setInks] = useState<PositionedInk[]>([]);
  const [revision, setRevision] = useState(0);
  const [shapePreview, setShapePreview] = useState<{
    stroke: InkStroke;
    left: number;
    top: number;
    width: number;
    height: number;
  } | null>(null);
  const [shapeDraftPreview, setShapeDraftPreview] = useState<InkStroke | null>(null);
  const [lasso, setLasso] = useState<{
    polygon: InkPoint[];
    items: PositionedInk[];
    box: { left: number; top: number; width: number; height: number };
  } | null>(null);
  const [moveDelta, setMoveDelta] = useState<InkPoint | null>(null);
  const moveDeltaRef = useRef<InkPoint | null>(null);
  const [confirmRequest, setConfirmRequest] = useState<ConfirmRequest | null>(null);
  const [confirmBusy, setConfirmBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const noticeTimerRef = useRef(0);

  const showNotice = useCallback((text: string, ms = 2000) => {
    setNotice(text);
    if (noticeTimerRef.current !== 0) window.clearTimeout(noticeTimerRef.current);
    noticeTimerRef.current = window.setTimeout(() => {
      noticeTimerRef.current = 0;
      setNotice(null);
    }, ms);
  }, []);

  const annotationConflict = useAnnotationConflict(client, documentId, showNotice);

  const active =
    tool === "pen" ||
    tool === "highlight" ||
    tool === "eraser" ||
    tool === "textbox" ||
    tool === "lasso" ||
    tool === "rect" ||
    tool === "ellipse" ||
    tool === "line" ||
    tool === "arrow";

  // 标注列表(与其它视图共用失效事件)。
  useEffect(() => {
    if (!client || !documentId) return;
    let cancelled = false;
    const load = async () => {
      try {
        const list = await client.call<AnnotationView[]>("annotation_list", { documentId });
        if (!cancelled) setAnnotations(list);
      } catch {
        if (!cancelled) setAnnotations([]);
      }
    };
    void load();
    const onInvalidate = (event: Event) => {
      const detail = (event as CustomEvent<AnnotationChangeDetail | null | undefined>).detail;
      if (detail == null) {
        void load();
        setRevision((r) => r + 1);
        return;
      }
      // 增量广播:就地合并,避免每次落笔都重拉整份标注列表。
      if (detail.type === "created") {
        setAnnotations((previous) => [
          detail.annotation,
          ...(previous ?? []).filter((a) => a.id !== detail.annotation.id),
        ]);
      } else if (detail.type === "updated") {
        setAnnotations((previous) =>
          (previous ?? []).map((a) => (a.id === detail.annotation.id ? detail.annotation : a)),
        );
      } else {
        setAnnotations((previous) => (previous ?? []).filter((a) => a.id !== detail.annotationId));
      }
      setRevision((r) => r + 1);
    };
    window.addEventListener("knowwise-annotations-changed", onInvalidate);
    return () => {
      cancelled = true;
      window.removeEventListener("knowwise-annotations-changed", onInvalidate);
    };
  }, [client, documentId]);

  // 已存笔迹 → 段落盒定位(滚动/变更时重算)。
  const reposition = useCallback(() => {
    const wrap = wrapRef.current;
    const root = rootRef.current;
    if (wrap === null || root === null) {
      setInks([]);
      return;
    }
    void (async () => {
      const { rangeFromOffsets } = await import("./annotations");
      if (wrapRef.current === null || rootRef.current === null) return;
      const wrapRect = wrap.getBoundingClientRect();
      const next: PositionedInk[] = [];
      for (const a of annotations ?? []) {
        if (a.locator === null || a.locator.kind !== format) continue;
        const start =
          a.locator.kind === "markdown"
            ? a.locator.startOffset
            : a.locator.kind === "docx"
              ? a.locator.bodyBlockStart
              : -1;
        const end =
          a.locator.kind === "markdown"
            ? a.locator.endOffset
            : a.locator.kind === "docx"
              ? a.locator.bodyBlockEnd
              : -1;
        if (start < 0 || end <= start) continue;
        const strokes = decodeInk(a.noteText);
        if (strokes === null) continue;
        const range = rangeFromOffsets(rootRef.current!, start, end);
        if (range === null) continue;
        const node =
          range.startContainer instanceof Element
            ? range.startContainer
            : range.startContainer.parentElement;
        const block = node?.closest("p,h1,h2,h3,h4,h5,h6,li,blockquote,pre,td,th");
        if (!(block instanceof HTMLElement)) continue;
        const r = block.getBoundingClientRect();
        next.push({
          annotation: a,
          strokes,
          top: r.top - wrapRect.top + wrap.scrollTop,
          left: r.left - wrapRect.left + wrap.scrollLeft,
          width: r.width,
          height: r.height,
        });
      }
      setInks(next);
    })();
  }, [annotations, format, rootRef, wrapRef]);

  useEffect(() => {
    reposition();
  }, [reposition, revision]);

  useEffect(() => {
    const wrap = wrapRef.current;
    if (wrap === null) return;
    let raf = 0;
    const onScroll = () => {
      if (raf === 0) raf = requestAnimationFrame(reposition);
    };
    wrap.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", onScroll);
    return () => {
      wrap.removeEventListener("scroll", onScroll);
      window.removeEventListener("resize", onScroll);
      if (raf !== 0) cancelAnimationFrame(raf);
    };
  }, [reposition, wrapRef]);

  // 按文档记忆画笔/橡皮。
  useEffect(() => {
    const saved = loadDocumentPen(documentId);
    if (saved?.penColor !== undefined) setPenColor(saved.penColor);
    if (saved?.penWidth !== undefined) setPenWidth(saved.penWidth);
    if (saved?.eraserRadius !== undefined) setEraserRadius(saved.eraserRadius);
  }, [documentId, setEraserRadius, setPenColor, setPenWidth]);
  useEffect(() => {
    saveDocumentPen(documentId, { penColor, penWidth, eraserRadius });
  }, [documentId, penColor, penWidth, eraserRadius]);

  // 切文档清空撤销栈。
  useEffect(() => () => clearInkHistory(documentId), [documentId]);

  // 滚动后固定坐标的套索框会失效,直接取消选择。
  useEffect(() => {
    if (lasso === null) return;
    const wrap = wrapRef.current;
    if (wrap === null) return;
    const onScroll = () => setLasso(null);
    wrap.addEventListener("scroll", onScroll, { passive: true });
    return () => wrap.removeEventListener("scroll", onScroll);
  }, [lasso, wrapRef]);

  useEffect(() => {
    if (lasso === null) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setLasso(null);
      setMoveDelta(null);
      moveDeltaRef.current = null;
      useReaderTools.getState().setTool("read");
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [lasso]);

  useInkHistoryHotkeys({
    documentId,
    containerRef: wrapRef,
    client,
    onApplied: (entry, direction) =>
      showNotice(`${direction === "undo" ? "已撤销" : "已重做"}:${entry.label}`),
    onError: showNotice,
  });

  // 最新笔迹/标注引用:橡皮队列必须使用最新 revision。
  const inksRef = useRef(inks);
  inksRef.current = inks;
  const annotationsRef = useRef<AnnotationView[] | null>(annotations);
  annotationsRef.current = annotations;
  const eraseAtRef = useRef<(x: number, y: number, radius: number) => Promise<void>>(
    async () => undefined,
  );
  const eraseJobRef = useRef<{ x: number; y: number; radius: number } | null>(null);
  const eraseRunningRef = useRef(false);
  const eraseGestureRef = useRef<InkGestureRecorder | null>(null);
  const eraseEndRef = useRef<() => void>(() => undefined);
  const beginErase = useCallback(() => {
    const previous = eraseGestureRef.current;
    if (previous !== null && previous.commit(documentId ?? "") > 0) {
      window.dispatchEvent(new CustomEvent("knowwise-ink-history-changed"));
    }
    eraseGestureRef.current = new InkGestureRecorder("橡皮擦除");
  }, [documentId]);
  const endErase = useCallback(() => {
    const settle = () => {
      if (eraseRunningRef.current || eraseJobRef.current !== null) {
        window.setTimeout(settle, 60);
        return;
      }
      const recorder = eraseGestureRef.current;
      eraseGestureRef.current = null;
      if (recorder !== null && documentId !== undefined && recorder.commit(documentId) > 0) {
        window.dispatchEvent(new CustomEvent("knowwise-ink-history-changed"));
      }
    };
    settle();
  }, [documentId]);
  eraseEndRef.current = endErase;

  if (!active && inks.length === 0) return null;

  const commitStroke = async (
    stroke: InkStroke,
    rangeInfo: { start: number; end: number } | null,
  ) => {
    if (!client || !documentId || !versionId) return;
    if (stroke.points.length === 1) stroke.points.push({ ...stroke.points[0]! });
    const range = rangeInfo ?? { start: 0, end: 0 };
    const locator: SourceLocator =
      format === "markdown"
        ? {
            kind: "markdown",
            startOffset: range.start,
            endOffset: range.end,
            mdastNodeType: "text",
            headingPath: [],
            locatorVersion: 1,
          }
        : {
            kind: "docx",
            bodyBlockStart: range.start,
            bodyBlockEnd: range.end,
            paragraphIds: [],
            headingPath: [],
            locatorVersion: 1,
          };
    const text = `手绘标注(${range.start}-${range.end})`;
    const rawSelection: RawSelectionInput = {
      documentId,
      documentVersionId: versionId,
      format,
      text,
      blocks: [{ renderKey: `ink-${Date.now()}`, text, blockOrder: 0, locator }],
      primaryLocator: locator,
      selectionKind: "text",
    };
    try {
      const anchor = await client.call<AnchorView>("evidence_create_anchor", {
        rawSelection,
        idempotencyKey: client.newIdempotencyKey(),
      });
      const encoded = encodeInk([stroke]);
      const created = await client.call<AnnotationView>("annotation_create", {
        documentId,
        anchorId: anchor.id,
        color: "pink",
        noteText: encoded,
        idempotencyKey: client.newIdempotencyKey(),
      });
      pushInkHistory(documentId, "新建笔迹", [
        {
          anchorId: anchor.id,
          before: null,
          after: { anchorId: anchor.id, color: "pink", noteText: encoded },
        },
      ]);
      broadcastAnnotationChange({ type: "created", annotation: created });
    } catch {
      showNotice("笔迹未保存,请重试");
    }
  };

  // 橡皮:只擦圆内的点并拆分笔迹,不再"点一下删整条标注"。
  const eraseOnce = async (clientX: number, clientY: number, radius: number) => {
    const wrap = wrapRef.current;
    if (wrap === null || !client || !layers.ink) return;
    const wrapRect = wrap.getBoundingClientRect();
    // 屏幕上正圆:命中半径按容器宽度换算成像素,距离也按像素算。
    const hitPx = Math.max(3, radius * wrapRect.width);
    for (const ink of inksRef.current) {
      const originX = wrapRect.left + ink.left - wrap.scrollLeft;
      const originY = wrapRect.top + ink.top - wrap.scrollTop;
      const hit = (p: { x: number; y: number }) =>
        Math.hypot(
          clientX - (originX + p.x * ink.width),
          clientY - (originY + p.y * ink.height),
        ) <= hitPx;
      const next = eraseInkCircle(ink.strokes, hit);
      if (next === null) continue;
      const current =
        annotationsRef.current?.find((item) => item.id === ink.annotation.id) ?? ink.annotation;
      const before = {
        anchorId: current.anchorId,
        color: current.color,
        noteText: current.noteText,
      };
      try {
        if (next.length === 0) {
          await client.call("annotation_delete", {
            annotationId: current.id,
            expectedRevision: current.revision,
          });
          annotationsRef.current = (annotationsRef.current ?? []).filter(
            (item) => item.id !== current.id,
          );
          setAnnotations(annotationsRef.current);
          inksRef.current = inksRef.current.filter((item) => item.annotation.id !== current.id);
          broadcastAnnotationChange({ type: "deleted", annotationId: current.id });
          eraseGestureRef.current?.record({ anchorId: current.anchorId, before, after: null });
        } else {
          const encoded = encodeInk(next);
          const updated = await client.call<AnnotationView>("annotation_update", {
            annotationId: current.id,
            color: current.color,
            noteText: encoded,
            expectedRevision: current.revision,
          });
          annotationsRef.current = (annotationsRef.current ?? []).map((item) =>
            item.id === updated.id ? updated : item,
          );
          setAnnotations(annotationsRef.current);
          inksRef.current = inksRef.current.map((item) =>
            item.annotation.id === updated.id
              ? { ...item, annotation: updated, strokes: next }
              : item,
          );
          broadcastAnnotationChange({ type: "updated", annotation: updated });
          eraseGestureRef.current?.record({
            anchorId: current.anchorId,
            before,
            after: { anchorId: current.anchorId, color: current.color, noteText: encoded },
          });
        }
        setRevision((r) => r + 1);
      } catch (error) {
        if (isRevisionConflict(error)) {
          annotationConflict.open(
            {
              anchorId: current.anchorId,
              annotationId: current.id,
              color: current.color,
              noteText: next.length === 0 ? null : encodeInk(next),
            },
            "擦除时该标注已被其他操作更新(修订冲突)。请选择如何处理。",
          );
        } else {
          showNotice("擦除未保存,请重试");
        }
      }
    }
  };
  eraseAtRef.current = eraseOnce;
  const eraseAt = (clientX: number, clientY: number, radius = 0.02) => {
    eraseJobRef.current = { x: clientX, y: clientY, radius };
    if (eraseRunningRef.current) return;
    eraseRunningRef.current = true;
    void (async () => {
      try {
        while (eraseJobRef.current !== null) {
          const job = eraseJobRef.current;
          eraseJobRef.current = null;
          await eraseAtRef.current(job.x, job.y, job.radius);
        }
      } finally {
        eraseRunningRef.current = false;
      }
    })();
  };

  /** 添加文字注释:点正文任意位置 → 打开编辑器(保存时才写库)。 */
  const addTextBoxAt = (clientX: number, clientY: number) => {
    const wrap = wrapRef.current;
    const root = rootRef.current;
    if (wrap === null || root === null || !documentId || !versionId) return;
    const wrapRect = wrap.getBoundingClientRect();
    const scroll = root.closest(".reader-body-wrap") as HTMLElement | null;
    const scrollTop = scroll?.scrollTop ?? 0;
    const nx = (clientX - wrapRect.left) / Math.max(1, wrapRect.width);
    const ny = (clientY - wrapRect.top + scrollTop) / Math.max(1, wrapRect.height + scrollTop);
    const block = nearestInkBlock(root, clientX, clientY, canvasRef.current) ?? root.firstElementChild;
    if (!(block instanceof HTMLElement)) return;
    const rangeInfo = inkBlockRange(root, block);
    const locator: SourceLocator =
      format === "markdown"
        ? {
            kind: "markdown",
            startOffset: rangeInfo?.start ?? 0,
            endOffset: rangeInfo?.end ?? 0,
            mdastNodeType: "text",
            headingPath: [],
            locatorVersion: 1,
          }
        : {
            kind: "docx",
            bodyBlockStart: rangeInfo?.start ?? 0,
            bodyBlockEnd: rangeInfo?.end ?? 0,
            paragraphIds: [],
            headingPath: [],
            locatorVersion: 1,
          };
    setTextBoxDraft({
      annotation: null,
      target: {
        x: Math.min(1, Math.max(0, nx)),
        y: Math.max(0, ny),
        anchorText: block.textContent?.slice(0, 400) ?? "",
        locator,
      },
      text: "",
      style: { ...DEFAULT_TEXTBOX_STYLE },
      screenX: Math.min(clientX, window.innerWidth - 340),
      screenY: Math.min(clientY, window.innerHeight - 220),
    });
    useReaderTools.getState().setTool("read");
  };

  const reload = () => {
    window.dispatchEvent(new CustomEvent("knowwise-annotations-changed"));
  };

  /** 一条笔迹在视口中的包围盒(套索命中/选择框)。 */
  const inkClientSamplePoints = (ink: PositionedInk, wrapRect: DOMRect): InkPoint[] => {
    const originX = wrapRect.left + ink.left - (wrapRef.current?.scrollLeft ?? 0);
    const originY = wrapRect.top + ink.top - (wrapRef.current?.scrollTop ?? 0);
    return ink.strokes.flatMap((stroke) =>
      strokeSamplePoints(stroke).map((p) => ({
        x: originX + p.x * ink.width,
        y: originY + p.y * ink.height,
      })),
    );
  };

  const selectWithLasso = (polygon: InkPoint[]) => {
    const wrap = wrapRef.current;
    if (wrap === null || !layers.ink) {
      setLasso(null);
      return;
    }
    const wrapRect = wrap.getBoundingClientRect();
    const selected = inksRef.current.filter((ink) =>
      ink.strokes.some((stroke) =>
        strokeSamplePoints(stroke).some((p) => {
          const client = {
            x: wrapRect.left + ink.left - wrap.scrollLeft + p.x * ink.width,
            y: wrapRect.top + ink.top - wrap.scrollTop + p.y * ink.height,
          };
          return pointInPolygon(client, polygon);
        }),
      ),
    );
    if (selected.length === 0) {
      setLasso(null);
      return;
    }
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const ink of selected) {
      for (const p of inkClientSamplePoints(ink, wrapRect)) {
        minX = Math.min(minX, p.x);
        minY = Math.min(minY, p.y);
        maxX = Math.max(maxX, p.x);
        maxY = Math.max(maxY, p.y);
      }
    }
    setLasso({
      polygon,
      items: selected,
      box: {
        left: minX - 8,
        top: minY - 8,
        width: maxX - minX + 16,
        height: maxY - minY + 16,
      },
    });
    setMoveDelta(null);
    moveDeltaRef.current = null;
  };

  const applyInkUpdate = (updated: AnnotationView, strokes: InkStroke[]) => {
    annotationsRef.current = (annotationsRef.current ?? []).map((item) =>
      item.id === updated.id ? updated : item,
    );
    setAnnotations(annotationsRef.current);
    inksRef.current = inksRef.current.map((item) =>
      item.annotation.id === updated.id
        ? { ...item, annotation: updated, strokes }
        : item,
    );
    setInks(inksRef.current);
    broadcastAnnotationChange({ type: "updated", annotation: updated });
  };

  const commitMove = async (delta: InkPoint) => {
    const current = lasso;
    setLasso(null);
    setMoveDelta(null);
    moveDeltaRef.current = null;
    if (current === null || !client || !documentId) return;
    if (Math.abs(delta.x) < 1 && Math.abs(delta.y) < 1) return;
    const changes = [];
    for (const ink of current.items) {
      const latest = annotationsRef.current?.find(
        (item) => item.id === ink.annotation.id,
      ) ?? ink.annotation;
      const next = translateStrokes(ink.strokes, delta.x / Math.max(1, ink.width), delta.y / Math.max(1, ink.height));
      const encoded = encodeInk(next);
      if (encoded === latest.noteText) continue;
      try {
        const updated = await client.call<AnnotationView>("annotation_update", {
          annotationId: latest.id,
          color: latest.color,
          noteText: encoded,
          expectedRevision: latest.revision,
        });
        applyInkUpdate(updated, next);
        changes.push({
          anchorId: latest.anchorId,
          before: { anchorId: latest.anchorId, color: latest.color, noteText: latest.noteText },
          after: { anchorId: latest.anchorId, color: latest.color, noteText: encoded },
        });
      } catch (error) {
        if (isRevisionConflict(error)) {
          annotationConflict.open(
            {
              anchorId: latest.anchorId,
              annotationId: latest.id,
              color: latest.color,
              noteText: encoded,
            },
            "移动时该标注已被其他操作更新(修订冲突)。请选择如何处理。",
          );
        } else {
          showNotice("移动未保存,请重试");
        }
      }
    }
    if (changes.length > 0) {
      pushInkHistory(documentId, "移动笔迹", changes);
      window.dispatchEvent(new CustomEvent("knowwise-ink-history-changed"));
      showNotice(`已移动 ${changes.length} 条笔迹`);
      setRevision((r) => r + 1);
    }
  };

  const deleteInkItems = async (items: PositionedInk[]) => {
    if (!client || !documentId) return;
    const changes = [];
    for (const ink of items) {
      const latest = annotationsRef.current?.find(
        (item) => item.id === ink.annotation.id,
      ) ?? ink.annotation;
      try {
        await client.call("annotation_delete", {
          annotationId: latest.id,
          expectedRevision: latest.revision,
        });
        annotationsRef.current = (annotationsRef.current ?? []).filter(
          (item) => item.id !== latest.id,
        );
        setAnnotations(annotationsRef.current);
        inksRef.current = inksRef.current.filter((item) => item.annotation.id !== latest.id);
        setInks(inksRef.current);
        broadcastAnnotationChange({ type: "deleted", annotationId: latest.id });
        changes.push({
          anchorId: latest.anchorId,
          before: { anchorId: latest.anchorId, color: latest.color, noteText: latest.noteText },
          after: null,
        });
      } catch (error) {
        if (isRevisionConflict(error)) {
          annotationConflict.open(
            {
              anchorId: latest.anchorId,
              annotationId: latest.id,
              color: latest.color,
              noteText: null,
            },
            "删除时该标注已被其他操作更新(修订冲突)。请选择如何处理。",
          );
        } else {
          showNotice("删除笔迹未完成,请重试");
        }
      }
    }
    if (changes.length > 0) {
      pushInkHistory(documentId, "删除笔迹", changes);
      window.dispatchEvent(new CustomEvent("knowwise-ink-history-changed"));
      showNotice(`已删除 ${changes.length} 条笔迹,可用 Ctrl+Z 撤销`);
      setRevision((r) => r + 1);
    }
  };

  const requestDeleteInk = (items: PositionedInk[]) => {
    setConfirmRequest({
      title: "删除笔迹",
      message: `确定删除选中的 ${items.length} 条笔迹/图形吗?删除后可用 Ctrl+Z 撤销。`,
      confirmLabel: "删除",
      onConfirm: async () => {
        setConfirmBusy(true);
        try {
          await deleteInkItems(items);
          setLasso(null);
        } finally {
          setConfirmBusy(false);
          setConfirmRequest(null);
        }
      },
    });
  };

  const pointerHandlers = active
    ? {
        onPointerDown: (event: React.PointerEvent<HTMLCanvasElement>) => {
          const canvas = canvasRef.current;
          const root = rootRef.current;
          if (canvas === null || root === null) return;
          const wrap = wrapRef.current;
          const wrapRect = wrap?.getBoundingClientRect();
          if (wrap === null || wrapRect === undefined) return;
          if (tool === "eraser") {
            setEraserRing({
              x: event.clientX - wrapRect.left + wrap.scrollLeft,
              y: event.clientY - wrapRect.top + wrap.scrollTop,
              d: Math.max(6, eraserRadius * 2 * wrapRect.width),
            });
            beginErase();
            void eraseAt(event.clientX, event.clientY, eraserRadius);
            return;
          }
          if (tool === "textbox") {
            addTextBoxAt(event.clientX, event.clientY);
            return;
          }
          if (tool === "lasso") {
            canvas.setPointerCapture?.(event.pointerId);
            if (
              lasso !== null &&
              event.clientX >= lasso.box.left &&
              event.clientX <= lasso.box.left + lasso.box.width &&
              event.clientY >= lasso.box.top &&
              event.clientY <= lasso.box.top + lasso.box.height
            ) {
              lassoMoveStartRef.current = { x: event.clientX, y: event.clientY };
              setMoveDelta({ x: 0, y: 0 });
              return;
            }
            lassoDrawRef.current = [{ x: event.clientX, y: event.clientY }];
            setMoveDelta(null);
            setLasso(null);
            return;
          }
          if (
            tool === "rect" ||
            tool === "ellipse" ||
            tool === "line" ||
            tool === "arrow"
          ) {
            const block =
              nearestInkBlock(root, event.clientX, event.clientY, canvas) ??
              (root.firstElementChild instanceof HTMLElement ? root.firstElementChild : root);
            const rangeInfo = inkBlockRange(root, block);
            const rect = block.getBoundingClientRect();
            const start = {
              x: (event.clientX - rect.left) / Math.max(1, rect.width),
              y: (event.clientY - rect.top) / Math.max(1, rect.height),
            };
            shapeBlockRef.current = block;
            shapeRangeRef.current = rangeInfo ?? { start: 0, end: 0 };
            shapeStartRef.current = start;
            const draft = makeShapeStroke(tool, start, start, penColor, penWidth);
            shapeDraftRef.current = draft;
            setShapePreview({
              stroke: draft,
              left: rect.left - wrapRect.left + wrap.scrollLeft,
              top: rect.top - wrapRect.top + wrap.scrollTop,
              width: rect.width,
              height: rect.height,
            });
            setShapeDraftPreview(draft);
            canvas.setPointerCapture?.(event.pointerId);
            return;
          }
          // 点到哪就在哪画:锚点块只用于落库定位,拿不到时退到最近的
          // 可锚定祖先/根节点,而不是直接拒绝(这正是"部分文档涂鸦不可用"
          // 的原因——列表间隙、代码块外层等命不中 p/li)。
          const block =
            nearestInkBlock(root, event.clientX, event.clientY, canvas) ??
            (root.firstElementChild instanceof HTMLElement ? root.firstElementChild : root);
          const rangeInfo = inkBlockRange(root, block);
          canvas.setPointerCapture?.(event.pointerId);
          const rect = block.getBoundingClientRect();
          const ctx = canvas.getContext("2d");
          if (ctx === null) return;
          // 画布缓冲必须与显示尺寸一致:canvas 默认 300×150,而正文区
          // 往往更宽。CSS 把显示撑满、位图却仍是默认尺寸时,落笔坐标会被
          // 压进左上角一小块,看起来就是"画了没反应"(PDF 视图同源问题)。
          const dprInk = window.devicePixelRatio || 1;
          const cw = Math.max(1, Math.round(wrapRect.width * dprInk));
          const ch = Math.max(1, Math.round(wrapRect.height * dprInk));
          if (
            inkCanvasSizeRef.current.w !== cw ||
            inkCanvasSizeRef.current.h !== ch ||
            canvas.width !== cw ||
            canvas.height !== ch
          ) {
            canvas.width = cw;
            canvas.height = ch;
            inkCanvasSizeRef.current = { w: cw, h: ch };
          } else {
            ctx.clearRect(0, 0, canvas.width, canvas.height);
          }
          ctx.setTransform(dprInk, 0, 0, dprInk, 0, 0);
          const x = (event.clientX - rect.left) / Math.max(1, rect.width);
          const y = (event.clientY - rect.top) / Math.max(1, rect.height);
          const isHighlight = tool === "highlight";
          const ratio = isHighlight ? Math.max(0.012, penWidth * 3) : penWidth;
          const strokeWidth = Math.max(1.5, rect.width * ratio);
          const strokeColor = isHighlight ? richWithAlpha(penColor, 0.35) : penColor;
          livePenRef.current = { color: strokeColor, width: strokeWidth };
          inkPendingRef.current = [
            {
              x: rect.left - wrapRect.left + x * rect.width,
              y: rect.top - wrapRect.top + y * rect.height,
            },
          ];
          ctx.lineCap = "round";
          ctx.lineJoin = "round";
          ctx.strokeStyle = strokeColor;
          ctx.lineWidth = strokeWidth;
          liveStrokeRef.current = { color: strokeColor, width: ratio, points: [{ x, y }] };
          liveBlockRef.current = block;
          liveRangeRef.current = rangeInfo ?? { start: 0, end: 0 };
          // 点击即出点(即时反馈)。
          ctx.beginPath();
          ctx.arc(
            rect.left - wrapRect.left + x * rect.width,
            rect.top - wrapRect.top + y * rect.height,
            Math.max(1.2, rect.width * 0.0018),
            0,
            Math.PI * 2,
          );
          ctx.fillStyle = strokeColor;
          ctx.fill();
        },
        onPointerMove: (event: React.PointerEvent<HTMLCanvasElement>) => {
          const wrap = wrapRef.current;
          if (wrap === null) return;
          const wrapRect = wrap.getBoundingClientRect();
          if (tool === "eraser") {
            setEraserRing({
              x: event.clientX - wrapRect.left + wrap.scrollLeft,
              y: event.clientY - wrapRect.top + wrap.scrollTop,
              d: Math.max(6, eraserRadius * 2 * wrapRect.width),
            });
            if (!(event.buttons & 1)) return;
            void eraseAt(event.clientX, event.clientY, eraserRadius);
            return;
          }
          if (tool === "lasso") {
            if (lassoMoveStartRef.current !== null) {
              if (!(event.buttons & 1)) return;
              const delta = {
                x: event.clientX - lassoMoveStartRef.current.x,
                y: event.clientY - lassoMoveStartRef.current.y,
              };
              moveDeltaRef.current = delta;
              setMoveDelta(delta);
              return;
            }
            const drawing = lassoDrawRef.current;
            if (drawing === null || !(event.buttons & 1)) return;
            const last = drawing[drawing.length - 1]!;
            if (Math.hypot(event.clientX - last.x, event.clientY - last.y) >= 3) {
              drawing.push({ x: event.clientX, y: event.clientY });
              setLasso({
                polygon: [...drawing],
                items: [],
                box: { left: 0, top: 0, width: 0, height: 0 },
              });
            }
            return;
          }
          if (shapeStartRef.current !== null) {
            if (!(event.buttons & 1)) return;
            const block = shapeBlockRef.current;
            if (block === null) return;
            const rect = block.getBoundingClientRect();
            const draft = makeShapeStroke(
              tool as "rect" | "ellipse" | "line" | "arrow",
              shapeStartRef.current,
              {
                x: Math.min(1, Math.max(0, (event.clientX - rect.left) / Math.max(1, rect.width))),
                y: Math.min(1, Math.max(0, (event.clientY - rect.top) / Math.max(1, rect.height))),
              },
              penColor,
              penWidth,
            );
            shapeDraftRef.current = draft;
            setShapeDraftPreview(draft);
            return;
          }
          const stroke = liveStrokeRef.current;
          const block = liveBlockRef.current;
          const canvas = canvasRef.current;
          if (stroke === null || block === null || canvas === null) return;
          if (!(event.buttons & 1)) return;
          const rect = block.getBoundingClientRect();
          const points =
            typeof event.nativeEvent.getCoalescedEvents === "function"
              ? event.nativeEvent.getCoalescedEvents()
              : [event.nativeEvent];
          for (const raw of points.length > 0 ? points : [event.nativeEvent]) {
            const x = (raw.clientX - rect.left) / Math.max(1, rect.width);
            const y = (raw.clientY - rect.top) / Math.max(1, rect.height);
            stroke.points.push({ x, y });
            inkPendingRef.current.push({
              x: rect.left - wrapRect.left + x * rect.width,
              y: rect.top - wrapRect.top + y * rect.height,
            });
          }
          // 合并到下一帧:高频指针对只做一次 stroke。
          if (inkRafRef.current === 0) {
            inkRafRef.current = requestAnimationFrame(() => {
              inkRafRef.current = 0;
              const target = canvasRef.current;
              const pen = livePenRef.current;
              if (target === null || pen === null) return;
              const c = target.getContext("2d");
              if (c === null) return;
              c.lineCap = "round";
              c.lineJoin = "round";
              c.strokeStyle = pen.color;
              c.lineWidth = pen.width;
              c.beginPath();
              const pts = inkPendingRef.current;
              if (pts.length > 0) {
                c.moveTo(pts[0]!.x, pts[0]!.y);
                for (let i = 1; i < pts.length; i += 1) c.lineTo(pts[i]!.x, pts[i]!.y);
                c.stroke();
                inkPendingRef.current = [pts[pts.length - 1]!];
              }
            });
          }
        },
        onPointerUp: () => {
          if (tool === "eraser") {
            endErase();
            return;
          }
          if (tool === "lasso") {
            if (lassoMoveStartRef.current !== null) {
              const delta = moveDeltaRef.current ?? { x: 0, y: 0 };
              lassoMoveStartRef.current = null;
              void commitMove(delta);
              return;
            }
            const drawing = lassoDrawRef.current;
            lassoDrawRef.current = null;
            if (drawing === null || drawing.length < 3) {
              setLasso(null);
              return;
            }
            selectWithLasso(drawing);
            return;
          }
          if (shapeStartRef.current !== null) {
            const draft = shapeDraftRef.current;
            const block = shapeBlockRef.current;
            const rangeInfo = shapeRangeRef.current;
            shapeStartRef.current = null;
            shapeDraftRef.current = null;
            shapeBlockRef.current = null;
            shapeRangeRef.current = null;
            setShapePreview(null);
            setShapeDraftPreview(null);
            if (draft === null || block === null) return;
            const a = draft.points[0]!;
            const b = draft.points[draft.points.length - 1]!;
            if (Math.hypot(a.x - b.x, a.y - b.y) < 0.004) return;
            void commitStroke(draft, rangeInfo);
            return;
          }
          const stroke = liveStrokeRef.current;
          const block = liveBlockRef.current;
          const rangeInfo = liveRangeRef.current;
          liveStrokeRef.current = null;
          liveBlockRef.current = null;
          liveRangeRef.current = null;
          if (inkRafRef.current !== 0) {
            cancelAnimationFrame(inkRafRef.current);
            inkRafRef.current = 0;
          }
          inkPendingRef.current = [];
          livePenRef.current = null;
          const canvas = canvasRef.current;
          if (canvas !== null) {
            canvas.getContext("2d")?.clearRect(0, 0, canvas.width, canvas.height);
          }
          if (stroke === null || block === null) return;
          void commitStroke(stroke, rangeInfo);
        },
        onPointerCancel: () => {
          if (tool === "eraser") endErase();
          if (tool === "lasso") {
            lassoMoveStartRef.current = null;
            lassoDrawRef.current = null;
            setLasso(null);
            setMoveDelta(null);
          }
          shapeStartRef.current = null;
          shapeDraftRef.current = null;
          shapeBlockRef.current = null;
          setShapePreview(null);
          setShapeDraftPreview(null);
        },
      }
    : {};

  const selectionBox =
    lasso === null
      ? null
      : {
          ...lasso.box,
          left: lasso.box.left + (moveDelta?.x ?? 0),
          top: lasso.box.top + (moveDelta?.y ?? 0),
        };

  return (
    <>
      {layers.ink
        ? inks.map((ink) => {
            const selected = lasso?.items.some(
              (item) => item.annotation.id === ink.annotation.id,
            );
            return (
              <svg
                key={ink.annotation.id}
                className={`rich-ink${active ? " erasing" : ""}${
                  selected ? " lasso-selected" : ""
                }`}
                style={{
                  top: `${ink.top}px`,
                  left: `${ink.left}px`,
                  width: `${ink.width}px`,
                  height: `${ink.height}px`,
                  transform:
                    selected && moveDelta !== null
                      ? `translate(${moveDelta.x}px, ${moveDelta.y}px)`
                      : undefined,
                }}
                viewBox={`0 0 ${Math.max(1, ink.width)} ${Math.max(1, ink.height)}`}
                role="button"
                tabIndex={active ? -1 : 0}
                aria-label="手绘笔迹,按 Delete 删除"
                onKeyDown={(event) => {
                  if (event.key === "Delete" || event.key === "Backspace") {
                    event.preventDefault();
                    requestDeleteInk([ink]);
                  }
                }}
              >
                {inkStrokeElements(
                  ink.strokes,
                  Math.max(1, ink.width),
                  Math.max(1, ink.height),
                )}
              </svg>
            );
          })
        : null}
      <canvas
        ref={canvasRef}
        className={`rich-ink-canvas${active ? " active" : ""}${
          tool === "eraser" ? " tool-eraser" : ""
        }${tool === "lasso" ? " tool-lasso" : ""}`}
        {...pointerHandlers}
        onPointerLeave={() => setEraserRing(null)}
      />
      {shapePreview !== null && shapeDraftPreview !== null ? (
        <svg
          className="shape-preview"
          style={{
            position: "absolute",
            left: `${shapePreview.left}px`,
            top: `${shapePreview.top}px`,
            width: `${shapePreview.width}px`,
            height: `${shapePreview.height}px`,
            pointerEvents: "none",
          }}
          viewBox={`0 0 ${Math.max(1, shapePreview.width)} ${Math.max(1, shapePreview.height)}`}
          aria-hidden="true"
        >
          {inkStrokeElements(
            [shapeDraftPreview],
            Math.max(1, shapePreview.width),
            Math.max(1, shapePreview.height),
          )}
        </svg>
      ) : null}
      {tool === "eraser" && eraserRing !== null ? (
        <span
          className="eraser-ring"
          aria-hidden="true"
          style={{
            left: `${eraserRing.x}px`,
            top: `${eraserRing.y}px`,
            width: `${eraserRing.d}px`,
            height: `${eraserRing.d}px`,
          }}
        />
      ) : null}
      {lasso !== null && lasso.polygon.length > 1 ? (
        <svg className="rt-lasso-overlay" aria-hidden="true">
          <polygon
            points={lasso.polygon.map((p) => `${p.x},${p.y}`).join(" ")}
            fill="rgba(63, 131, 248, 0.08)"
            stroke="#3f83f8"
            strokeWidth={1.5}
            strokeDasharray="5 4"
          />
        </svg>
      ) : null}
      {selectionBox !== null && lasso !== null && lasso.items.length > 0 ? (
        <>
          <div
            className="lasso-box"
            style={{
              position: "fixed",
              left: `${selectionBox.left}px`,
              top: `${selectionBox.top}px`,
              width: `${selectionBox.width}px`,
              height: `${selectionBox.height}px`,
            }}
            role="group"
            aria-label="套索选区"
          />
          <div
            className="lasso-actions"
            role="toolbar"
            aria-label="套索操作"
            style={{
              position: "fixed",
              left: `${Math.max(6, Math.min(window.innerWidth - 260, selectionBox.left))}px`,
              top: `${Math.max(6, selectionBox.top - 40)}px`,
            }}
            onPointerDown={(event) => event.stopPropagation()}
          >
            <span className="lasso-hint">{lasso.items.length} 条 · 拖动选区可移动</span>
            <button
              type="button"
              className="btn tiny danger"
              onClick={() => requestDeleteInk(lasso.items)}
            >
              删除
            </button>
            <button
              type="button"
              className="btn tiny ghost"
              onClick={() => {
                setLasso(null);
                setMoveDelta(null);
              }}
            >
              取消
            </button>
          </div>
        </>
      ) : null}
      {(annotations ?? []).map((annotation) => {
        if (!layers.textbox) return null;
        const payload = decodeTextBox(annotation.noteText);
        if (payload === null) return null;
        return (
          <TextBoxBubble
            key={annotation.id}
            annotation={annotation}
            styleLeft={`${payload.x * 100}%`}
            styleTop={`${payload.y * 100}%`}
            styleWidth={`${payload.width * 100}%`}
            onEdit={() =>
              setTextBoxDraft({
                annotation,
                target: null,
                text: payload.text,
                style: payload.style,
                screenX: window.innerWidth / 2 - 170,
                screenY: window.innerHeight / 2 - 110,
              })
            }
          />
        );
      })}
      {textBoxDraft !== null ? (
        <TextBoxEditor
          state={textBoxDraft}
          client={client}
          documentId={documentId}
          versionId={versionId}
          format={format}
          onClose={() => setTextBoxDraft(null)}
          onSaved={reload}
          onDeleted={() => {
            setTextBoxDraft(null);
            reload();
          }}
          onToast={showNotice}
        />
      ) : null}
      <ConfirmDialog
        request={confirmRequest}
        busy={confirmBusy}
        onCancel={() => {
          if (!confirmBusy) setConfirmRequest(null);
        }}
      />
      {annotationConflict.dialog}
      {notice !== null ? (
        <div className="reader-toast" role="status">
          {notice}
        </div>
      ) : null}
    </>
  );
}
