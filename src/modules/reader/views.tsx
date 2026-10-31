// M03 拥有:正文渲染视图(Markdown, DOCX, 与 PDF 真实 Canvas+TextLayer 渲染)。
// 交互层:
// - 共享工具状态(readerTools):画笔/橡皮/便签、Markdown 源码模式、笔记聚焦;
// - PDF:自管滚动容器虚拟化、页码跳转、适应宽度、阅读主题、高亮/下划线/
//   涂鸦/便签(Acrobat 式)、点击标注管理(改色/换样式/写备注/删除)、书签大纲;
// - Markdown/DOCX:划词浮动菜单、CSS Custom Highlight 回渲染、段落锚定
//   涂鸦(richTextTools)、大纲、源码模式(Typora 式)、笔记一键捕获。
import {
  memo,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { Markdown } from "./Markdown";
import DOMPurify from "dompurify";
import {
  ZoomIn,
  ZoomOut,
  Maximize2,
  PenLine,
  Highlighter,
  Eraser,
  Type,
  Trash2,
  ListTree,
  StickyNote,
  Search,
  Languages,
  Undo2,
  Redo2,
  Lasso,
  Square,
  Circle,
  Minus,
  MoveUpRight,
  Layers,
  Printer,
  PanelLeft,
  ListChecks,
  ScanText,
} from "lucide-react";
import { pageWindow } from "./pageWindow";
import { useInlineTranslation, useInlineTranslationLayer } from "./inlineTranslation";
import {
  PageTranslationPanel,
  TRANSLATE_LANGUAGES,
  TRANSLATION_IPC_TIMEOUT_MS,
  loadTranslationGlossary,
  submitWholeDocumentTranslation,
  usePdfPageTranslation,
} from "./translationScopes";
import {
  TextBoxEditor,
  TextBoxBubble,
  type TextBoxEditorState,
} from "./textBox";
import { loadPdfjs, type PdfPageText } from "./pdfDocument";
import { PageOcrBadge } from "./PageOcrBadge";
import { PageOcrRecognizeDialog } from "./pageOcrRecognize";
import type { PageOcrStatusView } from "../../shared/generated/PageOcrStatusView";
import type { CommandClient } from "../../app/api/client";
import { clipboardWriteText } from "../../app/ipc/transport";
import type { AnchorView } from "../../shared/generated/AnchorView";
import type { AnnotationView } from "../../shared/generated/AnnotationView";
import type { AnnotationColor } from "../../shared/generated/AnnotationColor";
import type { RawSelectionInput } from "../../shared/generated/RawSelectionInput";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import { QueryClientContext } from "@tanstack/react-query";
import { qk } from "../../app/api/keys";
import { useWorkspaceUi } from "../workspace/uiStore";
import {
  ANNOTATION_PALETTE,
  INK_BLOCK_SELECTOR,
  INK_PREFIX,
  blockTextRanges,
  clientRectsToPdfRects,
  decodeInk,
  decodeStyleNote,
  eraseInkCircle,
  domOffsetOf,
  encodeInk,
  encodeStyleNote,
  headingPathOf,
  inkBoundsToPdfRect,
  paletteOf,
  rangeFromOffsets,
  decodeTextBox,
  DEFAULT_TEXTBOX_STYLE,
  type InkPoint,
  type InkStroke,
} from "./annotations";
import {
  choosePositionBlock,
  locatorFromPositionBlock,
  restoreTargetFromLocator,
  type PositionBlockCandidate,
} from "./position";
import { loadPdfZoom, readerPrefsStyle, savePdfZoom, type ReaderPrefs } from "./readerPrefs";
import { buildCitationMarkdown } from "./selection";
import { useReaderTools, PEN_COLORS, type ReaderTool } from "./readerTools";
import { loadDocumentPen, saveDocumentPen } from "./readerTools";
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
  selectionBoxPx,
  strokeInPolygon,
  translateStrokes,
} from "./inkGeometry";
import { ConfirmDialog, type ConfirmRequest } from "./ConfirmDialog";
import { isRevisionConflict, useAnnotationConflict } from "./annotationConflict";

/** 线宽比例 ↔ 滑杆(2–10):只做界面映射,存储仍是比例。 */
function penWidthSlider(width: number): number {
  return Math.round(Math.min(10, Math.max(2, width * 1000)));
}
function sliderToPenWidth(value: number): number {
  return Math.min(0.012, Math.max(0.0015, value / 1000));
}
import { broadcastAnnotationChange, type AnnotationChangeDetail } from "./annotationEvents";
import { RichTextInkLayer, RichTextToolbar } from "./richTextTools";
import { inkStrokeElements } from "./InkSvg";
import { ReaderSearch } from "./ReaderSearch";
import { ReaderToolbarExtras } from "./readerToolbarExtras";
import { NoteLinksLayer } from "./noteLinks";
import { SelectionToolbar, type SelectionToolbarState } from "./SelectionToolbar";
import "./reading.css";

// —— 通用:标注创建(真实锚点 + 标注;样式写入 noteText 前缀)——

async function createAnnotatedSelection(
  client: CommandClient,
  input: {
    documentId: string;
    versionId: string;
    format: "pdf" | "markdown" | "docx";
    text: string;
    primaryLocator: SourceLocator;
    color: AnnotationColor;
    style: "highlight" | "underline";
    note?: string;
    selectionKind?: "text" | "page";
  },
): Promise<AnnotationView> {
  const rawSelection: RawSelectionInput = {
    documentId: input.documentId,
    documentVersionId: input.versionId,
    format: input.format,
    text: input.text,
    blocks: [
      {
        renderKey: `${input.format}-${input.primaryLocator.kind}-${Date.now()}`,
        text: input.text,
        blockOrder: 0,
        locator: input.primaryLocator,
      },
    ],
    primaryLocator: input.primaryLocator,
    selectionKind: input.selectionKind ?? "text",
  };
  const anchor = await client.call<AnchorView>("evidence_create_anchor", {
    rawSelection,
    idempotencyKey: client.newIdempotencyKey(),
  });
  return client.call<AnnotationView>("annotation_create", {
    documentId: input.documentId,
    anchorId: anchor.id,
    color: input.color,
    noteText: encodeStyleNote(input.style, input.note ?? ""),
    idempotencyKey: client.newIdempotencyKey(),
  });
}

/** 就选区向 Agent 提问:写入草稿并打开面板(全格式一致)。 */
function askAboutSelection(text: string) {
  useWorkspaceUi.getState().setDraft(`关于「${text.slice(0, 200)}」:\n\n`);
  useWorkspaceUi.getState().setAgentOpen(true);
}

// —— Markdown / DOCX:划词 + CSS Custom Highlight 回渲染 ——

const DOM_ALLOWED_TAGS = [
  "p", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote",
  "table", "thead", "tbody", "tr", "td", "th", "strong", "em", "a",
  "br", "hr", "span", "code", "pre", "img",
];

// —— Markdown / DOCX 阅读体验:位置保存恢复、阅读偏好、失败提示 ——

/** 是否要求减少动效(平滑滚动需尊重系统设置)。 */
function prefersReducedMotion(): boolean {
  try {
    return window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
  } catch {
    return false;
  }
}

/** 站内轻提示(role="status",2 秒自动消失,不打断阅读)。 */
function useToast(durationMs = 2000): {
  toast: string | null;
  showToast: (text: string, durationOverride?: number) => void;
} {  const [toast, setToast] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const showToast = useCallback(
    (text: string, durationOverride?: number) => {
      setToast(text);
      if (timerRef.current !== null) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(() => {
        timerRef.current = null;
        setToast(null);
      }, durationOverride ?? durationMs);
    },
    [durationMs],
  );
  useEffect(
    () => () => {
      if (timerRef.current !== null) clearTimeout(timerRef.current);
    },
    [],
  );
  return { toast, showToast };
}

/**
 * 块所属的标题路径(按文档顺序 + 标题级别维护层级栈)。
 * 与 annotations.headingPathOf(仅收集祖先标题)不同:正文段落是标题的兄弟
 * 节点,位置恢复需要「前面最近的标题链」。
 *
 * 旧实现对每个块都重新扫描全部标题并做 `compareDocumentPosition`,
 * 长文档是 O(块×标题)(实测 721 块 × 61 标题 ≈ 4.4 万次比较)。这里改为
 * 一次文档序遍历:遇到标题推进层级栈并快照路径,正文块取当前快照。
 */
function buildHeadingPathIndex(root: HTMLElement): {
  pathOf: (block: HTMLElement) => string[];
} {
  const snapshot = new Map<HTMLElement, string[]>();
  const stack: Array<{ level: number; title: string }> = [];
  // 与解析器一致的重复标题消歧("标题#2"):跨父级按文本出现次数编号。
  const seen = new Map<string, number>();
  const current = (): string[] => stack.map((entry) => entry.title);
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
  let node: Node | null = walker.nextNode();
  while (node !== null) {
    const el = node as HTMLElement;
    const tag = el.tagName;
    if (/^H[1-6]$/.test(tag)) {
      const title = (el.textContent ?? "").trim().slice(0, 80);
      if (title.length > 0) {
        const count = (seen.get(title) ?? 0) + 1;
        seen.set(title, count);
        const level = Number(tag[1]);
        while (stack.length > 0 && stack[stack.length - 1]!.level >= level) stack.pop();
        stack.push({ level, title: count > 1 ? `${title}#${count}` : title });
      }
      snapshot.set(el, current());
    } else if (el.matches(INK_BLOCK_SELECTOR)) {
      snapshot.set(el, current());
    }
    node = walker.nextNode();
  }
  return { pathOf: (block) => snapshot.get(block) ?? [] };
}

/**
 * 滚动位置跟踪:滚动停止约 250ms 后采集视口顶部最近的块,换算 locator 交给
 * 宿主防抖保存(宿主 PositionSaver 再节流 900ms)。只在用户滚动时触发:
 * 恢复位置/引用跳转的程序化滚动不写入,避免反复恢复时位置逐次漂移。
 */
function useRichTextPositionTracking({
  wrapRef,
  rootRef,
  format,
  enabled,
  onPositionChange,
  onViewportChange,
}: {
  wrapRef: React.RefObject<HTMLDivElement | null>;
  rootRef: React.RefObject<HTMLElement | null>;
  format: "markdown" | "docx";
  enabled: boolean;
  onPositionChange?: (locator: SourceLocator) => void;
  onViewportChange?: (viewport: { headingPath?: string[] }) => void;
}): void {
  const notifyRef = useRef(onPositionChange);
  notifyRef.current = onPositionChange;
  const notifyViewportRef = useRef(onViewportChange);
  notifyViewportRef.current = onViewportChange;
  useEffect(() => {
    if (!enabled) return;
    const wrap = wrapRef.current;
    const root = rootRef.current;
    if (wrap === null || root === null || notifyRef.current === undefined) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let lastOffset = -1;
    let lastHeading = "";
    // 用户是否有过滚动意图(滚轮/触摸/键盘/拖拽);程序化滚动一律不写。
    let userInitiated = false;
    const markUser = () => {
      userInitiated = true;
    };
    const compute = (writePosition = true) => {
      timer = null;
      const rootEl = rootRef.current;
      const wrapEl = wrapRef.current;
      if (rootEl === null || wrapEl === null) return;
      const wrapTop = wrapEl.getBoundingClientRect().top;
      const elements = Array.from(
        rootEl.querySelectorAll<HTMLElement>(INK_BLOCK_SELECTOR),
      );
      // 单遍文本偏移 + 单遍标题索引:旧实现每块两次 TreeWalker + 全量
      // compareDocumentPosition,长文档约 160ms 的主线程长任务。
      const ranges = blockTextRanges(rootEl, elements);
      const headingIndex = buildHeadingPathIndex(rootEl);
      const blocks: PositionBlockCandidate[] = [];
      for (const el of elements) {
        const range = ranges.get(el);
        if (range === undefined) continue;
        const rect = el.getBoundingClientRect();
        blocks.push({
          offset: range.start,
          endOffset: range.end,
          headingPath: headingIndex.pathOf(el),
          top: rect.top - wrapTop,
          bottom: rect.bottom - wrapTop,
          blockType: el.tagName.toLowerCase(),
        });
      }
      const block = choosePositionBlock(blocks, 0);
      if (block === null) return;
      const headingKey = block.headingPath.join("/");
      if (headingKey !== lastHeading) {
        lastHeading = headingKey;
        notifyViewportRef.current?.({ headingPath: block.headingPath });
      }
      if (!writePosition || block.offset === lastOffset) return;
      lastOffset = block.offset;
      notifyRef.current?.(locatorFromPositionBlock(format, block));
    };
    const schedule = () => {
      if (!userInitiated) return;
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => compute(), 250);
    };
    wrap.addEventListener("scroll", schedule, { passive: true });
    wrap.addEventListener("wheel", markUser, { passive: true });
    // 首屏也要报告“本节”范围(只读视口,不写阅读位置)。
    const initial = setTimeout(() => compute(false), 300);
    wrap.addEventListener("touchmove", markUser, { passive: true });
    wrap.addEventListener("pointerdown", markUser);
    window.addEventListener("keydown", markUser);
    return () => {
      clearTimeout(initial);
      wrap.removeEventListener("scroll", schedule);
      wrap.removeEventListener("wheel", markUser);
      wrap.removeEventListener("touchmove", markUser);
      wrap.removeEventListener("pointerdown", markUser);
      window.removeEventListener("keydown", markUser);
      if (timer !== null) clearTimeout(timer);
    };
  }, [enabled, format, rootRef, wrapRef]);
}

/**
 * 恢复上次阅读位置:精确偏移 → 标题路径降级;失败静默停在文首,不阻塞阅读。
 * 平滑滚动尊重 reducedMotion。
 * locator 为 undefined 表示位置查询尚未返回(继续等待);null 表示无保存位置。
 */
function useRestoreRichTextPosition({
  rootRef,
  format,
  locator,
}: {
  rootRef: React.RefObject<HTMLElement | null>;
  format: "markdown" | "docx";
  locator?: SourceLocator | null;
}): void {
  const restoredRef = useRef(false);
  useEffect(() => {
    if (locator === undefined || restoredRef.current) return;
    const root = rootRef.current;
    if (root === null) return;
    const target = restoreTargetFromLocator(locator, format);
    if (target === null) {
      restoredRef.current = true;
      return;
    }
    const wrap = root.closest(".reader-body-wrap") as HTMLElement | null;
    const reveal = (el: HTMLElement | null): boolean => {
      if (el === null) return false;
      // 恢复位置是"回到上次所在处",不是动画。`scrollIntoView`(即便
      // behavior:auto)会让浏览器对整篇文档做一次可见性/布局收敛,实测
      // 4 万字文档单次 40–90ms 主线程。这里改为直接把滚动容器设到目标偏移:
      // 只触发一次滚动,不再做整篇布局。
      if (wrap !== null) {
        const wrapRect = wrap.getBoundingClientRect();
        const elRect = el.getBoundingClientRect();
        const targetTop =
          wrap.scrollTop + (elRect.top - wrapRect.top) - (wrap.clientHeight - elRect.height) / 2;
        wrap.scrollTop = Math.max(0, targetTop);
      } else {
        el.scrollIntoView?.({ behavior: "auto", block: "center" });
      }
      el.classList.add("reader-restore-flash");
      window.setTimeout(() => el.classList.remove("reader-restore-flash"), 1400);
      return true;
    };
    // 1) 精确偏移(渲染文本 UTF-16 偏移,与标注同源)。
    const start = Math.max(0, target.startOffset);
    const range = rangeFromOffsets(root, start, Math.max(start + 1, target.endOffset));
    if (reveal(range?.startContainer.parentElement ?? null)) {
      restoredRef.current = true;
      return;
    }
    // 2) 标题路径降级(重复标题按完整路径区分)。
    if (target.headingPath.length > 0) {
      const pathKey = target.headingPath.join("/");
      const headingIndex = buildHeadingPathIndex(root);
      const headings = root.querySelectorAll<HTMLElement>("h1,h2,h3,h4,h5,h6");
      const match = [...headings].find(
        (h) => headingIndex.pathOf(h).join("/") === pathKey,
      );
      if (reveal(match ?? null)) {
        restoredRef.current = true;
        return;
      }
    }
    // 3) 定位失败:静默(读者可自行滚动,不弹错误打断)。
    restoredRef.current = true;
  }, [format, locator, rootRef]);
}

/** 引用回跳(MD/DOCX):跳转失败给一次提示,不再静默。 */
function useTextGotoWithFeedback(
  rootRef: React.RefObject<HTMLElement | null>,
  showToast: (text: string) => void,
): void {
  useEffect(() => {
    const onGoto = (event: Event) => {
      const detail = (
        event as CustomEvent<{ start?: number; end?: number }>
      ).detail;
      const root = rootRef.current;
      const start = detail?.start;
      const end = detail?.end;
      if (root === null || start === undefined || end === undefined) return;
      const range = rangeFromOffsets(root, start, end);
      if (range === null) {
        showToast("引用位置已失效,未能跳转");
        return;
      }
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(range);
      range.startContainer.parentElement?.scrollIntoView?.({
        behavior: prefersReducedMotion() ? "auto" : "smooth",
        block: "center",
      });
    };
    window.addEventListener("knowwise-goto-text", onGoto);
    return () => window.removeEventListener("knowwise-goto-text", onGoto);
  }, [rootRef, showToast]);
}

/** DOCX 引用回跳:解析定位是块序(0-based),按正文块元素顺序滚动并高亮。 */
function useDocxGotoWithFeedback(
  rootRef: React.RefObject<HTMLElement | null>,
  showToast: (text: string) => void,
): void {
  useEffect(() => {
    const onGoto = (event: Event) => {
      const detail = (
        event as CustomEvent<{ start?: number; end?: number }>
      ).detail;
      const root = rootRef.current;
      const start = detail?.start;
      const end = detail?.end;
      if (root === null || start === undefined || end === undefined) return;
      const blocks = collectDocxBlockElements(root);
      const from = Math.max(0, Math.min(start, end));
      const to = Math.min(blocks.length - 1, Math.max(start, end));
      if (from >= blocks.length || to < 0) {
        showToast("引用位置已失效,未能跳转");
        return;
      }
      const first = blocks[from];
      if (first === undefined) {
        showToast("引用位置已失效,未能跳转");
        return;
      }
      first.scrollIntoView?.({
        behavior: prefersReducedMotion() ? "auto" : "smooth",
        block: "center",
      });
      const targets = blocks.slice(from, to + 1);
      for (const element of targets) {
        element.classList.add("reader-restore-flash");
        window.setTimeout(() => element.classList.remove("reader-restore-flash"), 1600);
      }
    };
    window.addEventListener("knowwise-goto-docx", onGoto);
    return () => window.removeEventListener("knowwise-goto-docx", onGoto);
  }, [rootRef, showToast]);
}

/**
 * DOCX 正文块元素(与 adaptDocxHtml 的块序同源):
 * 标题/段落/引用/代码/列表项/表格各占一个序号;容器下钻。
 */
function collectDocxBlockElements(root: HTMLElement): HTMLElement[] {
  const out: HTMLElement[] = [];
  const walk = (element: Element) => {
    for (const child of Array.from(element.children)) {
      const tag = child.tagName.toLowerCase();
      if (/^h[1-6]$/.test(tag) || tag === "p" || tag === "blockquote" || tag === "pre") {
        out.push(child as HTMLElement);
      } else if (tag === "ul" || tag === "ol") {
        for (const item of Array.from(child.children)) {
          if (item.tagName.toLowerCase() === "li") out.push(item as HTMLElement);
        }
      } else if (tag === "table") {
        out.push(child as HTMLElement);
      } else if (child.children.length > 0) {
        walk(child);
      }
    }
  };
  walk(root);
  return out;
}

function useRichTextSelection({
  rootRef,
  client,
  documentId,
  versionId,
  format,
  documentTitle,
  onToast,
}: {
  rootRef: React.RefObject<HTMLElement | null>;
  client?: CommandClient;
  documentId?: string;
  versionId?: string;
  format: "markdown" | "docx";
  /** 复制为引用时的文档名。 */
  documentTitle?: string;
  /** 失败提示(标注未保存 / 复制引用失败);2 秒轻提示。 */
  onToast: (text: string, durationMs?: number) => void;
}) {
  const [popover, setPopover] = useState<SelectionToolbarState | null>(null);
  const [copied, setCopied] = useState(false);
  const canAnnotate = Boolean(client && documentId && versionId);
  const setTool = useReaderTools((s) => s.setTool);

  const handleMouseUp = useCallback(() => {
    const root = rootRef.current;
    const sel = typeof window === "undefined" ? null : window.getSelection();
    if (root === null || sel === null || sel.isCollapsed || sel.rangeCount === 0) {
      setPopover(null);
      return;
    }
    const range = sel.getRangeAt(0);
    const text = sel.toString().trim();
    if (text.length === 0 || !root.contains(range.commonAncestorContainer)) {
      setPopover(null);
      return;
    }
    const rect = range.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) return;
    // Capture offsets while the range is alive: clicking a toolbar button can
    // collapse the document selection before onClick runs.
    let start: number | undefined;
    let end: number | undefined;
    try {
      const rawStart = domOffsetOf(root, range.startContainer, range.startOffset);
      const rawEnd = domOffsetOf(root, range.endContainer, range.endOffset);
      if (rawStart >= 0 && rawEnd >= 0) {
        start = Math.min(rawStart, rawEnd);
        end = Math.max(rawStart, rawEnd);
      }
    } catch {
      start = undefined;
      end = undefined;
    }
    setPopover({
      text,
      x: Math.min(Math.max(rect.left + rect.width / 2, 180), window.innerWidth - 180),
      y: Math.max(rect.top, 60),
      headingPath: headingPathOf(root, range.startContainer),
      start,
      end,
    });
  }, [rootRef]);

  /** 建立选区标注;openNote=true 时作为笔记打开(出处=真实锚点)。 */
  const annotate = useCallback(
    async (
      color: AnnotationColor,
      style: "highlight" | "underline",
      openNote = false,
    ) => {
      if (!client || !documentId || !versionId) return;
      const text = popover?.text.trim() ?? "";
      if (text.length === 0) return;
      let start = popover?.start;
      let end = popover?.end;
      let headingPath = popover?.headingPath ?? [];
      if (start === undefined || end === undefined) {
        // Fallback for callers that did not capture offsets (PDF selection).
        const root = rootRef.current;
        const sel = typeof window === "undefined" ? null : window.getSelection();
        if (!root || sel === null || sel.rangeCount === 0) return;
        const range = sel.getRangeAt(0);
        const rawStart = domOffsetOf(root, range.startContainer, range.startOffset);
        const rawEnd = domOffsetOf(root, range.endContainer, range.endOffset);
        if (rawStart < 0 || rawEnd < 0) return;
        start = Math.min(rawStart, rawEnd);
        end = Math.max(rawStart, rawEnd);
        headingPath = headingPathOf(root, range.startContainer);
      }
      if (start === undefined || end === undefined) return;
      const locator: SourceLocator =
        format === "markdown"
          ? {
              kind: "markdown",
              startOffset: start,
              endOffset: end,
              mdastNodeType: "text",
              headingPath,
              locatorVersion: 1,
            }
          : {
              kind: "docx",
              bodyBlockStart: start,
              bodyBlockEnd: end,
              paragraphIds: [],
              headingPath,
              locatorVersion: 1,
            };
      setPopover(null);
      window.getSelection()?.removeAllRanges();
      try {
        const created = await createAnnotatedSelection(client, {
          documentId,
          versionId,
          format,
          text,
          primaryLocator: locator,
          color,
          style,
        });
        broadcastAnnotationChange({ type: "created", annotation: created });
        if (openNote) useReaderTools.getState().requestNotesFocus(created.id);
      } catch {
        // 创建失败:不打断阅读,但给 2 秒轻提示(不再静默)。
        onToast("标注未保存,请重试");
      }
    },
    [client, documentId, format, popover, rootRef, versionId, onToast],
  );

  const copy = useCallback(() => {
    if (popover === null) return;
    void navigator.clipboard?.writeText(popover.text);
    setCopied(true);
    setTimeout(() => {
      setCopied(false);
      setPopover(null);
    }, 1000);
  }, [popover]);

  /** 记入「待澄清」:保留原文与可回跳位置,问题由用户在笔记面板补完。 */
  const clarify = useCallback(() => {
    if (popover === null) return;
    const text = popover.text;
    const headingPath = popover.headingPath ?? [];
    let locator: SourceLocator | null = null;
    const root = rootRef.current;
    const sel = typeof window === "undefined" ? null : window.getSelection();
    if (root !== null && sel !== null && sel.rangeCount > 0) {
      try {
        const range = sel.getRangeAt(0);
        let start = domOffsetOf(root, range.startContainer, range.startOffset);
        let end = domOffsetOf(root, range.endContainer, range.endOffset);
        if (start >= 0 && end >= 0) {
          if (start > end) [start, end] = [end, start];
          locator =
            format === "markdown"
              ? {
                  kind: "markdown",
                  startOffset: start,
                  endOffset: end,
                  mdastNodeType: "text",
                  headingPath,
                  locatorVersion: 1,
                }
              : {
                  kind: "docx",
                  bodyBlockStart: start,
                  bodyBlockEnd: end,
                  paragraphIds: [],
                  headingPath,
                  locatorVersion: 1,
                };
        }
      } catch {
        locator = null;
      }
    }
    setPopover(null);
    sel?.removeAllRanges();
    useReaderTools.getState().requestClarificationFocus({ quote: text, locator });
  }, [format, popover, rootRef]);

  const aiNote = useCallback(() => {
    if (popover === null) return;
    const text = popover.text;
    useWorkspaceUi.getState().setDraft(`请根据选区内容「${text.slice(0, 300)}」，提炼核心要点并生成一条结构清晰的便签笔记：\n\n`);
    useWorkspaceUi.getState().setAgentOpen(true);
    setPopover(null);
    onToast("已将选区载入 AI 笔记生成提示");
  }, [onToast, popover]);

  /** 复制为 Markdown 引用块:引文 + 《文档名》· 章节名。 */
  const copyCitation = useCallback(() => {
    if (popover === null) return;
    const headingPath = popover.headingPath ?? [];
    const citation = buildCitationMarkdown({
      quote: popover.text,
      documentTitle,
      section: headingPath[headingPath.length - 1] ?? null,
    });
    setPopover(null);
    void (async () => {
      try {
        await clipboardWriteText(citation);
        onToast("已复制引用块(含出处),可直接粘贴到笔记");
      } catch {
        onToast("复制引用失败,请重试");
      }
    })();
  }, [popover, documentTitle, onToast]);

  const ask = useCallback(() => {
    if (popover === null) return;
    askAboutSelection(popover.text);
    setPopover(null);
  }, [popover]);

  /**
   * 翻译选区(浏览器式):把选中文字翻译成指定语言,结果就地显示在原文下方。
   * 只翻译这一段,不生成产物、不建任务。
   */
  const translateSelection = useCallback(
    async (targetLanguage?: string) => {
      if (popover === null || !client || !documentId) return;
      const text = popover.text;
      const target =
        targetLanguage ?? useReaderTools.getState().inlineTranslateTarget;
      if (targetLanguage !== undefined) {
        useReaderTools.getState().setInlineTranslateTarget(targetLanguage);
      }
      setPopover(null);
      // 远端模型可能耗时数十秒:先给出"正在翻译"的真实状态,避免点了没反应。
      onToast("正在翻译选中文字…", 120_000);
      try {
        const result = await client.call<{
          targetLanguage: string;
          segments: Array<{ source: string; translated: string | null; errorCode: string | null }>;
        }>("translation_inline", {
          documentId,
          targetLanguage: target,
          segments: [text],
        }, { timeoutMs: TRANSLATION_IPC_TIMEOUT_MS });
        const first = result.segments[0];
        if (first?.translated) {
          useReaderTools.getState().pushSelectionTranslation({
            source: text,
            translated: first.translated,
            targetLanguage: result.targetLanguage,
            x: popover.x,
            y: popover.y,
          });
          onToast("已翻译选中文字");
        } else {
          onToast(`翻译未完成(${first?.errorCode ?? "unknown"}),可重试`);
        }
      } catch (error) {
        const code =
          typeof error === "object" && error !== null && "appError" in error
            ? String((error as { appError: { code?: string } }).appError.code ?? "translation_failed")
            : "translation_failed";
        onToast(`翻译未完成(${code}),可重试`);
      }
    },
    [client, documentId, onToast, popover],
  );

  // 涂鸦/便签激活时不弹划词菜单。
  useEffect(() => {
    if (useReaderTools.getState().tool !== "read") setPopover(null);
  }, [setTool]);

  return {
    popover,
    handleMouseUp,
    annotate,
    copy,
    clarify,
    copyCitation,
    ask,
    copied,
    canAnnotate,
    aiNote,
    translateSelection,
  };
}

/** 把服务端标注注册为 CSS Custom Highlight(WebView2 支持;jsdom 自动跳过)。 */
function useRegisterHighlights(
  rootRef: React.RefObject<HTMLElement | null>,
  annotations: AnnotationView[],
  format: "markdown" | "docx",
  contentKey: string,
  visible = true,
) {
  useEffect(() => {
    const root = rootRef.current;
    const highlightsApi = (window as unknown as { CSS?: { highlights?: Map<string, unknown> } })
      .CSS?.highlights;
    if (root === null || !highlightsApi) return;
    for (const p of ANNOTATION_PALETTE) {
      highlightsApi.delete(`kw-hl-${p.color}`);
      highlightsApi.delete(`kw-ul-${p.color}`);
    }
    if (!visible) return;
    const HighlightCtor = (
      window as unknown as { Highlight?: new (...ranges: Range[]) => unknown }
    ).Highlight;
    if (HighlightCtor === undefined) return;
    const byKey = new Map<string, Range[]>();
    for (const a of annotations) {
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
      const range = rangeFromOffsets(root, start, end);
      if (range === null) continue;
      const { style } = decodeStyleNote(a.noteText);
      const key = `${style === "underline" ? "kw-ul" : "kw-hl"}-${a.color}`;
      const list = byKey.get(key) ?? [];
      list.push(range);
      byKey.set(key, list);
    }
    for (const [key, ranges] of byKey) {
      highlightsApi.set(key, new HighlightCtor(...ranges));
    }
    return () => {
      for (const key of byKey.keys()) highlightsApi.delete(key);
    };
  }, [rootRef, annotations, format, contentKey, visible]);
}

function RichTextAnnotationsLayer({
  client,
  documentId,
  versionId,
  format,
  rootRef,
  selection,
}: {
  client?: CommandClient;
  documentId?: string;
  versionId?: string;
  format: "markdown" | "docx";
  rootRef: React.RefObject<HTMLElement | null>;
  selection: ReturnType<typeof useRichTextSelection>;
}): ReactNode {
  const [annotations, setAnnotations] = useState<AnnotationView[] | null>(null);

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
    const onInvalidate = () => void load();
    window.addEventListener("knowwise-annotations-changed", onInvalidate);
    return () => {
      cancelled = true;
      window.removeEventListener("knowwise-annotations-changed", onInvalidate);
    };
  }, [client, documentId, versionId]);

  const layers = useReaderTools((state) => state.layers);
  const inlineTarget = useReaderTools((state) => state.inlineTranslateTarget);
  useRegisterHighlights(
    rootRef,
    annotations ?? [],
    format,
    `${documentId}:${versionId ?? ""}`,
    layers.highlight,
  );

  if (selection.popover === null) return null;
  return (
    <SelectionToolbar
      state={selection.popover}
      onAnnotate={(color, style) => void selection.annotate(color, style)}
      onNote={() => void selection.annotate("yellow", "highlight", true)}
      onAsk={selection.ask}
      onCopy={selection.copy}
      onClarify={selection.clarify}
      onCopyCitation={selection.copyCitation}
      onAiNote={selection.aiNote}
      onTranslate={(target) => void selection.translateSelection(target)}
      copied={selection.copied}
      canAnnotate={selection.canAnnotate}
      client={client}
      targetLanguage={inlineTarget}
    />
  );
}

export function MarkdownTextView({
  text,
  documentId,
  versionId,
  client,
  documentTitle,
  prefs,
  initialLocator,
  onPositionChange,
  onViewportChange,
}: {
  text: string;
  documentId?: string;
  versionId?: string;
  client?: CommandClient;
  /** 文档名(复制为引用的出处)。 */
  documentTitle?: string;
  /** 阅读偏好:字号 / 行距 / 版心宽度。 */
  prefs?: ReaderPrefs;
  /** 上次保存的位置(精确偏移 → 标题路径降级)。 */
  initialLocator?: SourceLocator | null;
  /** 滚动位置变化(宿主防抖 900ms 后写 reader_save_position)。 */
  onPositionChange?: (locator: SourceLocator) => void;
  /** 视口所在标题路径(供右栏“本节”范围)。 */
  onViewportChange?: (viewport: { headingPath?: string[] }) => void;
}): ReactNode {
  const rootRef = useRef<HTMLDivElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const mdSource = useReaderTools((s) => s.mdSource);
  const tool = useReaderTools((s) => s.tool);
  const inlineTranslate = useReaderTools((s) => s.inlineTranslate);
  const inlineTarget = useReaderTools((s) => s.inlineTranslateTarget);
  const inlineState = useInlineTranslation(
    rootRef,
    documentId ?? null,
    versionId ?? null,
    inlineTranslate && !mdSource,
    inlineTarget,
  );
  useInlineTranslationLayer(rootRef, inlineState, inlineTranslate && !mdSource, inlineTarget);
  const { toast, showToast } = useToast();
  const selection = useRichTextSelection({
    rootRef,
    client,
    documentId,
    versionId,
    format: "markdown",
    documentTitle,
    onToast: showToast,
  });
  useTextGotoWithFeedback(rootRef, showToast);
  useRichTextPositionTracking({
    wrapRef,
    rootRef,
    format: "markdown",
    enabled: !mdSource,
    onPositionChange,
    onViewportChange,
  });
  useRestoreRichTextPosition({ rootRef, format: "markdown", locator: initialLocator });

  return (
    <div className="rich-text-view">
      <SelectionTranslationToast />
      <RichTextToolbar format="markdown" rootRef={rootRef} disabled={mdSource} client={client} documentId={documentId} documentTitle={documentTitle} onToast={showToast} />
      {toast ? (
        <div className="reader-toast" role="status">
          {toast}
        </div>
      ) : null}
      <div
        className="reader-body-wrap"
        ref={wrapRef}
        style={prefs ? readerPrefsStyle(prefs) : undefined}
        onMouseUp={mdSource || tool !== "read" ? undefined : selection.handleMouseUp}
      >
        {mdSource ? (
          <pre className="md-source-view" aria-label="Markdown 源码">{text}</pre>
        ) : (
          <div className="reader-body markdown-body prose max-w-none" ref={rootRef}>
            <Markdown>{text}</Markdown>
          </div>
        )}
        {!mdSource ? (
          <>
            <RichTextInkLayer
              wrapRef={wrapRef}
              rootRef={rootRef}
              client={client}
              documentId={documentId}
              versionId={versionId}
              format="markdown"
            />
            <NoteLinksLayer
              rootRef={rootRef}
              wrapRef={wrapRef}
              client={client}
              documentId={documentId}
              versionId={versionId}
              format="markdown"
              enabled={!mdSource}
            />
          </>
        ) : null}
        <RichTextAnnotationsLayer
          client={client}
          documentId={documentId}
          versionId={versionId}
          format="markdown"
          rootRef={rootRef}
          selection={selection}
        />
      </div>
    </div>
  );
}

/** DOCX:mammoth 输出的 HTML 先净化再挂载(M03-A4:raw HTML 不执行)。 */
export function DocxHtmlView({
  html,
  documentId,
  versionId,
  client,
  documentTitle,
  prefs,
  initialLocator,
  onPositionChange,
  onViewportChange,
}: {
  html: string;
  documentId?: string;
  versionId?: string;
  client?: CommandClient;
  /** 文档名(复制为引用的出处)。 */
  documentTitle?: string;
  /** 阅读偏好:字号 / 行距 / 版心宽度。 */
  prefs?: ReaderPrefs;
  /** 上次保存的位置(精确偏移 → 标题路径降级)。 */
  initialLocator?: SourceLocator | null;
  /** 滚动位置变化(宿主防抖 900ms 后写 reader_save_position)。 */
  onPositionChange?: (locator: SourceLocator) => void;
  /** 视口所在标题路径(供右栏“本节”范围)。 */
  onViewportChange?: (viewport: { headingPath?: string[] }) => void;
}): ReactNode {
  const rootRef = useRef<HTMLDivElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const tool = useReaderTools((s) => s.tool);
  const { toast, showToast } = useToast();
  const selection = useRichTextSelection({
    rootRef,
    client,
    documentId,
    versionId,
    format: "docx",
    documentTitle,
    onToast: showToast,
  });
  useTextGotoWithFeedback(rootRef, showToast);
  useDocxGotoWithFeedback(rootRef, showToast);
  useRichTextPositionTracking({
    wrapRef,
    rootRef,
    format: "docx",
    enabled: true,
    onPositionChange,
    onViewportChange,
  });
  useRestoreRichTextPosition({ rootRef, format: "docx", locator: initialLocator });
  // DOCX 与 Markdown 共用同一套内联译文(按块插入,不改动净化后的 HTML)。
  const inlineTranslateDocx = useReaderTools((s) => s.inlineTranslate);
  const inlineTargetDocx = useReaderTools((s) => s.inlineTranslateTarget);
  const inlineStateDocx = useInlineTranslation(
    rootRef,
    documentId ?? null,
    versionId ?? null,
    inlineTranslateDocx,
    inlineTargetDocx,
  );
  useInlineTranslationLayer(rootRef, inlineStateDocx, inlineTranslateDocx, inlineTargetDocx);
  const clean = useMemo(
    () =>
      DOMPurify.sanitize(html, {
        ALLOWED_TAGS: DOM_ALLOWED_TAGS,
        ALLOWED_ATTR: ["href", "src", "alt", "colspan", "rowspan"],
      }),
    [html],
  );

  return (
    <div className="rich-text-view">
      <SelectionTranslationToast />
      <RichTextToolbar format="docx" rootRef={rootRef} client={client} documentId={documentId} documentTitle={documentTitle} onToast={showToast} />
      {toast ? (
        <div className="reader-toast" role="status">
          {toast}
        </div>
      ) : null}
      <div
        className="reader-body-wrap"
        ref={wrapRef}
        style={prefs ? readerPrefsStyle(prefs) : undefined}
        onMouseUp={tool !== "read" ? undefined : selection.handleMouseUp}
      >
        <div
          className="reader-body docx-body prose max-w-none"
          ref={rootRef}
          dangerouslySetInnerHTML={{ __html: clean }}
        />
        <RichTextInkLayer
          wrapRef={wrapRef}
          rootRef={rootRef}
          client={client}
          documentId={documentId}
          versionId={versionId}
          format="docx"
        />
        <NoteLinksLayer
          rootRef={rootRef}
          wrapRef={wrapRef}
          client={client}
          documentId={documentId}
          versionId={versionId}
          format="docx"
        />
        <RichTextAnnotationsLayer
          client={client}
          documentId={documentId}
          versionId={versionId}
          format="docx"
          rootRef={rootRef}
          selection={selection}
        />
      </div>
    </div>
  );
}

// —— PDF ——

type ReaderTheme = "paper" | "sepia" | "night";
const THEME_LABELS: Record<ReaderTheme, string> = {
  paper: "纸白",
  sepia: "米黄",
  night: "夜间",
};
const THEME_KEY = "knowwise.reader.theme";

function loadTheme(): ReaderTheme {
  try {
    const v = window.localStorage.getItem(THEME_KEY);
    return v === "sepia" || v === "night" ? v : "paper";
  } catch {
    return "paper";
  }
}

/** 无尺寸信息时的占位高度估计(与旧实现一致的兜底)。 */
function estimatePageHeight(page: PdfPageText): number {
  return Math.max(380, page.lines.length * 24 + 140);
}

type PageAnnotations = {
  /** 文本高亮/下划线(rects 位于 user space)。 */
  marks: Array<{
    annotation: AnnotationView;
    rects: Array<{ x: number; y: number; width: number; height: number }>;
    underline: boolean;
  }>;
  /** 涂鸦笔迹(归一化)。 */
  inks: Array<{ annotation: AnnotationView; strokes: InkStroke[] }>;
  /** 便签气泡(quoteText === "便签" 判定)。 */
  stickies: Array<{
    annotation: AnnotationView;
    rect: { x: number; y: number; width: number; height: number };
  }>;
};

function groupAnnotationsByPage(
  annotations: AnnotationView[] | null,
): Map<number, PageAnnotations> {
  const map = new Map<number, PageAnnotations>();
  if (annotations === null) return map;
  for (const a of annotations) {
    const locator = a.locator;
    if (locator === null || locator.kind !== "pdf") continue;
    const bucket = map.get(locator.pageNumber) ?? { marks: [], inks: [], stickies: [] };
    if (a.noteText.startsWith(INK_PREFIX)) {
      const strokes = decodeInk(a.noteText);
      if (strokes !== null) bucket.inks.push({ annotation: a, strokes });
    } else if (a.quoteText === "便签" && locator.rects.length > 0) {
      bucket.stickies.push({ annotation: a, rect: locator.rects[0]! });
    } else if (locator.rects.length > 0) {
      const { style } = decodeStyleNote(a.noteText);
      bucket.marks.push({ annotation: a, rects: locator.rects, underline: style === "underline" });
    }
    map.set(locator.pageNumber, bucket);
  }
  return map;
}

type PdfOutlineItem = { title: string; page: number | null; level: number };

/** pdf.js 书签 → 标题 + 解析后的页码(1 基)。 */
async function resolveOutline(pdfDoc: unknown): Promise<PdfOutlineItem[]> {
  type RawOutline = {
    title: string;
    dest: unknown;
    items?: RawOutline[];
  };
  const doc = pdfDoc as {
    getOutline?: () => Promise<RawOutline[] | null>;
    getDestination?: (id: string) => Promise<unknown[] | null>;
    getPageIndex?: (ref: unknown) => Promise<number>;
  };
  if (typeof doc?.getOutline !== "function") return [];
  const raw = await doc.getOutline();
  if (raw === null) return [];
  const out: PdfOutlineItem[] = [];
  const walk = async (items: RawOutline[], level: number) => {
    for (const item of items) {
      let page: number | null = null;
      try {
        let dest = item.dest;
        if (typeof dest === "string" && typeof doc.getDestination === "function") {
          dest = await doc.getDestination(dest);
        }
        if (Array.isArray(dest) && dest[0] !== undefined && typeof doc.getPageIndex === "function") {
          page = (await doc.getPageIndex(dest[0])) + 1;
        }
      } catch {
        page = null;
      }
      out.push({ title: (item.title ?? "").trim(), page, level });
      if (Array.isArray(item.items) && item.items.length > 0) {
        await walk(item.items, level + 1);
      }
    }
  };
  await walk(raw, 0);
  return out.filter((i) => i.title.length > 0);
}

/**
 * 划词翻译结果:右下角浮层显示最近一次翻译(原文 + 译文 + 目标语言)。
 * 不插入正文,不改变阅读位置;可一键关闭,离开视图自动消失。
 */
function SelectionTranslationToast(): ReactNode {
  const entries = useReaderTools((s) => s.selectionTranslations);
  const clear = useReaderTools((s) => s.clearSelectionTranslations);
  const latest = entries[entries.length - 1];
  const latestId = latest?.id ?? null;
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  const dragRef = useRef<{ dx: number; dy: number } | null>(null);

  // 初始位置贴着被翻译的选区(选区下方),超界时夹回视口;之后可拖动。
  useEffect(() => {
    if (latest === undefined) {
      setPos(null);
      return;
    }
    const width = 360;
    const anchorX = latest.x ?? window.innerWidth - width - 24;
    const anchorY =
      latest.y !== undefined ? latest.y + 18 : window.innerHeight - 300;
    setPos({
      x: Math.min(
        Math.max(12, anchorX - width / 2),
        Math.max(12, window.innerWidth - width - 12),
      ),
      y: Math.min(Math.max(56, anchorY), Math.max(56, window.innerHeight - 220)),
    });
  }, [latestId]);

  const startDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (pos === null) return;
    event.preventDefault();
    dragRef.current = { dx: event.clientX - pos.x, dy: event.clientY - pos.y };
    const move = (e: PointerEvent) => {
      const drag = dragRef.current;
      if (drag === null) return;
      setPos({
        x: Math.min(Math.max(8, e.clientX - drag.dx), window.innerWidth - 120),
        y: Math.min(Math.max(48, e.clientY - drag.dy), window.innerHeight - 80),
      });
    };
    const up = () => {
      dragRef.current = null;
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up, { once: true });
  };

  if (latest === undefined || pos === null) return null;
  return (
    <div
      className="selection-translation"
      style={{ left: `${pos.x}px`, top: `${pos.y}px` }}
      role="status"
      aria-live="polite"
    >
      <div
        className="selection-translation-head"
        onPointerDown={startDrag}
        title="拖动移动译文框"
      >
        <span>译文 · {latest.targetLanguage}</span>
        <button
          type="button"
          className="selection-translation-close"
          aria-label="关闭译文"
          onClick={() => clear()}
        >
          ×
        </button>
      </div>
      <p className="selection-translation-source">{latest.source}</p>
      <p className="selection-translation-text">{latest.translated}</p>
    </div>
  );
}

/** 给 #rrggbb 加上透明度;非 hex 颜色原样返回(不猜测)。 */
function withAlpha(color: string, alpha: number): string {
  const m = /^#([0-9a-f]{6})$/i.exec(color.trim());
  if (m === null) return color;
  const value = parseInt(m[1]!, 16);
  const r = (value >> 16) & 0xff;
  const g = (value >> 8) & 0xff;
  const b = value & 0xff;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/**
 * 单页:Canvas 光栅 + TextLayer + 标注/笔迹/便签覆盖层 + 活动笔迹画布。
 *
 * 滚动时可见页窗口每帧都可能变化:若父组件每次重建页面元素,
 * 所有可见页都会连带重绘 canvas(实测 3 秒滚动产生 18 个 60–166ms
 * 长任务、仅 ~10 FPS)。memo 让只有属性真正变化的页重渲。
 */
const PdfSinglePage = memo(function PdfSinglePage({
  pageNumber,
  pageText,
  pdfDoc,
  scale,
  tool,
  penColor,
  penWidth,
  eraserRadius,
  layers,
  pageAnnotations,
  pendingStrokes = [],
  onCommitInk,
  onEraseInkAt,
  onEraseStart,
  onEraseEnd,
  onMoveInk,
  onDeleteInk,
  onManageMark,
  onInkContext,
  onAddSticky,
  onAddTextBox,
  onOpenSticky,
  ocrStatus,
  onReprocessOcr,
  onRecognizeOcrPages,
  pageCount,
  ocrBusy,
  client,
  documentId,
  versionId,
  onOcrCorrected,
}: {
  pageNumber: number;
  pageText: PdfPageText;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  pdfDoc?: any;
  scale: number;
  tool: ReaderTool;
  penColor: string;
  /** 线宽比例(相对页面宽度);高亮笔会在此基础上加粗并半透明。 */
  penWidth: number;
  /** 橡皮擦半径比例(相对页面宽度)。 */
  eraserRadius: number;
  layers: import("./readerTools").AnnotationLayers;
  pageAnnotations: PageAnnotations;
  /** 尚未落库的活动笔迹(乐观显示)。 */
  pendingStrokes?: InkStroke[];
  onCommitInk: (pageNumber: number, strokes: InkStroke[], pageW: number, pageH: number) => void;
  onEraseInkAt: (pageNumber: number, x: number, y: number, radius?: number) => void;
  onEraseStart: () => void;
  onEraseEnd: () => void;
  onMoveInk: (
    items: Array<{ annotation: AnnotationView; strokes: InkStroke[] }>,
    dx: number,
    dy: number,
  ) => void;
  onDeleteInk: (items: Array<{ annotation: AnnotationView; strokes: InkStroke[] }>) => void;
  onManageMark: (annotation: AnnotationView, x: number, y: number) => void;
  onInkContext: (annotation: AnnotationView, x: number, y: number) => void;
  onAddSticky: (pageNumber: number, x: number, y: number) => void;
  onAddTextBox: (
    pageNumber: number,
    nx: number,
    ny: number,
    screenX: number,
    screenY: number,
  ) => void;
  onOpenSticky: (annotation: AnnotationView) => void;
  ocrStatus?: PageOcrStatusView;
  onReprocessOcr?: () => void;
  onRecognizeOcrPages?: (pages: number[]) => void;
  pageCount?: number;
  ocrBusy?: boolean;
  client?: CommandClient;
  documentId?: string;
  versionId?: string;
  onOcrCorrected?: (statuses: PageOcrStatusView[]) => void;
}): ReactNode {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const inkCanvasRef = useRef<HTMLCanvasElement>(null);
  const textLayerRef = useRef<HTMLDivElement>(null);
  const frameRef = useRef<HTMLDivElement>(null);
  const [useFallback, setUseFallback] = useState(false);
  const [dimensions, setDimensions] = useState<{ width: number; height: number } | null>(
    pageText.width != null && pageText.height != null
      ? { width: pageText.width * scale, height: pageText.height * scale }
      : null,
  );
  const liveStrokeRef = useRef<InkStroke | null>(null);
  // 几何形状:首末点归一化,松手提交为一条带 t 的笔迹。
  const [shapeDraft, setShapeDraft] = useState<InkStroke | null>(null);
  const shapeStartRef = useRef<InkPoint | null>(null);
  const shapeDraftRef = useRef<InkStroke | null>(null);
  // 套索:多边形(归一化)+ 选中笔迹;移动时用像素增量预览。
  const [lassoPoints, setLassoPoints] = useState<InkPoint[] | null>(null);
  const [selection, setSelection] = useState<
    Array<{ annotation: AnnotationView; strokes: InkStroke[] }> | null
  >(null);
  const [moveDelta, setMoveDelta] = useState<{ dx: number; dy: number } | null>(null);
  const moveDeltaRef = useRef<{ dx: number; dy: number } | null>(null);
  const lassoDrawingRef = useRef<InkPoint[] | null>(null);
  const moveStartRef = useRef<{ x: number; y: number } | null>(null);
  // 指针事件处理器会跨渲染存活;用 ref 读取最新选区,避免闭包拿到
  // 选择前的 null,把"拖动已选笔迹"误判成"重新框选"并清空选区。
  const selectionRef = useRef(selection);
  selectionRef.current = selection;

  const clearLasso = useCallback(() => {
    setLassoPoints(null);
    setSelection(null);
    setMoveDelta(null);
    lassoDrawingRef.current = null;
    moveStartRef.current = null;
  }, []);

  useEffect(() => {
    if (tool !== "lasso") clearLasso();
  }, [tool, clearLasso]);

  useEffect(() => {
    if (selection === null) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      clearLasso();
      useReaderTools.getState().setTool("read");
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [selection, clearLasso]);

  useEffect(() => {
    let cancelled = false;

    async function renderPage() {
      if (!pdfDoc) {
        setUseFallback(true);
        return;
      }
      try {
        const page = await pdfDoc.getPage(pageNumber);
        if (cancelled) return;
        const viewport = page.getViewport({ scale });
        setDimensions({ width: viewport.width, height: viewport.height });

        const canvas = canvasRef.current;
        if (!canvas || !canvas.getContext) {
          setUseFallback(true);
          return;
        }
        let ctx: CanvasRenderingContext2D | null = null;
        try {
          ctx = canvas.getContext("2d");
        } catch {
          setUseFallback(true);
          return;
        }
        if (!ctx) {
          setUseFallback(true);
          return;
        }

        const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
        canvas.width = Math.floor(viewport.width * dpr);
        canvas.height = Math.floor(viewport.height * dpr);
        canvas.style.width = `${Math.floor(viewport.width)}px`;
        canvas.style.height = `${Math.floor(viewport.height)}px`;
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

        const renderTask = page.render({ canvasContext: ctx, viewport });
        await renderTask.promise;
        if (cancelled) return;

        const textLayerContainer = textLayerRef.current;
        if (textLayerContainer) {
          textLayerContainer.innerHTML = "";
          textLayerContainer.style.width = `${Math.floor(viewport.width)}px`;
          textLayerContainer.style.height = `${Math.floor(viewport.height)}px`;
          const textContentSource = await page.getTextContent();
          if (cancelled) return;
          const pdfjs = await loadPdfjs();
          if (pdfjs.TextLayer) {
            const textLayer = new pdfjs.TextLayer({
              textContentSource,
              container: textLayerContainer,
              viewport,
            });
            await textLayer.render();
          }
        }
      } catch {
        if (!cancelled) setUseFallback(true);
      }
    }

    void renderPage();
    return () => {
      cancelled = true;
    };
  }, [pdfDoc, pageNumber, scale]);

  const width = dimensions?.width ?? 612 * scale;
  const height = dimensions?.height ?? estimatePageHeight(pageText) * scale;

  // 活动笔迹:指针事件绘制,松手提交。
  //
  // 性能约束(实测):旧实现在每次 pointerdown 时设置画布 width/height。
  // 给 canvas 赋 width/height 会清空位图并重设后备存储,连续落笔等于
  // 每次重建整张画布;pointermove 也逐事件 stroke。这里改为:
  // - 画布尺寸只在尺寸变化时设置(用 data 属性记录,避免读回布局);
  // - 笔画点先入队,真正的绘制合并到 requestAnimationFrame 一次完成。
  const inkRafRef = useRef(0);
  const inkSizeRef = useRef<{ w: number; h: number }>({ w: 0, h: 0 });
  const inkPendingRef = useRef<Array<{ x: number; y: number }>>([]);
  // 橡皮圆形光标:位置用百分比定位,跟随指针,离开时隐藏。
  const [eraserRing, setEraserRing] = useState<{ x: number; y: number } | null>(null);
  const inkPenRef = useRef<{ color: string; width: number } | null>(null);

  const flushInk = useCallback(() => {
    inkRafRef.current = 0;
    const canvas = inkCanvasRef.current;
    const pen = inkPenRef.current;
    if (canvas === null || pen === null) return;
    const ctx = canvas.getContext("2d");
    if (ctx === null) return;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.strokeStyle = pen.color;
    ctx.lineWidth = pen.width;
    ctx.beginPath();
    const points = inkPendingRef.current;
    if (points.length > 0) {
      // 坐标用 CSS 像素(transform 已按 devicePixelRatio 放大),
      // 直接乘 canvas.width 会把笔迹画到可视区之外。
      const cssW = canvas.clientWidth || canvas.width;
      const cssH = canvas.clientHeight || canvas.height;
      const first = points[0]!;
      ctx.moveTo(first.x * cssW, first.y * cssH);
      for (let i = 1; i < points.length; i += 1) {
        const p = points[i]!;
        ctx.lineTo(p.x * cssW, p.y * cssH);
      }
      ctx.stroke();
      inkPendingRef.current = [points[points.length - 1]!];
    }
  }, []);

  useEffect(
    () => () => {
      if (inkRafRef.current !== 0) cancelAnimationFrame(inkRafRef.current);
      inkRafRef.current = 0;
    },
    [],
  );

  const inkPointerHandlers =
    tool === "read" || tool === "note" || tool === "textbox"
      ? {}
      : {
          onPointerDown: (event: ReactPointerEvent<HTMLCanvasElement>) => {
            const frame = frameRef.current;
            const canvas = inkCanvasRef.current;
            if (frame === null || canvas === null) return;
            const rect = frame.getBoundingClientRect();
            const x = (event.clientX - rect.left) / rect.width;
            const y = (event.clientY - rect.top) / rect.height;
            if (tool === "eraser") {
              onEraseStart();
              onEraseInkAt(pageNumber, x, y, eraserRadius);
              return;
            }
            if (tool === "lasso") {
              canvas.setPointerCapture?.(event.pointerId);
              const currentSelection = selectionRef.current;
              if (currentSelection !== null && currentSelection.length > 0) {
                // 用屏幕像素与真实选择框做命中(与用户看到的虚线框一致);
                // 归一化包围盒在缩放/圆角/坐标舍入下会漏判,导致选区被清空。
                const box = selectionBoxPx(
                  currentSelection.flatMap((item) => item.strokes),
                  width,
                  height,
                );
                const inside =
                  box !== null &&
                  event.clientX >= rect.left + box.left &&
                  event.clientX <= rect.left + box.left + box.width &&
                  event.clientY >= rect.top + box.top &&
                  event.clientY <= rect.top + box.top + box.height;
                if (inside) {
                  moveStartRef.current = { x, y };
                  moveDeltaRef.current = { dx: 0, dy: 0 };
                  setMoveDelta({ dx: 0, dy: 0 });
                  return;
                }
              }
              lassoDrawingRef.current = [{ x, y }];
              setLassoPoints([{ x, y }]);
              setSelection(null);
              setMoveDelta(null);
              return;
            }
            if (tool === "rect" || tool === "ellipse" || tool === "line" || tool === "arrow") {
              canvas.setPointerCapture?.(event.pointerId);
              shapeStartRef.current = { x, y };
              const draft = makeShapeStroke(tool, { x, y }, { x, y }, penColor, penWidth);
              shapeDraftRef.current = draft;
              setShapeDraft(draft);
              return;
            }
            canvas.setPointerCapture?.(event.pointerId);
            const ctx = canvas.getContext("2d");
            if (ctx === null) return;
            // 画布缓冲必须与显示尺寸一致。之前只在"尺寸变化时"重设,
            // 但 canvas 默认缓冲是 300×150,而页面宽 800+:CSS 用 inset:0
            // 把显示撑大、位图却仍是 300×150,落笔坐标按 300 宽换算后
            // 被压到左上角一小块——看起来就是"画了没反应"。
            const dprInk = window.devicePixelRatio || 1;
            const w = Math.max(1, Math.round(rect.width * dprInk));
            const h = Math.max(1, Math.round(rect.height * dprInk));
            if (
              inkSizeRef.current.w !== w ||
              inkSizeRef.current.h !== h ||
              canvas.width !== w ||
              canvas.height !== h
            ) {
              canvas.width = w;
              canvas.height = h;
              inkSizeRef.current = { w, h };
            } else {
              ctx.clearRect(0, 0, canvas.width, canvas.height);
            }
            ctx.setTransform(dprInk, 0, 0, dprInk, 0, 0);
            // 高亮笔:同一条笔迹,但更粗、半透明、圆头;普通笔按用户线宽。
            const isHighlight = tool === "highlight";
            const ratio = isHighlight ? Math.max(0.012, penWidth * 3) : penWidth;
            const lineWidth = Math.max(1.5, rect.width * ratio);
            const strokeColor = isHighlight ? withAlpha(penColor, 0.35) : penColor;
            inkPenRef.current = { color: strokeColor, width: lineWidth };
            liveStrokeRef.current = {
              color: strokeColor,
              width: ratio,
              points: [{ x, y }],
            };
            inkPendingRef.current = [{ x, y }];
            // 点击即出点(即时反馈)。
            ctx.fillStyle = strokeColor;
            ctx.beginPath();
            ctx.arc(
              x * rect.width,
              y * rect.height,
              Math.max(1.2, Math.max(1.5, lineWidth) / 2),
              0,
              Math.PI * 2,
            );
            ctx.fill();
          },
          onPointerMove: (event: ReactPointerEvent<HTMLCanvasElement>) => {
            const frame = frameRef.current;
            if (frame === null) return;
            if (tool === "eraser") {
              const rectR = frame.getBoundingClientRect();
              setEraserRing({
                x: (event.clientX - rectR.left) / rectR.width,
                y: (event.clientY - rectR.top) / rectR.height,
              });
              if (!(event.buttons & 1)) return;
              onEraseInkAt(
                pageNumber,
                (event.clientX - rectR.left) / rectR.width,
                (event.clientY - rectR.top) / rectR.height,
                eraserRadius,
              );
              return;
            }
            const rect = frame.getBoundingClientRect();
            const nx = Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
            const ny = Math.min(1, Math.max(0, (event.clientY - rect.top) / rect.height));
            if (tool === "lasso") {
              if (moveStartRef.current !== null) {
                if (!(event.buttons & 1)) return;
                moveDeltaRef.current = {
                  dx: nx - moveStartRef.current.x,
                  dy: ny - moveStartRef.current.y,
                };
                setMoveDelta(moveDeltaRef.current);
                return;
              }
              const drawing = lassoDrawingRef.current;
              if (drawing === null || !(event.buttons & 1)) return;
              const last = drawing[drawing.length - 1]!;
              if (Math.hypot(nx - last.x, ny - last.y) >= 0.004) {
                drawing.push({ x: nx, y: ny });
                setLassoPoints([...drawing]);
              }
              return;
            }
            if (shapeStartRef.current !== null) {
              if (!(event.buttons & 1)) return;
              const start = shapeStartRef.current;
              const toolShape = tool as "rect" | "ellipse" | "line" | "arrow";
              const draft = makeShapeStroke(toolShape, start, { x: nx, y: ny }, penColor, penWidth);
              shapeDraftRef.current = draft;
              setShapeDraft(draft);
              return;
            }
            const stroke = liveStrokeRef.current;
            if (!(event.buttons & 1)) return;
            if (stroke === null) return;
            // 合并指针事件:高频触控/笔输入下只保留每帧最终位置即可。
            const points =
              typeof event.nativeEvent.getCoalescedEvents === "function"
                ? event.nativeEvent.getCoalescedEvents()
                : [event.nativeEvent];
            for (const raw of points.length > 0 ? points : [event.nativeEvent]) {
              const px = Math.min(1, Math.max(0, (raw.clientX - rect.left) / rect.width));
              const py = Math.min(1, Math.max(0, (raw.clientY - rect.top) / rect.height));
              stroke.points.push({ x: px, y: py });
              inkPendingRef.current.push({ x: px, y: py });
            }
            // 合并到下一帧绘制:高频指针对同时只做一次真正的 stroke。
            if (inkRafRef.current === 0) {
              inkRafRef.current = requestAnimationFrame(flushInk);
            }
          },
          onPointerUp: () => {
            if (tool === "eraser") {
              onEraseEnd();
              return;
            }
            if (tool === "lasso") {
              if (moveStartRef.current !== null) {
                const delta = moveDeltaRef.current;
                const currentSelection = selectionRef.current;
                moveStartRef.current = null;
                moveDeltaRef.current = null;
                setMoveDelta(null);
                if (
                  delta !== null &&
                  (delta.dx !== 0 || delta.dy !== 0) &&
                  currentSelection !== null
                ) {
                  void onMoveInk(currentSelection, delta.dx, delta.dy);
                  clearLasso();
                }
                // 只是点了一下选区(没有位移):保留选区与操作条,不静默清空。
                return;
              }
              const drawing = lassoDrawingRef.current;
              lassoDrawingRef.current = null;
              if (drawing === null || drawing.length < 3) {
                clearLasso();
                return;
              }
              const selected = (layers.ink ? pageAnnotations.inks : []).filter((item) =>
                item.strokes.some((stroke) => strokeInPolygon(stroke, drawing)),
              );
              if (selected.length === 0) {
                clearLasso();
                return;
              }
              setLassoPoints(drawing);
              setSelection(selected.map((item) => ({ ...item })));
              return;
            }
            if (shapeStartRef.current !== null) {
              shapeStartRef.current = null;
              const draft = shapeDraftRef.current;
              shapeDraftRef.current = null;
              setShapeDraft(null);
              if (draft === null) return;
              const a = draft.points[0]!;
              const b = draft.points[draft.points.length - 1]!;
              if (Math.hypot(a.x - b.x, a.y - b.y) < 0.004) return;
              onCommitInk(pageNumber, [draft], width / scale, height / scale);
              return;
            }
            const stroke = liveStrokeRef.current;
            if (stroke === null) return;
            liveStrokeRef.current = null;
            if (inkRafRef.current !== 0) {
              cancelAnimationFrame(inkRafRef.current);
              inkRafRef.current = 0;
              flushInk();
            }
            inkPendingRef.current = [];
            inkPenRef.current = null;
            const canvas = inkCanvasRef.current;
            if (canvas !== null) {
              canvas.getContext("2d")?.clearRect(0, 0, canvas.width, canvas.height);
            }
            if (stroke.points.length === 1) {
              stroke.points.push({ ...stroke.points[0]! });
            }
            onCommitInk(pageNumber, [stroke], width / scale, height / scale);
          },
          onPointerCancel: () => {
            if (tool === "eraser") onEraseEnd();
            if (tool === "lasso") clearLasso();
            shapeStartRef.current = null;
            shapeDraftRef.current = null;
            setShapeDraft(null);
          },
        };

  const selectedById =
    selection === null
      ? new Map<string, { annotation: AnnotationView; strokes: InkStroke[] }>()
      : new Map(selection.map((item) => [item.annotation.id, item]));
  const selectionPreviewStrokes =
    selection === null
      ? []
      : selection.flatMap((item) =>
          moveDelta === null
            ? item.strokes
            : translateStrokes(item.strokes, moveDelta.dx, moveDelta.dy),
        );
  const selectionBox =
    selection === null ? null : selectionBoxPx(selectionPreviewStrokes, width, height);

  return (
    <section
      data-page={pageNumber}
      className={`pdf-page${pageText.lines.length === 0 ? " scan-page" : ""}`}
      aria-label={`第 ${pageNumber} 页`}
    >
      {useFallback || !pdfDoc ? (
        pageText.lines.length === 0 ? (
          <p className="pdf-no-text">本页无文本层(扫描或图像页)。</p>
        ) : (
          pageText.lines.map((line, i) => (
            <p key={i} className="pdf-line">
              {line.length === 0 ? "\u00A0" : line}
            </p>
          ))
        )
      ) : (
        <div
          className={`pdf-page-frame${tool !== "read" ? " inking" : ""}`}
          style={{ width: `${width}px`, height: `${height}px` }}
          ref={frameRef}
          onPointerDown={(event) => {
            const rect = frameRef.current?.getBoundingClientRect();
            if (rect === undefined) return;
            if (tool === "textbox") {
              event.stopPropagation();
              onAddTextBox(
                pageNumber,
                (event.clientX - rect.left) / rect.width,
                (event.clientY - rect.top) / rect.height,
                event.clientX,
                event.clientY,
              );
              return;
            }
            if (tool !== "note") return;
            event.stopPropagation();
            onAddSticky(
              pageNumber,
              (event.clientX - rect.left) / rect.width,
              (event.clientY - rect.top) / rect.height,
            );
          }}
        >
          <canvas ref={canvasRef} className="pdf-canvas" />
          <div ref={textLayerRef} className="textLayer" />
          <PageOcrBadge
            status={ocrStatus}
            onReprocess={onReprocessOcr}
            onRecognizePages={onRecognizeOcrPages}
            pageCount={pageCount}
            busy={ocrBusy}
            client={client}
            documentId={documentId}
            versionId={versionId}
            onCorrected={onOcrCorrected}
          />
          {/* 服务端文本标注(高亮/下划线/文字框命中区),按图层显隐 */}
          {pageAnnotations.marks
            .filter(({ annotation }) =>
              decodeTextBox(annotation.noteText) !== null ? layers.textbox : layers.highlight,
            )
            .map(({ annotation, rects, underline }) => {
            const palette = paletteOf(annotation.color);
            return rects.map((r, i) => (
              <div
                key={`${annotation.id}-${i}`}
                className={`pdf-mark${underline ? " underline-mark" : ""}${
                  tool === "read" ? " clickable" : ""
                }`}
                style={
                  underline
                    ? {
                        left: `${r.x * scale}px`,
                        top: `${(r.y + r.height * 0.86) * scale}px`,
                        width: `${r.width * scale}px`,
                        height: `${Math.max(1.5, r.height * scale * 0.08)}px`,
                        backgroundColor: palette.hex,
                      }
                    : {
                        left: `${r.x * scale}px`,
                        top: `${r.y * scale}px`,
                        width: `${r.width * scale}px`,
                        height: `${r.height * scale}px`,
                        backgroundColor: palette.fill,
                      }
                }
                title={`${palette.label}色标注`}
                role="button"
                tabIndex={tool === "read" ? 0 : -1}
                onPointerDown={(event) => {
                  if (tool !== "read") return;
                  event.stopPropagation();
                  onManageMark(annotation, event.clientX, event.clientY);
                }}
                onKeyDown={(event) => {
                  if (tool !== "read") return;
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
                    onManageMark(annotation, rect.left, rect.bottom);
                  }
                }}
              />
            ));
          })}
          {/* 服务端涂鸦笔迹(移动预览时按平移量渲染) */}
          {layers.ink
            ? pageAnnotations.inks.map(({ annotation, strokes }) => {
                const isSelected = selectedById.has(annotation.id);
                const preview =
                  isSelected && moveDelta !== null
                    ? translateStrokes(strokes, moveDelta.dx, moveDelta.dy)
                    : strokes;
                return (
                  <svg
                    key={annotation.id}
                    className={`pdf-ink${tool === "read" ? " clickable" : ""}${
                      isSelected ? " lasso-selected" : ""
                    }`}
                    width={width}
                    height={height}
                    viewBox={`0 0 ${width} ${height}`}
                    role={tool === "read" ? "button" : undefined}
                    aria-label={tool === "read" ? "手绘笔迹,按 Delete 删除" : undefined}
                    tabIndex={tool === "read" ? 0 : -1}
                    onPointerDown={(event) => {
                      if (tool !== "read") return;
                      event.stopPropagation();
                      onInkContext(annotation, event.clientX, event.clientY);
                    }}
                    onKeyDown={(event) => {
                      if (tool !== "read") return;
                      if (event.key === "Delete" || event.key === "Backspace") {
                        event.preventDefault();
                        const rect = (event.currentTarget as SVGSVGElement).getBoundingClientRect();
                        onInkContext(annotation, rect.right, rect.top);
                      }
                    }}
                  >
                    {inkStrokeElements(preview, width, height)}
                  </svg>
                );
              })
            : null}
          {layers.ink && pendingStrokes.length > 0 ? (
            <svg
              className="pdf-ink pending-ink"
              width={width}
              height={height}
              viewBox={`0 0 ${width} ${height}`}
              aria-hidden="true"
            >
              {inkStrokeElements(pendingStrokes, width, height)}
            </svg>
          ) : null}
          {/* 便签气泡(Acrobat 式) */}
          {layers.note
            ? pageAnnotations.stickies.map(({ annotation, rect }) => (
            <button
              key={annotation.id}
              type="button"
              className={`pdf-sticky${tool === "read" ? " clickable" : ""}`}
              style={{ left: `${rect.x * scale}px`, top: `${rect.y * scale}px` }}
              title={decodeStyleNote(annotation.noteText).note || "便签"}
              aria-label={`便签:${pageNumber} 页`}
              onPointerDown={(event) => {
                if (tool !== "read") return;
                event.stopPropagation();
                onOpenSticky(annotation);
              }}
            >
              <StickyNote className="w-3.5 h-3.5" aria-hidden />
            </button>
          ))
            : null}
          {/* 套索多边形与选区操作 */}
          {lassoPoints !== null && lassoPoints.length > 1 ? (
            <svg
              className="lasso-overlay"
              width={width}
              height={height}
              viewBox={`0 0 ${width} ${height}`}
              aria-hidden="true"
            >
              <polygon
                points={lassoPoints.map((p) => `${p.x * width},${p.y * height}`).join(" ")}
                fill="rgba(63, 131, 248, 0.08)"
                stroke="#3f83f8"
                strokeWidth={1.5}
                strokeDasharray="5 4"
              />
            </svg>
          ) : null}
          {selection !== null && selectionBox !== null ? (
            <>
              <div
                className="lasso-box"
                style={{
                  left: `${selectionBox.left}px`,
                  top: `${selectionBox.top}px`,
                  width: `${selectionBox.width}px`,
                  height: `${selectionBox.height}px`,
                }}
                aria-label="套索选区"
                role="group"
              />
              <div
                className="lasso-actions"
                role="toolbar"
                aria-label="套索操作"
                style={{
                  left: `${Math.max(6, Math.min(width - 236, selectionBox.left))}px`,
                  top: `${Math.max(4, selectionBox.top - 40)}px`,
                }}
                onPointerDown={(event) => event.stopPropagation()}
              >
                <span className="lasso-hint">
                  {selection.length} 条 · 拖动选区可移动
                </span>
                <button
                  type="button"
                  className="btn tiny danger"
                  onClick={() => {
                    const items = selection;
                    clearLasso();
                    onDeleteInk(items);
                  }}
                >
                  删除
                </button>
                <button type="button" className="btn tiny ghost" onClick={clearLasso}>
                  取消
                </button>
              </div>
            </>
          ) : null}
          {/* 几何形状拖拽预览 */}
          {shapeDraft !== null ? (
            <svg
              className="shape-preview"
              width={width}
              height={height}
              viewBox={`0 0 ${width} ${height}`}
              aria-hidden="true"
            >
              {inkStrokeElements([shapeDraft], width, height)}
            </svg>
          ) : null}
          {/* 活动笔迹画布(仅绘图工具激活时接收指针) */}
          <canvas
            ref={inkCanvasRef}
            className={`pdf-ink-canvas${
              tool === "pen" ||
              tool === "highlight" ||
              tool === "eraser" ||
              tool === "lasso" ||
              tool === "rect" ||
              tool === "ellipse" ||
              tool === "line" ||
              tool === "arrow"
                ? " active"
                : ""
            }${tool === "eraser" ? " tool-eraser" : ""}${
              tool === "lasso" ? " tool-lasso" : ""
            }`}
            {...inkPointerHandlers}
            onPointerLeave={() => setEraserRing(null)}
          />
          {/* 圆形橡皮:圈的大小就是实际命中半径,所见即所擦。 */}
          {tool === "eraser" && eraserRing !== null ? (
            <span
              className="eraser-ring"
              aria-hidden="true"
              style={{
                left: `${eraserRing.x * width}px`,
                top: `${eraserRing.y * height}px`,
                width: `${eraserRadius * 2 * width}px`,
                height: `${eraserRadius * 2 * width}px`,
              }}
            />
          ) : null}
        </div>
      )}
      <p className="pdf-page-label" aria-hidden="true">
        {pageNumber}
      </p>
    </section>
  );
});

/**
 * 页面列表:虚拟化窗口只决定"渲染哪些页",不重建其余页的子树。
 *
 * 关键点(实测):滚动时父视图每帧都会因为可见窗口变化而重渲;若列表本身
 * 在父组件里内联生成,每次都会换来新的 pageAnnotations 兜底对象与内联
 * 回调,所有可见页的 canvas 跟着重绘——3 秒滚动产生 18 个 60–166ms 长任务。
 * 这里用 memo + 属性引用稳定化,只有真正变化的页才重渲。
 */
const PdfPageList = memo(function PdfPageList({
  pages,
  layout,
  windowPages,
  pdfDoc,
  scale,
  tool,
  penColor,
  penWidth,
  eraserRadius,
  layers,
  annotationsByPage,
  optimisticByPage,
  commitInk,
  eraseInkAt,
  eraseStart,
  eraseEnd,
  moveInk,
  deleteInk,
  manageMark,
  addSticky,
  addTextBox,
  pageOcr,
  onReprocessOcr,
  onRecognizeOcrPages,
  pageCount,
  ocrBusy,
  client,
  documentId,
  versionId,
  onOcrCorrected,
}: {
  pages: PdfPageText[];
  layout: Array<{ top: number; height: number }>;
  windowPages: Set<number>;
  pdfDoc?: unknown;
  scale: number;
  tool: ReaderTool;
  penColor: string;
  penWidth: number;
  eraserRadius: number;
  layers: import("./readerTools").AnnotationLayers;
  annotationsByPage: Map<number, PageAnnotations>;
  optimisticByPage: Map<number, InkStroke[]>;
  commitInk: (
    pageNumber: number,
    strokes: InkStroke[],
    pageW: number,
    pageH: number,
  ) => void;
  eraseInkAt: (pageNumber: number, x: number, y: number, radius?: number) => void;
  eraseStart: () => void;
  eraseEnd: () => void;
  moveInk: (
    items: Array<{ annotation: AnnotationView; strokes: InkStroke[] }>,
    dx: number,
    dy: number,
  ) => void;
  deleteInk: (items: Array<{ annotation: AnnotationView; strokes: InkStroke[] }>) => void;
  manageMark: (annotation: AnnotationView, x: number, y: number) => void;
  addSticky: (pageNumber: number, nx: number, ny: number) => void;
  addTextBox: (
    pageNumber: number,
    nx: number,
    ny: number,
    screenX: number,
    screenY: number,
  ) => void;
  pageOcr?: Map<number, PageOcrStatusView>;
  onReprocessOcr?: () => void;
  onRecognizeOcrPages?: (pages: number[]) => void;
  pageCount?: number;
  ocrBusy?: boolean;
  client?: CommandClient;
  documentId?: string;
  versionId?: string;
  onOcrCorrected?: (statuses: PageOcrStatusView[]) => void;
}) {
  return (
    <>
      {pages.map((page) => {
        const seg = layout[page.pageNumber - 1];
        const placeholderHeight = seg?.height ?? estimatePageHeight(page);
        return windowPages.has(page.pageNumber) ? (
          <PdfSinglePage
            key={page.pageNumber}
            pageNumber={page.pageNumber}
            pageText={page}
            pdfDoc={pdfDoc}
            scale={scale}
            tool={tool}
            penColor={penColor}
            penWidth={penWidth}
            eraserRadius={eraserRadius}
            layers={layers}
            pageAnnotations={
              annotationsByPage.get(page.pageNumber) ?? EMPTY_PAGE_ANNOTATIONS
            }
            pendingStrokes={optimisticByPage.get(page.pageNumber) ?? EMPTY_STROKES}
            onCommitInk={commitInk}
            onEraseInkAt={eraseInkAt}
            onEraseStart={eraseStart}
            onEraseEnd={eraseEnd}
            onMoveInk={moveInk}
            onDeleteInk={deleteInk}
            onManageMark={manageMark}
            onInkContext={manageMark}
            onAddSticky={addSticky}
            onAddTextBox={addTextBox}
            onOpenSticky={openStickyFocus}
            ocrStatus={pageOcr?.get(page.pageNumber)}
            onReprocessOcr={onReprocessOcr}
            onRecognizeOcrPages={onRecognizeOcrPages}
            pageCount={pageCount}
            ocrBusy={ocrBusy}
            client={client}
            documentId={documentId}
            versionId={versionId}
            onOcrCorrected={onOcrCorrected}
          />
        ) : (
          <div
            key={page.pageNumber}
            data-page={page.pageNumber}
            className="pdf-page-placeholder"
            style={{ height: `${placeholderHeight}px` }}
            aria-hidden="true"
          />
        );
      })}
    </>
  );
});

const EMPTY_PAGE_ANNOTATIONS: PageAnnotations = { marks: [], inks: [], stickies: [] };
const EMPTY_STROKES: InkStroke[] = [];

/** 缩略图(懒加载):进入可视区才向 pdf.js 取页并绘制小 canvas。 */
function PdfThumbnail({
  pageNumber,
  pdfDoc,
  active,
  onSelect,
}: {
  pageNumber: number;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  pdfDoc?: any;
  active: boolean;
  onSelect: () => void;
}): ReactNode {
  const buttonRef = useRef<HTMLButtonElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [visible, setVisible] = useState(false);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    const el = buttonRef.current;
    if (el === null) return;
    if (typeof IntersectionObserver === "undefined") {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setVisible(true);
          observer.disconnect();
        }
      },
      { rootMargin: "120px 0px" },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!visible || !pdfDoc) return;
    let cancelled = false;
    void (async () => {
      try {
        const page = await pdfDoc.getPage(pageNumber);
        if (cancelled) return;
        const base = page.getViewport({ scale: 1 });
        const scale = Math.min(0.26, 150 / Math.max(1, base.width));
        const viewport = page.getViewport({ scale });
        const canvas = canvasRef.current;
        if (canvas === null) return;
        const dpr = window.devicePixelRatio || 1;
        canvas.width = Math.max(1, Math.floor(viewport.width * dpr));
        canvas.height = Math.max(1, Math.floor(viewport.height * dpr));
        const ctx = canvas.getContext("2d");
        if (ctx === null) return;
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        await page.render({ canvasContext: ctx, viewport }).promise;
      } catch {
        if (!cancelled) setFailed(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [visible, pdfDoc, pageNumber]);
  return (
    <button
      ref={buttonRef}
      type="button"
      className={`pdf-thumb${active ? " active" : ""}`}
      aria-label={`缩略图:第 ${pageNumber} 页`}
      aria-current={active ? "page" : undefined}
      onClick={onSelect}
    >
      {failed ? (
        <span className="pdf-thumb-failed">第 {pageNumber} 页</span>
      ) : (
        <canvas ref={canvasRef} className="pdf-thumb-canvas" />
      )}
      <span className="pdf-thumb-no">{pageNumber}</span>
    </button>
  );
}

/** 标注管理:按页列出笔迹/文本框,支持定位、统计与批量删除(确认)。 */
function PdfAnnotationManager({
  annotations,
  pages,
  onLocate,
  onDeleteNow,
  onClose,
}: {
  annotations: AnnotationView[];
  pages: PdfPageText[];
  onLocate: (pageNumber: number) => void;
  onDeleteNow: (items: AnnotationView[]) => Promise<void>;
  onClose: () => void;
}): ReactNode {
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const items = useMemo(() => {
    const out: Array<{
      annotation: AnnotationView;
      page: number;
      kind: "ink" | "textbox";
    }> = [];
    for (const annotation of annotations) {
      const locator = annotation.locator;
      if (locator === null || locator.kind !== "pdf") continue;
      if (decodeInk(annotation.noteText) !== null) {
        out.push({ annotation, page: locator.pageNumber, kind: "ink" });
      } else if (decodeTextBox(annotation.noteText) !== null) {
        out.push({ annotation, page: locator.pageNumber, kind: "textbox" });
      }
    }
    return out.sort(
      (a, b) => a.page - b.page || a.annotation.createdAt - b.annotation.createdAt,
    );
  }, [annotations]);
  const stats = useMemo(() => {
    const perPage = new Map<number, { ink: number; textbox: number }>();
    const colors = new Map<string, number>();
    for (const page of pages) perPage.set(page.pageNumber, { ink: 0, textbox: 0 });
    for (const item of items) {
      const bucket = perPage.get(item.page) ?? { ink: 0, textbox: 0 };
      bucket[item.kind] += 1;
      perPage.set(item.page, bucket);
      colors.set(item.annotation.color, (colors.get(item.annotation.color) ?? 0) + 1);
    }
    return { perPage, colors };
  }, [items, pages]);
  const grouped = useMemo(() => {
    const map = new Map<number, typeof items>();
    for (const item of items) {
      const list = map.get(item.page) ?? [];
      list.push(item);
      map.set(item.page, list);
    }
    return [...map.entries()].sort((a, b) => a[0] - b[0]);
  }, [items]);
  return (
    <aside className="pdf-manager" role="dialog" aria-label="标注管理">
      <header className="pdf-manager-head">
        <strong>标注管理</strong>
        <span className="pdf-manager-total">{items.length} 条</span>
        <button type="button" className="icon-btn" aria-label="关闭标注管理" onClick={onClose}>
          ×
        </button>
      </header>
      <p className="pdf-manager-stats" aria-label="标注统计">
        笔迹 {items.filter((item) => item.kind === "ink").length} · 文本框{" "}
        {items.filter((item) => item.kind === "textbox").length}
        {[...stats.colors.entries()].map(([color, count]) => (
          <span key={color} className="pdf-manager-stat">
            <span
              className="pdf-manager-dot"
              style={{ background: paletteOf(color as never).hex }}
            />
            {paletteOf(color as never).label}
            {count}
          </span>
        ))}
      </p>
      <div className="pdf-manager-list">
        {grouped.length === 0 ? (
          <p className="pdf-manager-empty">还没有笔迹或文本框。</p>
        ) : (
          grouped.map(([pageNumber, list]) => (
            <section key={pageNumber} className="pdf-manager-page">
              <h4>
                第 {pageNumber} 页
                <span>
                  笔迹 {(stats.perPage.get(pageNumber)?.ink ?? 0)} · 文本框{" "}
                  {stats.perPage.get(pageNumber)?.textbox ?? 0}
                </span>
              </h4>
              <ul>
                {list.map((item) => (
                  <li key={item.annotation.id}>
                    <input
                      type="checkbox"
                      checked={selected.has(item.annotation.id)}
                      onChange={() =>
                        setSelected((prev) => {
                          const next = new Set(prev);
                          if (next.has(item.annotation.id)) next.delete(item.annotation.id);
                          else next.add(item.annotation.id);
                          return next;
                        })
                      }
                      aria-label={`选择第 ${pageNumber} 页的${item.kind === "ink" ? "笔迹" : "文本框"}`}
                    />
                    <span className="pdf-manager-kind">
                      {item.kind === "ink" ? "笔迹" : "文本框"}
                    </span>
                    <span className="pdf-manager-color">
                      <span
                        className="pdf-manager-dot"
                        style={{ background: paletteOf(item.annotation.color).hex }}
                      />
                      {paletteOf(item.annotation.color).label}
                    </span>
                    <button
                      type="button"
                      className="btn tiny ghost"
                      onClick={() => onLocate(pageNumber)}
                    >
                      定位
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          ))
        )}
      </div>
      <footer className="pdf-manager-foot">
        <span>已选 {selected.size} 项</span>
        <button
          type="button"
          className="btn tiny danger"
          disabled={selected.size === 0}
          onClick={() => setConfirm(true)}
        >
          批量删除
        </button>
      </footer>
      <ConfirmDialog
        request={
          confirm
            ? {
                title: "批量删除标注",
                message: `确定删除选中的 ${selected.size} 条笔迹/文本框吗?删除后可用 Ctrl+Z 撤销笔迹。`,
                confirmLabel: "删除",
                onConfirm: async () => {
                  setBusy(true);
                  try {
                    const targets = items
                      .map((item) => item.annotation)
                      .filter((annotation) => selected.has(annotation.id));
                    await onDeleteNow(targets);
                    setSelected(new Set());
                  } finally {
                    setBusy(false);
                    setConfirm(false);
                  }
                },
              }
            : null
        }
        busy={busy}
        onCancel={() => {
          if (!busy) setConfirm(false);
        }}
      />
    </aside>
  );
}
const openStickyFocus = (annotation: AnnotationView) =>
  useReaderTools.getState().requestNotesFocus(annotation.id);

/**
 * PDF 页面主视图:自管滚动容器(虚拟化窗口与滚动事件同源)、精确占位高度、
 * 缩放/适应宽度/页码跳转、阅读主题、划词标注与笔记、涂鸦/橡皮/便签、
 * 书签大纲。
 */
export function PdfPagesView({
  pages,
  pdfDoc,
  initialPage = 1,
  onVisiblePageChange,
  documentId,
  versionId,
  client,
  documentTitle,
  pageOcr,
  onReprocessOcr,
  ocrBusy,
  pdfBytes,
}: {
  pages: PdfPageText[];
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  pdfDoc?: any;
  initialPage?: number;
  onVisiblePageChange?: (pageNumber: number) => void;
  documentId?: string;
  versionId?: string;
  client?: CommandClient;
  /** 文档名(复制为引用的出处)。 */
  documentTitle?: string;
  /** 2D 页级 OCR 状态(按页号)。 */
  pageOcr?: Map<number, PageOcrStatusView>;
  onReprocessOcr?: () => void;
  ocrBusy?: boolean;
  /** 原始 PDF 字节(指定页 OCR 用;null 表示尚未加载)。 */
  pdfBytes?: Uint8Array | null;
}): ReactNode {
  const scrollRef = useRef<HTMLDivElement>(null);
  const startPage = Math.min(Math.max(1, initialPage), Math.max(1, pages.length));
  const [visible, setVisible] = useState<[number, number]>([startPage, startPage]);
  // PDF 缩放与「适应宽度」按文档记忆(同一文档重开保持;无记忆回退 125%)。
  const [scale, setScale] = useState(() => loadPdfZoom(documentId)?.scale ?? 1.25);
  const [fitMode, setFitMode] = useState(() => loadPdfZoom(documentId)?.fitMode ?? false);
  const [viewMode, setViewMode] = useState<"continuous" | "single" | "two">("continuous");
  const [thumbsOpen, setThumbsOpen] = useState(false);
  const [managerOpen, setManagerOpen] = useState(false);
  const [printMode, setPrintMode] = useState(false);
  /** 指定页 OCR 对话框:非 null 时打开,页码为预填值。 */
  const [ocrDialogPages, setOcrDialogPages] = useState<number[] | null>(null);
  const zoomDocRef = useRef(documentId ?? null);
  const [theme, setTheme] = useState<ReaderTheme>(loadTheme);
  const [copied, setCopied] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [pageInput, setPageInput] = useState(String(startPage));
  const [popover, setPopover] = useState<SelectionToolbarState | null>(null);
  const [markMenu, setMarkMenu] = useState<{
    annotation: AnnotationView;
    x: number;
    y: number;
  } | null>(null);
  const [textBoxDraft, setTextBoxDraft] = useState<TextBoxEditorState | null>(null);
  const [outline, setOutline] = useState<PdfOutlineItem[] | null>(null);
  const [outlineOpen, setOutlineOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const [annotations, setAnnotations] = useState<AnnotationView[] | null>(null);
  const [optimisticInks, setOptimisticInks] = useState<
    Array<{ id: string; pageNumber: number; strokes: InkStroke[] }>
  >([]);
  const [confirmRequest, setConfirmRequest] = useState<ConfirmRequest | null>(null);
  const [confirmBusy, setConfirmBusy] = useState(false);
  const [layersOpen, setLayersOpen] = useState(false);
  const [, forceHistory] = useState(0);
  const bumpHistory = useCallback(() => forceHistory((tick) => tick + 1), []);
  const eraseGestureRef = useRef<InkGestureRecorder | null>(null);
  // 橡皮逐点擦除会连续改写同一条标注;用 ref 保证队列里每次都用最新 revision/笔迹。
  const annotationsRef = useRef<AnnotationView[] | null>(null);
  useEffect(() => {
    annotationsRef.current = annotations;
  }, [annotations]);
  const restoredRef = useRef(false);
  const queryClient = useContext(QueryClientContext);

  // 共享工具状态(跨格式保持;画笔/橡皮/便签)。
  const tool = useReaderTools((s) => s.tool);
  const penColor = useReaderTools((s) => s.penColor);
  const setTool = useReaderTools((s) => s.setTool);
  const toggleTool = useReaderTools((s) => s.toggleTool);
  const setPenColor = useReaderTools((s) => s.setPenColor);
  const penWidth = useReaderTools((s) => s.penWidth);
  const setPenWidth = useReaderTools((s) => s.setPenWidth);
  const eraserRadius = useReaderTools((s) => s.eraserRadius);
  const setEraserRadius = useReaderTools((s) => s.setEraserRadius);
  const layers = useReaderTools((s) => s.layers);
  const setLayer = useReaderTools((s) => s.setLayer);
  useInkHistoryVersion(documentId);
  // 翻译:单句(划词)、整页(当前页译文面板)、整篇(产物任务)共用目标语言。
  const inlineTranslate = useReaderTools((s) => s.inlineTranslate);
  const setInlineTranslate = useReaderTools((s) => s.setInlineTranslate);
  const inlineTarget = useReaderTools((s) => s.inlineTranslateTarget);
  const setInlineTarget = useReaderTools((s) => s.setInlineTranslateTarget);
  const [docTranslateBusy, setDocTranslateBusy] = useState(false);
  /** 阅读区上方的翻译范围:整页(当前页译文面板)/ 整篇(生成译文产物)。 */
  const [translateScope, setTranslateScope] = useState<"page" | "doc">("page");

  const showToast = useCallback((text: string, durationMs = 1800) => {
    setToast(text);
    setTimeout(() => setToast(null), durationMs);
  }, []);
  // 冲突决策结果要可见;对话框必须挂到阅读器根节点(portal 到文档层)。
  const annotationConflict = useAnnotationConflict(client, documentId, showToast);

  const onOcrCorrected = useCallback(
    (_statuses: PageOcrStatusView[]) => {
      if (documentId !== null && documentId !== undefined) {
        void queryClient?.invalidateQueries({ queryKey: ["parse", "status", documentId] });
      }
      if (versionId !== null && versionId !== undefined) {
        void queryClient?.invalidateQueries({ queryKey: ["page-ocr", versionId] });
      }
      // 不失效 ["reader","render-plan"]:OCR 文本不改变 PDF 渲染,
      // 重取渲染计划会在解析进行中触发 10.9.1 竞态,导致阅读区短暂报错/徽标消失。
      showToast("OCR 文本已保存,本页索引已重建");
    },
    [documentId, versionId, queryClient, showToast],
  );

  // 按文档记忆画笔/橡皮;切文档恢复该文档上次的设置。
  useEffect(() => {
    const saved = loadDocumentPen(documentId);
    if (saved?.penColor !== undefined) setPenColor(saved.penColor);
    if (saved?.penWidth !== undefined) setPenWidth(saved.penWidth);
    if (saved?.eraserRadius !== undefined) setEraserRadius(saved.eraserRadius);
  }, [documentId, setEraserRadius, setPenColor, setPenWidth]);
  useEffect(() => {
    saveDocumentPen(documentId, { penColor, penWidth, eraserRadius });
  }, [documentId, penColor, penWidth, eraserRadius]);

  // 切文档清空撤销栈(按文档隔离,不跨文档撤销)。
  useEffect(() => () => clearInkHistory(documentId), [documentId]);

  useInkHistoryHotkeys({
    documentId,
    containerRef: scrollRef,
    client,
    onApplied: (entry, direction) => {
      bumpHistory();
      showToast(`${direction === "undo" ? "已撤销" : "已重做"}:${entry.label}`);
    },
    onError: showToast,
  });

  const undoNow = useCallback(() => {
    if (!client || !documentId) return;
    void undoInk(client, documentId)
      .then((entry) => {
        if (entry !== null) {
          bumpHistory();
          showToast(`已撤销:${entry.label}`);
        }
      })
      .catch(() => showToast("撤销未完成,请重试"));
  }, [bumpHistory, client, documentId, showToast]);
  const redoNow = useCallback(() => {
    if (!client || !documentId) return;
    void redoInk(client, documentId)
      .then((entry) => {
        if (entry !== null) {
          bumpHistory();
          showToast(`已重做:${entry.label}`);
        }
      })
      .catch(() => showToast("重做未完成,请重试"));
  }, [bumpHistory, client, documentId, showToast]);

  // 缩放记忆:写入当前文档(节流由状态变更频率天然约束);换文档时恢复其记忆。
  useEffect(() => {
    savePdfZoom(documentId, { scale, fitMode });
  }, [documentId, scale, fitMode]);

  useEffect(() => {
    const nextDoc = documentId ?? null;
    if (zoomDocRef.current === nextDoc) return;
    zoomDocRef.current = nextDoc;
    const saved = loadPdfZoom(nextDoc);
    setScale(saved?.scale ?? 1.25);
    setFitMode(saved?.fitMode ?? false);
  }, [documentId]);

  // 书签大纲:pdf.js outline → 标题+页码。
  useEffect(() => {
    let cancelled = false;
    void resolveOutline(pdfDoc).then((items) => {
      if (!cancelled) setOutline(items);
    });
    return () => {
      cancelled = true;
    };
  }, [pdfDoc]);

  // 标注列表:挂载与变更事件时拉取(本地视图自管,不依赖上层 Provider)。
  //
  // 关键:变更事件带回增量时直接更新本地列表,不再重拉整份 annotation_list。
  // 旧实现每画一笔都广播一次 → 本视图、笔记面板、Markdown 标注层各自重拉,
  // 实测 20 笔草图发出 100 次 IPC(其中 60 次是 annotation_list,下载 537KB)。
  // 无 detail 的旧广播仍重拉,保留兼容路径。
  const reloadAnnotations = useCallback(async () => {
    if (!client || !documentId) return;
    try {
      const list = await client.call<AnnotationView[]>("annotation_list", { documentId });
      setAnnotations(list);
    } catch {
      setAnnotations([]);
    }
  }, [client, documentId]);

  useEffect(() => {
    void reloadAnnotations();
    const onInvalidate = (event: Event) => {
      const detail = (event as CustomEvent<AnnotationChangeDetail | null | undefined>).detail;
      if (detail == null) {
        void reloadAnnotations();
        return;
      }
      setAnnotations((previous) => {
        const list = previous ?? [];
        if (detail.type === "created") {
          return [detail.annotation, ...list.filter((item) => item.id !== detail.annotation.id)];
        }
        if (detail.type === "updated") {
          return list.map((item) =>
            item.id === detail.annotation.id ? detail.annotation : item,
          );
        }
        return list.filter((item) => item.id !== detail.annotationId);
      });
    };
    window.addEventListener("knowwise-annotations-changed", onInvalidate);
    return () => window.removeEventListener("knowwise-annotations-changed", onInvalidate);
  }, [reloadAnnotations]);

  const invalidateAnnotations = useCallback(
    (detail?: import("./annotationEvents").AnnotationChangeDetail) => {
      broadcastAnnotationChange(detail);
      // 有增量 detail 时由各视图就地合并,不再让 query 失效触发全量重拉;
      // 无 detail 的旧调用仍走缓存失效(数据一致性优先)。
      if (detail === undefined && client && documentId && queryClient) {
        void queryClient.invalidateQueries({ queryKey: qk.documentAnnotations(documentId) });
      }
    },
    [client, documentId, queryClient],
  );

  const annotationsByPage = useMemo(() => groupAnnotationsByPage(annotations), [annotations]);
  const optimisticByPage = useMemo(() => {
    const map = new Map<number, InkStroke[]>();
    for (const entry of optimisticInks) {
      const list = map.get(entry.pageNumber) ?? [];
      list.push(...entry.strokes);
      map.set(entry.pageNumber, list);
    }
    return map;
  }, [optimisticInks]);

  // 布局:每页渲染高度(含页脚标号)与累计偏移;占位与滚动换算共用。
  const PAGE_GAP = 18;
  const layout = useMemo(() => {
    const pageHeight = (index: number): number => {
      const page = pages[index]!;
      return page.height != null ? page.height * scale + 26 : estimatePageHeight(page) * scale;
    };
    const segments: Array<{ top: number; height: number }> = new Array(pages.length);
    if (viewMode === "single") {
      // 单页模式只有当前页参与滚动换算,避免页内滚动误切页。
      for (let i = 0; i < pages.length; i += 1) {
        segments[i] = { top: 0, height: i === visible[0] - 1 ? pageHeight(i) : 0 };
      }
      return segments;
    }
    if (viewMode === "two") {
      // 封面单独一页:第 1 页独占一行,之后每行两页(2-3、4-5…)。
      let top = 0;
      let index = 0;
      while (index < pages.length) {
        const row = index === 0 ? [0] : [index, index + 1].filter((i) => i < pages.length);
        const height = Math.max(...row.map(pageHeight)) + PAGE_GAP;
        for (const i of row) segments[i] = { top, height };
        top += height;
        index += row.length;
      }
      return segments;
    }
    let top = 0;
    for (let i = 0; i < pages.length; i += 1) {
      const height = pageHeight(i);
      segments[i] = { top, height };
      top += height + PAGE_GAP;
    }
    return segments;
  }, [pages, scale, viewMode, visible]);

  // 滚动 → 可见窗口(事件挂在真正的滚动容器上)。
  useEffect(() => {
    const el = scrollRef.current;
    if (el === null) return;
    let raf = 0;
    const update = () => {
      raf = 0;
      const top = el.scrollTop;
      const bottom = top + el.clientHeight;
      let first: number | null = null;
      let last: number | null = null;
      for (let i = 0; i < layout.length; i += 1) {
        const seg = layout[i]!;
        if (seg.top + seg.height > top + 4 && seg.top < bottom - 4) {
          if (first === null) first = i + 1;
          last = i + 1;
        }
      }
      if (first !== null && last !== null) {
        const f = first;
        const l = last;
        setVisible((prev) => (prev[0] === f && prev[1] === l ? prev : [f, l]));
      }
    };
    const onScroll = () => {
      if (raf === 0) raf = requestAnimationFrame(update);
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    update();
    return () => {
      el.removeEventListener("scroll", onScroll);
      if (raf !== 0) cancelAnimationFrame(raf);
    };
  }, [layout]);

  const notifiedRef = useRef(-1);
  useEffect(() => {
    if (onVisiblePageChange && visible[0] !== notifiedRef.current) {
      notifiedRef.current = visible[0];
      onVisiblePageChange(visible[0]);
      setPageInput(String(visible[0]));
    }
  }, [visible, onVisiblePageChange]);

  const scrollToPage = useCallback(
    (target: number, smooth = false) => {
      const el = scrollRef.current;
      const clamped = Math.min(Math.max(1, target), Math.max(1, pages.length));
      if (el === null) return;
      // 单页模式没有长滚动内容:只切换当前页。
      if (viewMode === "single") {
        setVisible([clamped, clamped]);
        setPageInput(String(clamped));
        return;
      }
      requestAnimationFrame(() => {
        const seg = layout[clamped - 1];
        if (seg !== undefined) {
          // 双页行首与上一行共享边界;再减偏移会让上一行仍被判为可见。
          const top = Math.max(0, viewMode === "two" ? seg.top : seg.top - 8);
          if (smooth && !prefersReducedMotion() && typeof el.scrollTo === "function") {
            el.scrollTo({ top, behavior: "smooth" });
          } else {
            el.scrollTop = top;
          }
        }
      });
      setVisible([clamped, clamped]);
      setPageInput(String(clamped));
    },
    [layout, pages.length, viewMode],
  );

  // 恢复:首帧滚动到保存页。
  useEffect(() => {
    if (restoredRef.current || pages.length === 0) return;
    restoredRef.current = true;
    scrollToPage(startPage);
  }, [pages.length, startPage, scrollToPage]);

  // 引用跳转(citation 回跳)。
  useEffect(() => {
    const onGoto = (event: Event) => {
      const detail = (
        event as CustomEvent<{ documentId?: string; locator?: { kind: string; pageNumber?: number } }>
      ).detail;
      if (detail?.locator?.kind !== "pdf" || detail.locator.pageNumber === undefined) return;
      scrollToPage(Math.min(Math.max(1, detail.locator.pageNumber), Math.max(1, pages.length)), true);
    };
    window.addEventListener("knowwise-goto", onGoto);
    return () => window.removeEventListener("knowwise-goto", onGoto);
  }, [pages.length, scrollToPage]);

  // Ctrl+F 全局唤起。
  useEffect(() => {
    const onOpenSearch = () => setSearchOpen(true);
    window.addEventListener("knowwise-open-search", onOpenSearch);
    return () => window.removeEventListener("knowwise-open-search", onOpenSearch);
  }, []);

  // 划词:MouseUp 捕获选区并定位浮动菜单。
  const popoverPageRef = useRef(1);

  const handleMouseUp = useCallback(() => {
    if (tool !== "read") {
      setPopover(null);
      return;
    }
    const sel = typeof window !== "undefined" ? window.getSelection() : null;
    if (!sel || sel.isCollapsed || sel.rangeCount === 0 || !sel.toString().trim()) {
      setPopover(null);
      return;
    }
    const text = sel.toString().trim();
    try {
      const range = sel.getRangeAt(0);
      const rect = range.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) return;
      let pageNumber = visible[0];
      let parent: HTMLElement | null = range.commonAncestorContainer as HTMLElement | null;
      while (parent && parent !== scrollRef.current) {
        const pageAttr = parent.getAttribute?.("data-page");
        if (pageAttr) {
          pageNumber = parseInt(pageAttr, 10);
          break;
        }
        parent = parent.parentElement;
      }
      setPopover({
        text,
        x: Math.min(Math.max(rect.left + rect.width / 2, 190), window.innerWidth - 190),
        y: Math.max(rect.top, 64),
      });
      popoverPageRef.current = pageNumber;
    } catch {
      setPopover(null);
    }
  }, [tool, visible]);

  // 划词高亮/下划线/笔记:client rects → user space rects 存入锚点。
  const annotateSelection = useCallback(
    async (
      color: AnnotationColor,
      style: "highlight" | "underline",
      openNote = false,
    ) => {
      const sel = typeof window === "undefined" ? null : window.getSelection();
      if (!client || !documentId || !versionId || sel === null || sel.rangeCount === 0) return;
      const text = sel.toString().trim();
      if (text.length === 0) return;
      const pageNumber = popoverPageRef.current;
      const frame = scrollRef.current?.querySelector(
        `section[data-page="${pageNumber}"] .pdf-page-frame`,
      );
      let rects: Array<{ x: number; y: number; width: number; height: number }> = [];
      if (frame instanceof HTMLElement) {
        rects = clientRectsToPdfRects(sel.getRangeAt(0).getClientRects(), frame, scale);
      }
      setPopover(null);
      window.getSelection()?.removeAllRanges();
      try {
        const created = await createAnnotatedSelection(client, {
          documentId,
          versionId,
          format: "pdf",
          text,
          primaryLocator: {
            kind: "pdf",
            pageNumber,
            rects,
            pageRotation: "0",
            coordinateSpace: "pdf_user_space_v1",
          },
          color,
          style,
        });
        showToast(openNote ? "已创建笔记,在右侧开始书写" : style === "underline" ? "已加下划线" : "已高亮");
        invalidateAnnotations({ type: "created", annotation: created });
        if (openNote) useReaderTools.getState().requestNotesFocus(created.id);
      } catch {
        showToast("标注未保存,请重试");
      }
    },
    [client, documentId, versionId, scale, showToast, invalidateAnnotations],
  );

  // 便签:点按页面位置插入(Acrobat 式),气泡渲染在页面上。
  /** 添加文字注释:只打开编辑器,保存时才写库(取消不留空注释)。 */
  const addTextBox = useCallback(
    (pageNumber: number, nx: number, ny: number, screenX: number, screenY: number) => {
      if (!client || !documentId || !versionId) return;
      const page = pages.find((p) => p.pageNumber === pageNumber);
      const w = page?.width ?? 612;
      const h = page?.height ?? 792;
      setTextBoxDraft({
        annotation: null,
        target: {
          x: nx,
          y: ny,
          anchorText: "",
          locator: {
            kind: "pdf",
            pageNumber,
            rects: [{ x: nx * w, y: ny * h, width: 1, height: 1 }],
            pageRotation: "0",
            coordinateSpace: "pdf_user_space_v1",
          },
        },
        text: "",
        style: { ...DEFAULT_TEXTBOX_STYLE },
        screenX: Math.min(screenX, window.innerWidth - 340),
        screenY: Math.min(screenY, window.innerHeight - 220),
      });
      // 画完一次就回到阅读态,避免误点继续插框。
      setTool("read");
    },
    [client, documentId, pages, setTool, versionId],
  );

  const addSticky = useCallback(
    async (pageNumber: number, nx: number, ny: number) => {
      if (!client || !documentId || !versionId) return;
      const page = pages.find((p) => p.pageNumber === pageNumber);
      const w = page?.width ?? 612;
      const h = page?.height ?? 792;
      const rect = {
        x: Math.max(0, nx * w - 12),
        y: Math.max(0, ny * h - 12),
        width: 24,
        height: 24,
      };
      setTool("read");
      try {
        const created = await createAnnotatedSelection(client, {
          documentId,
          versionId,
          format: "pdf",
          text: "便签",
          primaryLocator: {
            kind: "pdf",
            pageNumber,
            rects: [rect],
            pageRotation: "0",
            coordinateSpace: "pdf_user_space_v1",
          },
          color: "yellow",
          style: "highlight",
          selectionKind: "page",
        });
        invalidateAnnotations({ type: "created", annotation: created });
        useReaderTools.getState().requestNotesFocus(created.id);
      } catch {
        showToast("便签未创建,请重试");
      }
    },
    [client, documentId, versionId, pages, setTool, showToast, invalidateAnnotations],
  );

  // 涂鸦:保存为带笔迹 noteText 的标注(紧凑编码 + 字符预算)。
  const commitInkNow = useCallback(
    async (pageNumber: number, strokes: InkStroke[], pageW: number, pageH: number) => {
      if (!client || !documentId || !versionId) return;
      try {
        const rawSelection: RawSelectionInput = {
          documentId,
          documentVersionId: versionId,
          format: "pdf",
          text: `手绘标注(第 ${pageNumber} 页)`,
          blocks: [
            {
              renderKey: `ink-${pageNumber}-${Date.now()}`,
              text: `手绘标注(第 ${pageNumber} 页)`,
              blockOrder: 0,
              locator: {
                kind: "pdf",
                pageNumber,
                rects: [inkBoundsToPdfRect(strokes, pageW, pageH)],
                pageRotation: "0",
                coordinateSpace: "pdf_user_space_v1",
              },
            },
          ],
          primaryLocator: {
            kind: "pdf",
            pageNumber,
            rects: [inkBoundsToPdfRect(strokes, pageW, pageH)],
            pageRotation: "0",
            coordinateSpace: "pdf_user_space_v1",
          },
          selectionKind: "page",
        };
        const anchor = await client.call<AnchorView>("evidence_create_anchor", {
          rawSelection,
          idempotencyKey: client.newIdempotencyKey(),
        });
        const encoded = encodeInk(strokes);
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
        bumpHistory();
        showToast("笔迹已保存");
        invalidateAnnotations({ type: "created", annotation: created });
      } catch {
        showToast("笔迹未保存,请重试");
      }
    },
    [client, documentId, versionId, showToast, invalidateAnnotations, bumpHistory],
  );

  // 乐观显示 + 串行写库:落笔先上屏,anchor/annotation 创建按顺序排队,
  // 避免连续快速落笔并发写同一文档造成乱序。
  const inkCommitQueueRef = useRef<Promise<void>>(Promise.resolve());
  const commitInk = useCallback(
    (pageNumber: number, strokes: InkStroke[], pageW: number, pageH: number) => {
      const id = `optimistic-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      setOptimisticInks((prev) => [...prev, { id, pageNumber, strokes }]);
      const task = inkCommitQueueRef.current
        .catch(() => undefined)
        .then(() => commitInkNow(pageNumber, strokes, pageW, pageH))
        .finally(() => {
          setOptimisticInks((prev) => prev.filter((entry) => entry.id !== id));
        });
      inkCommitQueueRef.current = task;
      return task;
    },
    [commitInkNow],
  );

  // 橡皮:圆形范围内逐点擦除(不是点一下删整条)。
  // 一次拖动会高频触发,用"最后一个位置 + 串行队列"避免并发写同一标注;
  // 每次都用 ref 里的最新 revision,避免 conflict。
  const eraseOnce = useCallback(
    async (job: { pageNumber: number; x: number; y: number; radius: number }) => {
      if (!client) return;
      const bucket = groupAnnotationsByPage(annotationsRef.current ?? []).get(job.pageNumber);
      if (!bucket) return;
      const page = pages.find((p) => p.pageNumber === job.pageNumber);
      const pageW = page?.width ?? 612;
      const pageH = page?.height ?? 792;
      const hitPx = Math.max(3, job.radius * pageW);
      const hit = (p: { x: number; y: number }) =>
        Math.hypot((p.x - job.x) * pageW, (p.y - job.y) * pageH) <= hitPx;
      for (const { annotation, strokes } of bucket.inks) {
        const next = eraseInkCircle(strokes, hit);
        if (next === null) continue;
        const before = {
          anchorId: annotation.anchorId,
          color: annotation.color,
          noteText: annotation.noteText,
        };
        try {
          if (next.length === 0) {
            await client.call("annotation_delete", {
              annotationId: annotation.id,
              expectedRevision: annotation.revision,
            });
            const remaining = (annotationsRef.current ?? []).filter(
              (item) => item.id !== annotation.id,
            );
            annotationsRef.current = remaining;
            setAnnotations(remaining);
            invalidateAnnotations({ type: "deleted", annotationId: annotation.id });
            eraseGestureRef.current?.record({
              anchorId: annotation.anchorId,
              before,
              after: null,
            });
          } else {
            const encoded = encodeInk(next);
            const updated = await client.call<AnnotationView>("annotation_update", {
              annotationId: annotation.id,
              color: annotation.color,
              noteText: encoded,
              expectedRevision: annotation.revision,
            });
            const nextList = (annotationsRef.current ?? []).map((item) =>
              item.id === updated.id ? updated : item,
            );
            annotationsRef.current = nextList;
            setAnnotations(nextList);
            invalidateAnnotations({ type: "updated", annotation: updated });
            eraseGestureRef.current?.record({
              anchorId: annotation.anchorId,
              before,
              after: {
                anchorId: annotation.anchorId,
                color: annotation.color,
                noteText: encoded,
              },
            });
          }
        } catch (error) {
          if (isRevisionConflict(error)) {
            annotationConflict.open(
              {
                anchorId: annotation.anchorId,
                annotationId: annotation.id,
                color: annotation.color,
                noteText: next.length === 0 ? null : encodeInk(next),
              },
              "擦除时该标注已被其他操作更新(修订冲突)。请选择如何处理。",
            );
          } else {
            showToast("擦除未保存,请重试");
          }
        }
      }
    },
    [client, invalidateAnnotations, pages, showToast],
  );
  const eraseOnceRef = useRef(eraseOnce);
  eraseOnceRef.current = eraseOnce;
  const eraseJobRef = useRef<{ pageNumber: number; x: number; y: number; radius: number } | null>(
    null,
  );
  const eraseRunningRef = useRef(false);
  const eraseInkAt = useCallback(
    (pageNumber: number, x: number, y: number, radius = 0.02) => {
      eraseJobRef.current = { pageNumber, x, y, radius };
      if (eraseRunningRef.current) return;
      eraseRunningRef.current = true;
      void (async () => {
        try {
          while (eraseJobRef.current !== null) {
            const job = eraseJobRef.current;
            eraseJobRef.current = null;
            await eraseOnceRef.current(job);
          }
        } finally {
          eraseRunningRef.current = false;
        }
      })();
    },
    [],
  );

  // 一次橡皮拖动 = 一条撤销记录:等串行写队列真正空闲后再入栈。
  const eraseEndRequestedRef = useRef(false);
  const beginEraseGesture = useCallback(() => {
    const previous = eraseGestureRef.current;
    if (previous !== null && documentId !== undefined && previous.commit(documentId) > 0) {
      bumpHistory();
    }
    eraseGestureRef.current = new InkGestureRecorder("橡皮擦除");
    eraseEndRequestedRef.current = false;
  }, [bumpHistory, documentId]);
  const endEraseGesture = useCallback(() => {
    eraseEndRequestedRef.current = true;
    const settle = () => {
      if (eraseRunningRef.current || eraseJobRef.current !== null) {
        window.setTimeout(settle, 60);
        return;
      }
      const recorder = eraseGestureRef.current;
      eraseGestureRef.current = null;
      if (recorder !== null && documentId !== undefined && recorder.commit(documentId) > 0) {
        bumpHistory();
      }
    };
    settle();
  }, [bumpHistory, documentId]);

  const moveInk = useCallback(
    async (
      items: Array<{ annotation: AnnotationView; strokes: InkStroke[] }>,
      dx: number,
      dy: number,
    ) => {
      if (!client || !documentId) return;
      const changes = [];
      for (const item of items) {
        // 选区快照可能落后于库(上一次移动/擦除刚写入);始终用最新 revision,
        // 避免把正常连续操作误判成冲突。
        const latest =
          (annotationsRef.current ?? []).find((entry) => entry.id === item.annotation.id) ??
          item.annotation;
        const encoded = encodeInk(translateStrokes(item.strokes, dx, dy));
        if (encoded === latest.noteText) continue;
        try {
          const updated = await client.call<AnnotationView>("annotation_update", {
            annotationId: latest.id,
            color: latest.color,
            noteText: encoded,
            expectedRevision: latest.revision,
          });
          changes.push({
            anchorId: latest.anchorId,
            before: {
              anchorId: latest.anchorId,
              color: latest.color,
              noteText: latest.noteText,
            },
            after: {
              anchorId: latest.anchorId,
              color: latest.color,
              noteText: encoded,
            },
          });
          invalidateAnnotations({ type: "updated", annotation: updated });
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
            showToast("移动未保存,请重试");
          }
        }
      }
      if (changes.length > 0) {
        pushInkHistory(documentId, "移动笔迹", changes);
        bumpHistory();
        showToast(`已移动 ${changes.length} 条笔迹`);
      }
    },
    [client, documentId, invalidateAnnotations, showToast, bumpHistory],
  );

  const deleteInkNow = useCallback(
    async (items: Array<{ annotation: AnnotationView; strokes: InkStroke[] }>) => {
      if (!client || !documentId) return;
      const changes = [];
      for (const item of items) {
        const annotation = (annotationsRef.current ?? []).find(
          (entry) => entry.id === item.annotation.id,
        ) ?? item.annotation;
        try {
          await client.call("annotation_delete", {
            annotationId: annotation.id,
            expectedRevision: annotation.revision,
          });
          changes.push({
            anchorId: annotation.anchorId,
            before: {
              anchorId: annotation.anchorId,
              color: annotation.color,
              noteText: annotation.noteText,
            },
            after: null,
          });
          invalidateAnnotations({ type: "deleted", annotationId: annotation.id });
        } catch (error) {
          if (isRevisionConflict(error)) {
            annotationConflict.open(
              {
                anchorId: annotation.anchorId,
                annotationId: annotation.id,
                color: annotation.color,
                noteText: null,
              },
              "删除时该标注已被其他操作更新(修订冲突)。请选择如何处理。",
            );
          } else {
            showToast("删除笔迹未完成,请重试");
          }
        }
      }
      if (changes.length > 0) {
        pushInkHistory(documentId, "删除笔迹", changes);
        bumpHistory();
        showToast(`已删除 ${changes.length} 条笔迹,可用 Ctrl+Z 撤销`);
      }
    },
    [client, documentId, invalidateAnnotations, showToast, bumpHistory],
  );

  const requestDeleteInk = useCallback(
    (items: Array<{ annotation: AnnotationView; strokes: InkStroke[] }>) => {
      setConfirmRequest({
        title: "删除笔迹",
        message: `确定删除选中的 ${items.length} 条笔迹/图形吗?删除后可用 Ctrl+Z 撤销。`,
        confirmLabel: "删除",
        onConfirm: async () => {
          setConfirmBusy(true);
          try {
            await deleteInkNow(items);
          } finally {
            setConfirmBusy(false);
            setConfirmRequest(null);
          }
        },
      });
    },
    [deleteInkNow],
  );

  /** 标注管理面板的批量删除(确认已在面板内完成)。 */
  const deleteManagerItems = useCallback(
    async (items: AnnotationView[]) => {
      if (!client || !documentId) return;
      const changes = [];
      let deleted = 0;
      for (const target of items) {
        const annotation = (annotationsRef.current ?? []).find(
          (entry) => entry.id === target.id,
        ) ?? target;
        try {
          await client.call("annotation_delete", {
            annotationId: annotation.id,
            expectedRevision: annotation.revision,
          });
          deleted += 1;
          if (decodeInk(annotation.noteText) !== null) {
            changes.push({
              anchorId: annotation.anchorId,
              before: {
                anchorId: annotation.anchorId,
                color: annotation.color,
                noteText: annotation.noteText,
              },
              after: null,
            });
          }
          invalidateAnnotations({ type: "deleted", annotationId: annotation.id });
        } catch (error) {
          if (isRevisionConflict(error)) {
            annotationConflict.open(
              {
                anchorId: annotation.anchorId,
                annotationId: annotation.id,
                color: annotation.color,
                noteText: null,
              },
              "批量删除时该标注已被其他操作更新(修订冲突)。请选择如何处理。",
            );
          } else {
            showToast("部分标注删除失败,请重试");
          }
        }
      }
      if (changes.length > 0) {
        pushInkHistory(documentId, "批量删除笔迹", changes);
        bumpHistory();
      }
      showToast(`已删除 ${deleted} 条标注`);
    },
    [bumpHistory, client, documentId, invalidateAnnotations, showToast],
  );

  const updateMark = useCallback(
    async (
      annotation: AnnotationView,
      patch: { color?: AnnotationColor; underline?: boolean; note?: string },
    ) => {
      if (!client) return;
      const current = decodeStyleNote(annotation.noteText);
      const isStylePayload =
        !annotation.noteText.startsWith(INK_PREFIX) &&
        decodeTextBox(annotation.noteText) === null;
      const nextColor = patch.color ?? annotation.color;
      const nextStyle =
        patch.underline !== undefined
          ? patch.underline
            ? "underline"
            : "highlight"
          : current.style;
      // 备注写入独立字段;样式类标注的旧备注从 noteText 清掉、样式前缀保留;
      // 涂鸦/文本框载荷原样保存,绝不按普通文本重写。
      const nextNoteText = isStylePayload
        ? encodeStyleNote(nextStyle, patch.note !== undefined ? "" : current.note)
        : annotation.noteText;
      setMarkMenu(null);
      try {
        const updated = await client.call<AnnotationView>("annotation_update", {
          annotationId: annotation.id,
          color: nextColor,
          noteText: nextNoteText,
          ...(patch.note !== undefined ? { remark: patch.note } : {}),
          expectedRevision: annotation.revision,
        });
        // 必须用服务端返回的新 revision 更新本地:拿旧 revision 做下一次
        // 改色/下划线/备注会把正常连续编辑误判成冲突。
        invalidateAnnotations({ type: "updated", annotation: updated });
      } catch (error) {
        if (isRevisionConflict(error)) {
          annotationConflict.open(
            {
              anchorId: annotation.anchorId,
              annotationId: annotation.id,
              color: nextColor,
              noteText: nextNoteText,
              ...(patch.note !== undefined ? { remark: patch.note } : {}),
            },
            "这条标注已被其他操作修改(修订冲突)。请选择如何处理。",
          );
        } else {
          showToast("修改未保存,请重试");
        }
      }
    },
    [client, invalidateAnnotations, showToast],
  );

  const deleteMark = useCallback(
    (annotation: AnnotationView) => {
      if (!client) return;
      setMarkMenu(null);
      const kindLabel =
        decodeInk(annotation.noteText) !== null
          ? "笔迹"
          : decodeTextBox(annotation.noteText) !== null
            ? "文字注释"
            : annotation.quoteText === "便签"
              ? "便签"
              : decodeStyleNote(annotation.noteText).style === "underline"
                ? "下划线"
                : "高亮";
      setConfirmRequest({
        title: `删除${kindLabel}`,
        message: `确定删除这条${kindLabel}吗?${
          decodeInk(annotation.noteText) !== null ? "删除后可用 Ctrl+Z 撤销。" : "删除后不可恢复。"
        }`,
        confirmLabel: "删除",
        onConfirm: async () => {
          setConfirmBusy(true);
          try {
            await client.call("annotation_delete", {
              annotationId: annotation.id,
              expectedRevision: annotation.revision,
            });
            if (decodeInk(annotation.noteText) !== null && documentId !== undefined) {
              pushInkHistory(documentId, `删除${kindLabel}`, [
                {
                  anchorId: annotation.anchorId,
                  before: {
                    anchorId: annotation.anchorId,
                    color: annotation.color,
                    noteText: annotation.noteText,
                  },
                  after: null,
                },
              ]);
              bumpHistory();
            }
            showToast("已删除标注");
            invalidateAnnotations({ type: "deleted", annotationId: annotation.id });
          } catch (error) {
            if (isRevisionConflict(error)) {
              annotationConflict.open(
                {
                  anchorId: annotation.anchorId,
                  annotationId: annotation.id,
                  color: annotation.color,
                  noteText: null,
                },
                "删除时该标注已被其他操作更新(修订冲突)。请选择如何处理。",
              );
            } else {
              showToast("删除失败,请重试");
            }
          } finally {
            setConfirmBusy(false);
            setConfirmRequest(null);
          }
        },
      });
    },
    [bumpHistory, client, documentId, invalidateAnnotations, showToast],
  );

  const copySelection = useCallback(() => {
    if (popover === null) return;
    void navigator.clipboard?.writeText(popover.text);
    setCopied(true);
    setTimeout(() => {
      setCopied(false);
      setPopover(null);
    }, 1000);
  }, [popover]);

  /** 复制为 Markdown 引用块:引文 + 《文档名》· 第 N 页。 */
  const copyCitation = useCallback(() => {
    if (popover === null) return;
    const citation = buildCitationMarkdown({
      quote: popover.text,
      documentTitle,
      pageNumber: popoverPageRef.current,
    });
    setPopover(null);
    void (async () => {
      try {
        await clipboardWriteText(citation);
        showToast("已复制引用块(含出处),可直接粘贴到笔记", 2000);
      } catch {
        showToast("复制引用失败,请重试");
      }
    })();
  }, [popover, documentTitle, showToast]);

  const askSelection = useCallback(() => {
    if (popover === null) return;
    askAboutSelection(popover.text);
    setPopover(null);
  }, [popover]);

  const aiNoteSelection = useCallback(() => {
    if (popover === null) return;
    const text = popover.text;
    useWorkspaceUi.getState().setDraft(`请根据选区内容「${text.slice(0, 300)}」，提炼核心要点并生成一条结构清晰的便签笔记：\n\n`);
    useWorkspaceUi.getState().setAgentOpen(true);
    setPopover(null);
    showToast("已将选区载入 AI 笔记生成提示");
  }, [popover, showToast]);

  /** 记入「待澄清」:PDF 保留页码,回跳时直接翻到该页。 */
  const clarifySelection = useCallback(() => {
    if (popover === null) return;
    useReaderTools.getState().requestClarificationFocus({
      quote: popover.text,
      locator: {
        kind: "pdf",
        pageNumber: popoverPageRef.current,
        rects: [],
        pageRotation: "0",
        coordinateSpace: "pdf_user_space_v1",
      },
    });
    setPopover(null);
  }, [popover]);

  // 适应宽度:按容器宽度计算缩放。
  const applyFit = useCallback(() => {
    const el = scrollRef.current;
    const first = pages.find((p) => p.width != null);
    if (el === null || first?.width == null) return;
    // 双页:两列并排,还要扣掉列间距;单页/连续按整宽。
    const columns = viewMode === "two" ? 2 : 1;
    const gap = viewMode === "two" ? 18 : 0;
    const available = Math.max(120, el.clientWidth - 56 - gap);
    const minScale = viewMode === "two" ? 0.2 : 0.5;
    const fit = Math.max(minScale, Math.min(2.5, available / columns / first.width));
    setScale(fit);
    setFitMode(true);
  }, [pages, viewMode]);

  // 进入双页/退出双页时重新计算适配宽度(否则双页会横向溢出)。
  const previousViewModeRef = useRef(viewMode);
  useEffect(() => {
    if (previousViewModeRef.current === viewMode) return;
    previousViewModeRef.current = viewMode;
    const id = requestAnimationFrame(() => applyFit());
    return () => cancelAnimationFrame(id);
  }, [viewMode, applyFit]);

  useEffect(() => {
    if (!fitMode) return;
    const el = scrollRef.current;
    if (el === null) return;
    const onResize = () => applyFit();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [fitMode, applyFit]);

  // 记忆为「适应宽度」时,等容器完成首次布局后按当前宽度重算(窗口尺寸可能已变)。
  useEffect(() => {
    if (!fitMode) return;
    const id = requestAnimationFrame(() => {
      const el = scrollRef.current;
      if (el !== null && el.clientWidth > 0) applyFit();
    });
    return () => cancelAnimationFrame(id);
  }, [fitMode, applyFit]);

  // 打开标注管理浮层:引用稳定,避免页面列表因内联 lambda 整体重渲。
  const onManageMarkRef = useCallback(
    (annotation: AnnotationView, x: number, y: number) => setMarkMenu({ annotation, x, y }),
    [],
  );

  const setThemeAndPersist = useCallback((next: ReaderTheme) => {
    setTheme(next);
    try {
      window.localStorage.setItem(THEME_KEY, next);
    } catch {
      // 忽略存储不可用(隐私模式等)。
    }
  }, []);

  // 传给页面列表的回调保持引用稳定:滚动重渲不应让所有页重挂监听。
  const onCommitInkRef = useRef(commitInk);
  onCommitInkRef.current = commitInk;
  const onEraseInkAtRef = useRef(eraseInkAt);
  onEraseInkAtRef.current = eraseInkAt;
  const onAddStickyRef = useRef(addSticky);
  onAddStickyRef.current = addSticky;
  const onAddTextBoxRef = useRef(addTextBox);
  onAddTextBoxRef.current = addTextBox;

  const windowPages = useMemo(() => {
    // 打印要求所有页都已渲染(虚拟化会让未渲染页变成空白)。
    if (printMode) return new Set(pages.map((page) => page.pageNumber));
    if (viewMode === "single") return new Set([visible[0]]);
    return new Set(
      pageWindow({
        visibleStart: visible[0],
        visibleEnd: visible[1],
        pageCount: pages.length,
        buffer: 2,
      }),
    );
  }, [visible, pages, printMode, viewMode]);

  // 打印/另存为 PDF:先渲染全部页,再交给系统打印对话框(WebView2 支持另存为 PDF)。
  useEffect(() => {
    if (!printMode) return;
    const timer = window.setTimeout(() => {
      window.print();
      setPrintMode(false);
    }, 2200);
    return () => window.clearTimeout(timer);
  }, [printMode]);

  const canAnnotate = Boolean(client && documentId && versionId);
  const singlePage =
    pages.find((page) => page.pageNumber === visible[0]) ?? pages[0] ?? null;
  // 备注优先读独立字段;涂鸦/文本框的 noteText 是载荷,绝不当备注展示。
  const markMenuNote =
    markMenu === null
      ? ""
      : markMenu.annotation.remark?.trim() ||
        (markMenu.annotation.noteText.startsWith(INK_PREFIX) ||
        decodeTextBox(markMenu.annotation.noteText) !== null
          ? ""
          : decodeStyleNote(markMenu.annotation.noteText).note);

  // 整页翻译:随可见页切换;关闭时清空,不保留过期译文。
  const currentPageLines = useMemo(
    () => pages.find((p) => p.pageNumber === visible[0])?.lines ?? null,
    [pages, visible],
  );
  const pageTranslation = usePdfPageTranslation(
    client,
    documentId,
    currentPageLines,
    visible[0],
    inlineTranslate,
    inlineTarget,
  );
  const translateWholeDocument = useCallback(async () => {
    if (!client || !documentId) return;
    setDocTranslateBusy(true);
    try {
      const glossary = await loadTranslationGlossary(client);
      const { taskId } = await submitWholeDocumentTranslation(
        client,
        documentId,
        inlineTarget,
        `整篇翻译 · ${documentTitle ?? "文档"}`,
        glossary,
      );
      showToast(`整篇翻译已提交(task ${taskId.slice(0, 8)}),完成后在资料中打开`, 3200);
    } catch (error) {
      const code =
        typeof error === "object" && error !== null && "appError" in error
          ? String((error as { appError: { code?: string } }).appError.code ?? "translation_failed")
          : "translation_failed";
      showToast(`整篇翻译未提交(${code}),请重试`, 3200);
    } finally {
      setDocTranslateBusy(false);
    }
  }, [client, documentId, inlineTarget, documentTitle, showToast]);

  /** 划词翻译(单句):结果在右下角浮层显示,不改正文。 */
  const translateSelection = useCallback(
    async (targetLanguage?: string) => {
      if (popover === null || !client || !documentId) return;
      const text = popover.text;
      const target = targetLanguage ?? useReaderTools.getState().inlineTranslateTarget;
      if (targetLanguage !== undefined) {
        useReaderTools.getState().setInlineTranslateTarget(targetLanguage);
      }
      setPopover(null);
      showToast("正在翻译选中文字…", 120_000);
      try {
        const result = await client.call<{
          targetLanguage: string;
          segments: Array<{ source: string; translated: string | null; errorCode: string | null }>;
        }>("translation_inline", { documentId, targetLanguage: target, segments: [text] }, { timeoutMs: TRANSLATION_IPC_TIMEOUT_MS });
        const first = result.segments[0];
        if (first?.translated) {
          useReaderTools.getState().pushSelectionTranslation({
            source: text,
            translated: first.translated,
            targetLanguage: result.targetLanguage,
            x: popover.x,
            y: popover.y,
          });
          showToast("已翻译选中文字");
        } else {
          showToast(`翻译未完成(${first?.errorCode ?? "unknown"}),可重试`, 2600);
        }
      } catch (error) {
        const code =
          typeof error === "object" && error !== null && "appError" in error
            ? String((error as { appError: { code?: string } }).appError.code ?? "translation_failed")
            : "translation_failed";
        showToast(`翻译未完成(${code}),可重试`, 2600);
      }
    },
    [popover, client, documentId, showToast],
  );

  return (
    <div
      className={`pdf-viewer reader-theme-${theme}${
        tool !== "read" ? ` tool-${tool}` : ""
      }${printMode ? " printing" : ""}`}
    >
      {/* 阅读器工具栏 */}
      <div className="reader-toolbar" role="toolbar" aria-label="阅读器工具栏">
        <div className="reader-toolbar-group">
          <span className="reader-page-indicator" aria-live="polite">
            {visible[0]} / {pages.length}
          </span>
          <input
            className="reader-page-jump"
            type="number"
            min={1}
            max={pages.length}
            value={pageInput}
            aria-label="跳转到页码"
            onChange={(e) => setPageInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                const n = parseInt(pageInput, 10);
                if (Number.isFinite(n)) scrollToPage(n, true);
              }
            }}
            onBlur={() => setPageInput(String(visible[0]))}
          />
        </div>
        <div className="reader-toolbar-group">
          <button
            type="button"
            className="icon-btn reader-ocr-pages-btn"
            title="识别指定页 OCR:仅本机识别所选页,写入索引;不必等全文识别"
            aria-label="识别指定页"
            disabled={client === undefined || documentId === undefined || versionId === undefined}
            onClick={() => setOcrDialogPages([visible[0]])}
          >
            <ScanText className="w-4 h-4" />
          </button>
        </div>
        <div className="reader-toolbar-group">
          <button
            type="button"
            className="icon-btn"
            title="缩小"
            aria-label="缩小"
            onClick={() => {
              setFitMode(false);
              setScale((s) => Math.max(0.5, Math.round((s - 0.1) * 100) / 100));
            }}
          >
            <ZoomOut className="w-4 h-4" />
          </button>
          <span className="reader-zoom-label">{Math.round(scale * 100)}%</span>
          <button
            type="button"
            className="icon-btn"
            title="放大"
            aria-label="放大"
            onClick={() => {
              setFitMode(false);
              setScale((s) => Math.min(3, Math.round((s + 0.1) * 100) / 100));
            }}
          >
            <ZoomIn className="w-4 h-4" />
          </button>
          <button
            type="button"
            className={`icon-btn${fitMode ? " active" : ""}`}
            title="适应宽度"
            aria-label="适应宽度"
            aria-pressed={fitMode}
            onClick={() => {
              if (fitMode) {
                setFitMode(false);
                setScale(1.25);
              } else {
                applyFit();
              }
            }}
          >
            <Maximize2 className="w-4 h-4" />
          </button>
          <label className="reader-view-mode" title="页面视图">
            <span className="visually-hidden">页面视图</span>
            <select
              aria-label="页面视图"
              value={viewMode}
              onChange={(event) => {
                const next =
                  event.target.value === "single"
                    ? "single"
                    : event.target.value === "two"
                      ? "two"
                      : "continuous";
                setViewMode(next);
                if (next === "two") setFitMode(true);
              }}
            >
              <option value="continuous">连续</option>
              <option value="single">单页</option>
              <option value="two">双页(封面单页)</option>
            </select>
          </label>
          <button
            type="button"
            className={`icon-btn${thumbsOpen ? " active" : ""}`}
            title="缩略图侧栏"
            aria-label="缩略图侧栏"
            aria-pressed={thumbsOpen}
            onClick={() => setThumbsOpen((open) => !open)}
          >
            <PanelLeft className="w-4 h-4" />
          </button>
          <button
            type="button"
            className={`icon-btn${managerOpen ? " active" : ""}`}
            title="标注管理:按页列出笔迹/文本框、定位、统计与批量删除"
            aria-label="标注管理"
            aria-pressed={managerOpen}
            onClick={() => setManagerOpen((open) => !open)}
          >
            <ListChecks className="w-4 h-4" />
          </button>
          <button
            type="button"
            className="icon-btn"
            title="打印 / 另存为 PDF(在系统打印对话框中选择)"
            aria-label="打印或另存为 PDF"
            onClick={() => setPrintMode(true)}
          >
            <Printer className="w-4 h-4" />
          </button>
        </div>
        <div className="reader-toolbar-group">
          <div className="theme-switch" role="radiogroup" aria-label="阅读底色">
            {(Object.keys(THEME_LABELS) as ReaderTheme[]).map((t) => (
              <button
                key={t}
                type="button"
                className={`theme-dot theme-${t}${theme === t ? " active" : ""}`}
                title={`${THEME_LABELS[t]}底色`}
                aria-label={`${THEME_LABELS[t]}底色`}
                aria-pressed={theme === t}
                onClick={() => setThemeAndPersist(t)}
              />
            ))}
          </div>
        </div>
        <div className="reader-toolbar-group">
          <button
            type="button"
            className="icon-btn"
            title="撤销笔迹 (Ctrl+Z)"
            aria-label="撤销笔迹"
            disabled={!canUndoInk(documentId)}
            onClick={undoNow}
          >
            <Undo2 className="w-4 h-4" />
          </button>
          <button
            type="button"
            className="icon-btn"
            title="重做笔迹 (Ctrl+Shift+Z / Ctrl+Y)"
            aria-label="重做笔迹"
            disabled={!canRedoInk(documentId)}
            onClick={redoNow}
          >
            <Redo2 className="w-4 h-4" />
          </button>
        </div>
        <div className="reader-toolbar-group">
          <button
            type="button"
            className={`icon-btn${tool === "pen" ? " active" : ""}`}
            title="画笔(在页面上手绘)"
            aria-label="画笔"
            aria-pressed={tool === "pen"}
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
                  title={`画笔 ${c}`}
                  aria-label={`画笔颜色 ${c}`}
                  onClick={() => setPenColor(c)}
                />
              ))}
            </span>
          ) : null}
          <button
            type="button"
            className={`icon-btn${tool === "eraser" ? " active" : ""}`}
            title="橡皮(圆形范围,可调大小)"
            aria-label="橡皮"
            aria-pressed={tool === "eraser"}
            onClick={() => toggleTool("eraser")}
          >
            <Eraser className="w-4 h-4" />
          </button>
          {/* 固定宽度的工具参数槽:切换工具不改变工具栏高度(否则正文会被顶下去)。 */}
          <span className="tool-size-slot">
            {tool === "pen" || tool === "highlight" ? (
              <label
                className="pen-width"
                title={tool === "highlight" ? "高亮笔粗细" : "画笔粗细"}
              >
                <span className="visually-hidden">
                  {tool === "highlight" ? "高亮笔粗细" : "画笔粗细"}
                </span>
                <input
                  type="range"
                  min={2}
                  max={10}
                  step={1}
                  value={penWidthSlider(penWidth)}
                  aria-label={tool === "highlight" ? "高亮笔粗细" : "画笔粗细"}
                  onChange={(e) => setPenWidth(sliderToPenWidth(Number(e.target.value)))}
                />
              </label>
            ) : null}
            {tool === "eraser" ? (
              <label className="pen-width" title="橡皮大小">
                <span className="visually-hidden">橡皮大小</span>
                <input
                  type="range"
                  min={1}
                  max={8}
                  step={1}
                  value={Math.round(eraserRadius * 100)}
                  aria-label="橡皮大小"
                  onChange={(e) => setEraserRadius(Number(e.target.value) / 100)}
                />
              </label>
            ) : null}
          </span>
          <button
            type="button"
            className={`icon-btn${tool === "lasso" ? " active" : ""}`}
            title="套索:框选笔迹后拖动移动或删除 (Esc 取消)"
            aria-label="套索"
            aria-pressed={tool === "lasso"}
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
            title="文字注释:点击页面任意位置插入可编辑文本框"
            aria-label="文字注释"
            aria-pressed={tool === "textbox"}
            disabled={!canAnnotate}
            onClick={() => toggleTool("textbox")}
          >
            <Type className="w-4 h-4" />
          </button>
          <button
            type="button"
            className={`icon-btn${tool === "note" ? " active" : ""}`}
            title="便签:点击页面位置插入"
            aria-label="便签"
            aria-pressed={tool === "note"}
            disabled={!canAnnotate}
            onClick={() => toggleTool("note")}
          >
            <StickyNote className="w-4 h-4" />
          </button>
        </div>
        <div className="reader-toolbar-group reader-translate-group" aria-label="翻译">
          <button
            type="button"
            className={`icon-btn${inlineTranslate ? " active" : ""}`}
            title={
              translateScope === "doc"
                ? docTranslateBusy
                  ? "整篇翻译提交中…"
                  : "整篇翻译:生成译文产物,可在任务中心/资料查看"
                : inlineTranslate
                  ? "关闭整页译文"
                  : "整页翻译:当前页整段译文"
            }
            aria-label={translateScope === "doc" ? "整篇翻译" : "整页翻译"}
            aria-pressed={translateScope === "page" ? inlineTranslate : docTranslateBusy}
            disabled={!client || !documentId || docTranslateBusy}
            onClick={() => {
              if (translateScope === "doc") {
                setInlineTranslate(false);
                void translateWholeDocument();
              } else {
                setInlineTranslate(!inlineTranslate);
              }
            }}
          >
            <Languages className="w-4 h-4" />
          </button>
          <select
            className="reader-translate-scope"
            aria-label="翻译范围"
            value={translateScope}
            onChange={(event) => {
              const next = event.target.value === "doc" ? "doc" : "page";
              setTranslateScope(next);
              if (next === "doc") setInlineTranslate(false);
            }}
          >
            <option value="page">整页</option>
            <option value="doc">整篇</option>
          </select>
          <select
            className="reader-translate-target"
            aria-label="翻译目标语言"
            value={inlineTarget}
            onChange={(event) => setInlineTarget(event.target.value)}
          >
            {TRANSLATE_LANGUAGES.map((lang) => (
              <option key={lang.value} value={lang.value}>
                {lang.label}
              </option>
            ))}
          </select>
        </div>
        <div className="reader-toolbar-group">
          <button
            type="button"
            className={`icon-btn${searchOpen ? " active" : ""}`}
            title="文档内搜索 (Ctrl+F)"
            aria-label="文档内搜索"
            aria-pressed={searchOpen}
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
            onClick={() => setOutlineOpen((v) => !v)}
          >
            <ListTree className="w-4 h-4" />
          </button>
          {outlineOpen ? (
            <div className="outline-popover" role="listbox" aria-label="文档大纲">
              {outline === null ? (
                <p className="outline-empty">大纲加载中…</p>
              ) : outline.length === 0 ? (
                <p className="outline-empty">本文档没有书签大纲。</p>
              ) : (
                outline.map((item, i) => {
                  const activeOutline = outline.reduce(
                    (acc, entry, index) =>
                      entry.page !== null && entry.page <= visible[0] ? index : acc,
                    -1,
                  );
                  return (
                  <button
                    key={i}
                    type="button"
                    role="option"
                    aria-current={activeOutline === i ? "location" : undefined}
                    className={`outline-item level-${Math.min(item.level, 3)}${
                      activeOutline === i ? " current" : ""
                    }`}
                    disabled={item.page === null}
                    onClick={() => {
                      if (item.page !== null) {
                        scrollToPage(item.page, true);
                        setOutlineOpen(false);
                      }
                    }}
                  >
                    {item.title}
                    {item.page !== null ? (
                      <span className="outline-page">{item.page}</span>
                    ) : null}
                  </button>
                  );
                })
              )}
            </div>
          ) : null}
        </div>
        <ReaderToolbarExtras />
      </div>

      <ReaderSearch
        open={searchOpen}
        onClose={() => setSearchOpen(false)}
        mode="pdf"
        pages={pages}
        onJumpPage={(n) => scrollToPage(n, true)}
      />
      <SelectionTranslationToast />
      <PageTranslationPanel
        state={pageTranslation}
        onClose={() => setInlineTranslate(false)}
      />
      {toast ? (
        <div className="reader-toast" role="status">
          {toast}
        </div>
      ) : null}

      {/* 划词浮动菜单 */}
      {popover ? (
        <SelectionToolbar
          state={popover}
          onAnnotate={(color, style) => void annotateSelection(color, style)}
          onNote={() => void annotateSelection("yellow", "highlight", true)}
          onAsk={askSelection}
          onCopy={copySelection}
          onClarify={clarifySelection}
          onCopyCitation={copyCitation}
          onAiNote={aiNoteSelection}
          onTranslate={(target) => void translateSelection(target)}
          copied={copied}
          canAnnotate={canAnnotate}
          client={client}
          targetLanguage={inlineTarget}
        />
      ) : null}

      {/* 已有标注管理菜单(改色/换样式/写备注/删除) */}
      {markMenu ? (
        <div
          className="selection-popover mark-menu"
          style={{ left: `${markMenu.x}px`, top: `${markMenu.y}px` }}
          role="menu"
          aria-label="标注管理"
        >
          {ANNOTATION_PALETTE.map((p) => (
            <button
              key={p.color}
              type="button"
              className={`color-dot${markMenu.annotation.color === p.color ? " selected" : ""}`}
              style={{ backgroundColor: p.hex }}
              title={`改为${p.label}色`}
              aria-label={`改为${p.label}色`}
              onClick={() => void updateMark(markMenu.annotation, { color: p.color })}
            />
          ))}
          <span className="selection-popover-divider" aria-hidden />
          <button
            type="button"
            className="popover-btn"
            onClick={() =>
              void updateMark(markMenu.annotation, {
                underline: decodeStyleNote(markMenu.annotation.noteText).style !== "underline",
              })
            }
            title="高亮/下划线切换"
          >
            {decodeStyleNote(markMenu.annotation.noteText).style === "underline"
              ? "改为高亮"
              : "改为下划线"}
          </button>
          <button
            type="button"
            className="popover-btn danger"
            onClick={() => deleteMark(markMenu.annotation)}
            title="删除标注"
          >
            <Trash2 className="w-3.5 h-3.5 text-rose-400" aria-hidden />
            <span>删除</span>
          </button>
          <input
            key={markMenu.annotation.id}
            className="mark-note-input"
            type="text"
            defaultValue={markMenuNote}
            placeholder="写点备注…(回车保存)"
            aria-label="标注备注"
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                void updateMark(markMenu.annotation, {
                  note: (e.target as HTMLInputElement).value,
                });
              }
            }}
          />
        </div>
      ) : null}

      {/* 已保存的文字注释(按页渲染) */}
      {pages.map((page) =>
        windowPages.has(page.pageNumber)
          ? (annotationsByPage.get(page.pageNumber)?.marks ?? [])
              .map(({ annotation }) => {
                const payload = decodeTextBox(annotation.noteText);
                if (payload === null) return null;
                // 文本框 y 是页面内归一化坐标,必须用页高而不是双页行高。
                const pageH =
                  page.height != null ? page.height * scale : estimatePageHeight(page) * scale;
                return (
                  <TextBoxBubble
                    key={annotation.id}
                    annotation={annotation}
                    styleLeft={`calc(${payload.x * 100}% )`}
                    styleTop={`${payload.y * pageH}px`}
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
              })
              .filter(Boolean)
          : null,
      )}
      {textBoxDraft !== null ? (
        <TextBoxEditor
          state={textBoxDraft}
          client={client}
          documentId={documentId}
          versionId={versionId}
          format="pdf"
          onClose={() => setTextBoxDraft(null)}
          onSaved={() => void reloadAnnotations()}
          onDeleted={() => {
            setTextBoxDraft(null);
            void reloadAnnotations();
          }}
          onToast={showToast}
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
      {ocrDialogPages !== null &&
      client !== undefined &&
      documentId !== undefined &&
      versionId !== undefined ? (
        <PageOcrRecognizeDialog
          client={client}
          documentId={documentId}
          versionId={versionId}
          pageCount={pages.length}
          currentPage={visible[0]}
          initialPages={ocrDialogPages}
          bytes={pdfBytes ?? null}
          onClose={() => setOcrDialogPages(null)}
          onSaved={(statuses) => onOcrCorrected(statuses)}
          onBackgroundNotice={(message, kind) =>
            showToast(message, kind === "error" ? 5000 : 3200)
          }
        />
      ) : null}
      {managerOpen ? (
        <PdfAnnotationManager
          annotations={annotations ?? []}
          pages={pages}
          onLocate={(pageNumber) => scrollToPage(pageNumber, true)}
          onDeleteNow={deleteManagerItems}
          onClose={() => setManagerOpen(false)}
        />
      ) : null}
      <div className="pdf-view-body">
        {thumbsOpen ? (
          <aside className="pdf-thumb-rail" aria-label="页面缩略图">
            <header className="pdf-thumb-head">
              <span>页面缩略图</span>
              <button
                type="button"
                className="icon-btn"
                aria-label="关闭缩略图"
                onClick={() => setThumbsOpen(false)}
              >
                ×
              </button>
            </header>
            <div className="pdf-thumb-list">
              {pages.map((page) => (
                <PdfThumbnail
                  key={page.pageNumber}
                  pageNumber={page.pageNumber}
                  pdfDoc={pdfDoc}
                  active={page.pageNumber === visible[0]}
                  onSelect={() => scrollToPage(page.pageNumber)}
                />
              ))}
            </div>
          </aside>
        ) : null}
        {/* 页面滚动容器(自管,虚拟化事件源) */}
        <div
          className={`reader-body pdf-body pdf-scroll${
            viewMode === "two" ? " two-page-mode" : ""
          }`}
          ref={scrollRef}
          role="feed"
          aria-label="PDF 页面"
          onMouseUp={handleMouseUp}
          onPointerDown={() => setMarkMenu(null)}
        >
          {viewMode === "single" && singlePage !== null ? (
            <PdfSinglePage
              key={singlePage.pageNumber}
              pageNumber={singlePage.pageNumber}
              pageText={singlePage}
              pdfDoc={pdfDoc}
              scale={scale}
              tool={tool}
              penColor={penColor}
              penWidth={penWidth}
              eraserRadius={eraserRadius}
              layers={layers}
              pageAnnotations={
                annotationsByPage.get(singlePage.pageNumber) ?? EMPTY_PAGE_ANNOTATIONS
              }
              pendingStrokes={
                optimisticByPage.get(singlePage.pageNumber) ?? EMPTY_STROKES
              }
              onCommitInk={onCommitInkRef.current}
              onEraseInkAt={onEraseInkAtRef.current}
              onEraseStart={beginEraseGesture}
              onEraseEnd={endEraseGesture}
              onMoveInk={moveInk}
              onDeleteInk={requestDeleteInk}
              onManageMark={onManageMarkRef}
              onInkContext={onManageMarkRef}
              onAddSticky={onAddStickyRef.current}
              onAddTextBox={onAddTextBoxRef.current}
              onOpenSticky={openStickyFocus}
              ocrStatus={pageOcr?.get(singlePage.pageNumber)}
              onReprocessOcr={onReprocessOcr}
              onRecognizeOcrPages={(ocrPages) => setOcrDialogPages(ocrPages)}
              pageCount={pages.length}
              ocrBusy={ocrBusy}
              client={client}
              documentId={documentId}
              versionId={versionId}
              onOcrCorrected={onOcrCorrected}
            />
          ) : (
            <PdfPageList
              pages={pages}
              layout={layout}
              windowPages={windowPages}
              pdfDoc={pdfDoc}
              scale={scale}
              tool={tool}
              penColor={penColor}
              penWidth={penWidth}
              eraserRadius={eraserRadius}
              layers={layers}
              annotationsByPage={annotationsByPage}
              optimisticByPage={optimisticByPage}
              commitInk={onCommitInkRef.current}
              eraseInkAt={onEraseInkAtRef.current}
              eraseStart={beginEraseGesture}
              eraseEnd={endEraseGesture}
              moveInk={moveInk}
              deleteInk={requestDeleteInk}
              manageMark={onManageMarkRef}
              addSticky={onAddStickyRef.current}
              addTextBox={onAddTextBoxRef.current}
              pageOcr={pageOcr}
              onReprocessOcr={onReprocessOcr}
              onRecognizeOcrPages={(ocrPages) => setOcrDialogPages(ocrPages)}
              pageCount={pages.length}
              ocrBusy={ocrBusy}
              client={client}
              documentId={documentId}
              versionId={versionId}
              onOcrCorrected={onOcrCorrected}
            />
          )}
        </div>
      </div>
    </div>
  );
}
