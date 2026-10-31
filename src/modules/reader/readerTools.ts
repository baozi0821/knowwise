// 阅读器工具状态(PDF 与 Markdown/DOCX 视图共享):画笔/橡皮、画笔颜色、
// Markdown 源码模式、笔记面板聚焦请求。跨格式切换时工具状态保持。
import { create } from "zustand";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import { useWorkspaceUi } from "../workspace/uiStore";

export type ReaderTool =
  | "read"
  | "pen"
  | "highlight"
  | "eraser"
  | "note"
  | "textbox"
  | "lasso"
  | "rect"
  | "ellipse"
  | "line"
  | "arrow";

/** 标注图层显隐(只影响视图,不改数据)。 */
export type AnnotationLayers = {
  highlight: boolean;
  ink: boolean;
  textbox: boolean;
  note: boolean;
};

export const DEFAULT_LAYERS: AnnotationLayers = {
  highlight: true,
  ink: true,
  textbox: true,
  note: true,
};

const PEN_SETTINGS_PREFIX = "knowwise.reader.pen.v1.";

/** 按文档记忆画笔/橡皮设置(仅界面偏好,不是用户资产)。 */
export function loadDocumentPen(documentId: string | undefined): {
  penColor?: string;
  penWidth?: number;
  eraserRadius?: number;
} | null {
  if (documentId === undefined) return null;
  try {
    const raw = window.localStorage.getItem(`${PEN_SETTINGS_PREFIX}${documentId}`);
    if (raw === null) return null;
    const parsed = JSON.parse(raw) as {
      penColor?: unknown;
      penWidth?: unknown;
      eraserRadius?: unknown;
    };
    return {
      penColor:
        typeof parsed.penColor === "string" && /^#[0-9a-f]{6}$/i.test(parsed.penColor)
          ? parsed.penColor
          : undefined,
      penWidth:
        typeof parsed.penWidth === "number"
          ? Math.min(0.012, Math.max(0.0015, parsed.penWidth))
          : undefined,
      eraserRadius:
        typeof parsed.eraserRadius === "number"
          ? Math.min(0.08, Math.max(0.005, parsed.eraserRadius))
          : undefined,
    };
  } catch {
    return null;
  }
}

export function saveDocumentPen(
  documentId: string | undefined,
  value: { penColor: string; penWidth: number; eraserRadius: number },
): void {
  if (documentId === undefined) return;
  try {
    window.localStorage.setItem(
      `${PEN_SETTINGS_PREFIX}${documentId}`,
      JSON.stringify(value),
    );
  } catch {
    // 存储不可用只影响下次记忆。
  }
}

export const PEN_COLORS = ["#f04f9e", "#f2b705", "#3f83f8", "#31c48d", "#ef4444", "#0f172a"];

/** 划词"待澄清":先带出原文与位置,由用户在笔记面板补完问题。 */
export type PendingClarification = {
  quote: string;
  locator: SourceLocator | null;
};

export type ReaderToolsState = {
  tool: ReaderTool;
  penColor: string;
  /** 画笔线宽(相对页面宽度的比例,0.0015–0.012);高亮笔用同一数值但更粗。 */
  penWidth: number;
  /** 橡皮擦半径(相对页面宽度的比例)。 */
  eraserRadius: number;
  /** Markdown 源码模式(Typora 式切换)。 */
  mdSource: boolean;
  /** 内联翻译:在原文旁直接显示译文(浏览器式),不是产物。 */
  inlineTranslate: boolean;
  /** 内联翻译目标语言(BCP-47 或常见名称,如 zh-CN / en-US)。 */
  inlineTranslateTarget: string;
  /** 划词翻译结果(就地显示在选区下方,不落库)。 */
  selectionTranslations: Array<{
    id: string;
    source: string;
    translated: string;
    targetLanguage: string;
    /** 选区所在视口坐标:译文浮层初始就近显示,可拖动。 */
    x?: number;
    y?: number;
  }>;
  /** 请求笔记面板聚焦的标注 ID(消费后清空)。 */
  focusAnnotationId: string | null;
  /** 请求笔记面板打开指定笔记 ID(消费后清空;用于正文页边标记)。 */
  focusNoteId: string | null;
  /** 标注图层显隐(仅视图)。 */
  layers: AnnotationLayers;
  /** 待澄清预填(消费后清空)。 */
  pendingClarification: PendingClarification | null;

  setTool: (tool: ReaderTool) => void;
  toggleTool: (tool: ReaderTool) => void;
  setPenColor: (color: string) => void;
  setPenWidth: (width: number) => void;
  setEraserRadius: (radius: number) => void;
  setLayer: (layer: keyof AnnotationLayers, visible: boolean) => void;
  toggleMdSource: () => void;
  setInlineTranslate: (on: boolean) => void;
  setInlineTranslateTarget: (language: string) => void;
  pushSelectionTranslation: (entry: {
    source: string;
    translated: string;
    targetLanguage: string;
    x?: number;
    y?: number;
  }) => void;
  clearSelectionTranslations: () => void;
  /** 打开笔记面板并聚焦指定标注(无 ID 仅打开)。 */
  requestNotesFocus: (annotationId?: string) => void;
  consumeFocusAnnotation: () => string | null;
  /** 打开笔记面板并定位到一条笔记。 */
  requestNoteFocus: (noteId: string) => void;
  consumeFocusNote: () => string | null;
  /** 打开笔记面板的"待澄清"视图,预填原文与位置。 */
  requestClarificationFocus: (payload: PendingClarification) => void;
  consumePendingClarification: () => PendingClarification | null;
};

export const useReaderTools = create<ReaderToolsState>((set, get) => ({
  tool: "read",
  penColor: PEN_COLORS[0]!,
  penWidth: 0.0035,
  eraserRadius: 0.02,
  mdSource: false,
  inlineTranslate: false,
  inlineTranslateTarget: "zh-CN",
  selectionTranslations: [],
  focusAnnotationId: null,
  focusNoteId: null,
  layers: { ...DEFAULT_LAYERS },
  pendingClarification: null,

  setTool: (tool) => set({ tool }),
  toggleTool: (tool) => set({ tool: get().tool === tool ? "read" : tool }),
  setPenColor: (penColor) => set({ penColor }),
  setPenWidth: (penWidth) => set({ penWidth: Math.min(0.012, Math.max(0.0015, penWidth)) }),
  setEraserRadius: (eraserRadius) =>
    set({ eraserRadius: Math.min(0.08, Math.max(0.005, eraserRadius)) }),
  setLayer: (layer, visible) =>
    set((state) => ({ layers: { ...state.layers, [layer]: visible } })),
  toggleMdSource: () => set({ mdSource: !get().mdSource }),
  setInlineTranslate: (inlineTranslate) => set({ inlineTranslate }),
  setInlineTranslateTarget: (inlineTranslateTarget) => set({ inlineTranslateTarget }),
  pushSelectionTranslation: (entry) =>
    set((state) => ({
      selectionTranslations: [
        ...state.selectionTranslations.slice(-9),
        { ...entry, id: `${Date.now()}-${state.selectionTranslations.length}` },
      ],
    })),
  clearSelectionTranslations: () => set({ selectionTranslations: [] }),
  requestNotesFocus: (annotationId) => {
    useWorkspaceUi.getState().setNotesOpen(true);
    set({ focusAnnotationId: annotationId ?? null, tool: "read" });
  },
  consumeFocusAnnotation: () => {
    const id = get().focusAnnotationId;
    if (id !== null) set({ focusAnnotationId: null });
    return id;
  },
  requestNoteFocus: (noteId) => {
    useWorkspaceUi.getState().setNotesOpen(true);
    set({ focusNoteId: noteId, tool: "read" });
  },
  consumeFocusNote: () => {
    const id = get().focusNoteId;
    if (id !== null) set({ focusNoteId: null });
    return id;
  },
  requestClarificationFocus: (payload) => {
    useWorkspaceUi.getState().setNotesOpen(true);
    set({ pendingClarification: payload, tool: "read" });
  },
  consumePendingClarification: () => {
    const pending = get().pendingClarification;
    if (pending !== null) set({ pendingClarification: null });
    return pending;
  },
}));
