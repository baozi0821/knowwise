// M13 拥有:可重建 UI 状态(《M13》§3/§7)。全局 store 不保存笔记正文、
// Artifact 内容、Citation 或文档文本——持久资产全部由模块经查询从 Rust
// 恢复;这里只有面板开关、标签页、当前文档、选区与草稿。
//
// 布局模型:右栏是唯一的辅助面板(Dock),用标签在「AI 助手 / 笔记 / 资料」之间
// 切换,整列全高、宽度可拖拽;布局偏好(开关/宽度/标签)落 localStorage,
// 只保存可重建的界面状态,不保存任何用户资产。
//
// 标签页模型:每个文档至多一个标签;activeDocumentId 即当前标签。
// 草稿按文档保留(切标签/专注模式不丢);关闭标签只清选区与导航上下文。

import { create } from "zustand";

export type ReaderDrawerKind = "outline" | "annotations" | "notes" | "bookmarks";

/** 右栏(Dock)的标签:AI 助手 / 笔记 / 资料。 */
export type DockTab = "agent" | "notes" | "artifacts";

export type SelectionState = {
  documentId: string;
  documentVersionId: string;
  text: string;
};

export type DocumentTab = {
  documentId: string;
  versionId: string | null;
};

/** 主阅读区的「资料阅览」目标:用阅读器(而非侧栏)打开一个 AI 产物版本。 */
export type ArtifactReading = {
  artifactId: string;
  versionId: string | null;
  documentId: string | null;
};

/** 阅读器当前视口(可重建;PDF 有页码,Markdown/Word 为 undefined)。 */
export type ReaderViewportState = {
  pageNumber?: number;
  pageCount?: number;
  /** MD/DOCX 视口所在标题路径(用于“本节”范围;PDF 不虚构)。 */
  headingPath?: string[];
};

/**
 * 整页译文浮层的位置/尺寸(相对 .pdf-viewer 左上角)。
 * null = 使用按容器计算的默认位置;数值在渲染时按容器边界收敛。
 */
export type TranslationPanelRect = {
  x: number;
  y: number;
  w: number;
  h: number;
};

export type WorkspaceUiState = {
  /** 打开的标签页(有序);currentDocumentId 是其中的活动项。 */
  tabs: DocumentTab[];
  currentDocumentId: string | null;
  currentVersionId: string | null;
  drawer: ReaderDrawerKind | null;
  /** 右栏是否展开(包含 AI 助手与笔记两个标签)。 */
  dockOpen: boolean;
  /** 右栏当前标签。 */
  dockTab: DockTab;
  /** 专注模式:隐藏右栏与抽屉;选区菜单/快捷键仍可唤回(《M13》§4)。 */
  focusMode: boolean;
  selection: SelectionState | null;
  /** 每文档当前页(Live 状态,不落盘;用于右栏“当前页 ±N”提问范围)。 */
  readerViewport: Record<string, ReaderViewportState>;
  /** 每文档一份未发送草稿;关闭 Agent/专注/切换文档都不丢弃。 */
  drafts: Record<string, string>;
  /** 可选:减少动态/关闭平滑滚动(系统偏好之外的显式开关)。 */
  reducedMotion: boolean;
  /** 面板宽度(可拖拽调整)。 */
  sidebarWidth: number;
  dockWidth: number;
  /** 双文档对照的左右宽度比例(左窗格占比 0.25–0.75)。 */
  compareRatio: number;
  /** 左侧文档库开关。 */
  sidebarOpen: boolean;
  /** 整页译文浮层的位置/尺寸;null = 默认位置。 */
  pageTranslationLayout: TranslationPanelRect | null;
  setPageTranslationLayout: (rect: TranslationPanelRect | null) => void;
  /**
   * 主阅读区正在阅览的资料;null = 显示文档正文。
   * 与文档阅读互斥:打开资料时保留文档标签与阅读状态,关闭即回到文档。
   */
  artifactReading: ArtifactReading | null;
  /** 双文档对照:右窗格;null = 单窗格。 */
  compareTarget: DocumentTab | null;
  setCompareTarget: (target: DocumentTab | null) => void;

  openDocument: (documentId: string, versionId: string | null) => void;
  closeDocument: () => void;
  /** 关闭指定标签页;活动标签被关时切到相邻标签。 */
  closeTab: (documentId: string) => void;
  /** Ctrl+Tab 循环切换标签。 */
  cycleTab: (direction?: 1 | -1) => void;
  setSelection: (selection: SelectionState) => void;
  clearSelection: () => void;
  setReaderViewport: (documentId: string, viewport: ReaderViewportState) => void;
  setDraft: (text: string) => void;
  setDrawer: (drawer: ReaderDrawerKind | null) => void;
  setDockOpen: (open: boolean) => void;
  /**
   * 把一则笔记加入「本轮引用来源」(与引用资料同一链路)。
   * AgentRegion 消费后清空;这里只做一次性投递,不保存状态历史。
   */
  pendingNoteSource: { id: string; title: string; documentId: string } | null;
  addNoteSource: (note: { id: string; title: string; documentId: string }) => void;
  consumeNoteSource: () =>
    | { id: string; title: string; documentId: string }
    | null;
  /** 在主阅读区打开/关闭资料阅览。 */
  openArtifactReading: (target: ArtifactReading) => void;
  closeArtifactReading: () => void;
  /**
   * 「查看结果」等入口投递到资料面板的一次性打开请求。
   * 面板可能尚未挂载:先存这里,挂载后消费,避免事件丢失。
   */
  pendingArtifactOpen: {
    artifactId: string | null;
    versionId: string | null;
    read: boolean;
  } | null;
  setPendingArtifactOpen: (entry: {
    artifactId: string | null;
    versionId: string | null;
    read: boolean;
  }) => void;
  consumePendingArtifactOpen: () => {
    artifactId: string | null;
    versionId: string | null;
    read: boolean;
  } | null;
  /** 选中标签;顺带展开右栏(点标签即打开)。 */
  setDockTab: (tab: DockTab) => void;
  /** 点当前标签收起、点另一个标签展开并切换。 */
  toggleDockTab: (tab: DockTab) => void;
  /**
   * 兼容旧调用:setAgentOpen(true) = 展开右栏并切到 Agent;
   * setNotesOpen(true) = 展开右栏并切到笔记;false = 收起右栏。
   */
  setAgentOpen: (open: boolean) => void;
  setNotesOpen: (open: boolean) => void;
  setFocusMode: (focusMode: boolean) => void;
  setReducedMotion: (reducedMotion: boolean) => void;
  setSidebarWidth: (width: number) => void;
  setDockWidth: (width: number) => void;
  setCompareRatio: (ratio: number) => void;
  setSidebarOpen: (open: boolean) => void;
};

function tabAfterClose(
  tabs: DocumentTab[],
  closedIndex: number,
): { currentDocumentId: string | null; currentVersionId: string | null } {
  const next = tabs[closedIndex] ?? tabs[closedIndex - 1];
  return next === undefined
    ? { currentDocumentId: null, currentVersionId: null }
    : { currentDocumentId: next.documentId, currentVersionId: next.versionId };
}

/** 布局偏好(可重建,非用户资产):只保存面板开关与宽度。 */
const UI_PREFS_KEY = "knowwise.ui.layout.v1";

type UiPrefs = {
  dockOpen: boolean;
  dockTab: DockTab;
  dockWidth: number;
  sidebarWidth: number;
  compareRatio: number;
  sidebarOpen: boolean;
  reducedMotion: boolean;
  pageTranslationLayout: TranslationPanelRect | null;
};

const DEFAULT_PREFS: UiPrefs = {
  dockOpen: true,
  dockTab: "agent",
  dockWidth: 420,
  sidebarWidth: 240,
  compareRatio: 0.5,
  sidebarOpen: true,
  reducedMotion: false,
  pageTranslationLayout: null,
};

function isDockTab(value: unknown): value is DockTab {
  return value === "agent" || value === "notes" || value === "artifacts";
}

function clampDockWidth(width: number): number {
  if (!Number.isFinite(width)) return DEFAULT_PREFS.dockWidth;
  return Math.min(900, Math.max(300, Math.round(width)));
}

function clampSidebarWidth(width: number): number {
  if (!Number.isFinite(width)) return DEFAULT_PREFS.sidebarWidth;
  return Math.min(420, Math.max(180, Math.round(width)));
}

function isTranslationPanelRect(value: unknown): value is TranslationPanelRect {
  if (typeof value !== "object" || value === null) return false;
  const rect = value as Record<string, unknown>;
  return (["x", "y", "w", "h"] as const).every(
    (key) => typeof rect[key] === "number" && Number.isFinite(rect[key]),
  );
}

/** 双文档分栏比例:0.5 为 50/50;两侧最小约 240px,由组件按容器宽度再收敛。 */
export function clampCompareRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return DEFAULT_PREFS.compareRatio;
  return Math.min(0.75, Math.max(0.25, ratio));
}

/** 读取布局偏好;损坏/不可用(隐私模式、jsdom)时回退默认值。 */
export function loadUiPrefs(): UiPrefs {
  try {
    const raw = globalThis.localStorage?.getItem(UI_PREFS_KEY);
    if (raw === null || raw === undefined) return { ...DEFAULT_PREFS };
    const parsed = JSON.parse(raw) as Partial<UiPrefs>;
    return {
      dockOpen: typeof parsed.dockOpen === "boolean" ? parsed.dockOpen : DEFAULT_PREFS.dockOpen,
      dockTab: isDockTab(parsed.dockTab) ? parsed.dockTab : DEFAULT_PREFS.dockTab,
      dockWidth: clampDockWidth(parsed.dockWidth ?? DEFAULT_PREFS.dockWidth),
      sidebarWidth: clampSidebarWidth(parsed.sidebarWidth ?? DEFAULT_PREFS.sidebarWidth),
      compareRatio: clampCompareRatio(parsed.compareRatio ?? DEFAULT_PREFS.compareRatio),
      sidebarOpen:
        typeof parsed.sidebarOpen === "boolean" ? parsed.sidebarOpen : DEFAULT_PREFS.sidebarOpen,
      reducedMotion:
        typeof parsed.reducedMotion === "boolean"
          ? parsed.reducedMotion
          : DEFAULT_PREFS.reducedMotion,
      pageTranslationLayout: isTranslationPanelRect(parsed.pageTranslationLayout)
        ? {
            x: parsed.pageTranslationLayout.x,
            y: parsed.pageTranslationLayout.y,
            w: parsed.pageTranslationLayout.w,
            h: parsed.pageTranslationLayout.h,
          }
        : DEFAULT_PREFS.pageTranslationLayout,
    };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

function persistUiPrefs(state: WorkspaceUiState): void {
  try {
    const prefs: UiPrefs = {
      dockOpen: state.dockOpen,
      dockTab: state.dockTab,
      dockWidth: state.dockWidth,
      sidebarWidth: state.sidebarWidth,
      compareRatio: state.compareRatio,
      sidebarOpen: state.sidebarOpen,
      reducedMotion: state.reducedMotion,
      pageTranslationLayout: state.pageTranslationLayout,
    };
    globalThis.localStorage?.setItem(UI_PREFS_KEY, JSON.stringify(prefs));
  } catch {
    // 存储不可用只影响"下次启动的布局记忆",不影响功能。
  }
}

const prefs = loadUiPrefs();

export const useWorkspaceUi = create<WorkspaceUiState>((set, get) => ({
  tabs: [],
  currentDocumentId: null,
  currentVersionId: null,
  drawer: null,
  dockOpen: prefs.dockOpen,
  dockTab: prefs.dockTab,
  artifactReading: null,
  compareTarget: null,
  pendingNoteSource: null,
  pendingArtifactOpen: null,
  focusMode: false,
  selection: null,
  readerViewport: {},
  drafts: {},
  reducedMotion: prefs.reducedMotion,
  sidebarWidth: prefs.sidebarWidth,
  dockWidth: prefs.dockWidth,
  compareRatio: prefs.compareRatio,
  sidebarOpen: prefs.sidebarOpen,
  pageTranslationLayout: prefs.pageTranslationLayout,

  setCompareTarget: (compareTarget) => set({ compareTarget }),
  openDocument: (documentId, versionId) => {
    const state = get();
    const tabs = state.tabs.some((t) => t.documentId === documentId)
      ? state.tabs.map((t) => (t.documentId === documentId ? { ...t, versionId } : t))
      : [...state.tabs, { documentId, versionId }];
    if (state.currentDocumentId === documentId) {
      if (state.currentVersionId !== versionId) {
        set({ tabs, currentVersionId: versionId, selection: null });
      }
      return;
    }
    // 先清理当前选择与导航上下文,再加载新文档(《M13》§7)。
    set({
      tabs,
      currentDocumentId: documentId,
      currentVersionId: versionId,
      selection: null,
      // 打开/切换到文档时退出资料阅览,避免主区仍显示产物。
      artifactReading: null,
    });
  },
  closeDocument: () => {
    const state = get();
    const index = state.tabs.findIndex((t) => t.documentId === state.currentDocumentId);
    const tabs = state.tabs.filter((t) => t.documentId !== state.currentDocumentId);
    const next = tabAfterClose(tabs, index);
    set({ tabs, ...next, selection: null, drawer: null });
  },
  closeTab: (documentId) => {
    const state = get();
    const index = state.tabs.findIndex((t) => t.documentId === documentId);
    if (index === -1) return;
    const tabs = state.tabs.filter((t) => t.documentId !== documentId);
    if (state.currentDocumentId === documentId) {
      const next = tabAfterClose(tabs, index);
      set({ tabs, ...next, selection: null });
    } else {
      set({ tabs });
    }
  },
  cycleTab: (direction = 1) => {
    const state = get();
    if (state.tabs.length < 2 || state.currentDocumentId === null) return;
    const index = state.tabs.findIndex((t) => t.documentId === state.currentDocumentId);
    const nextIndex = (index + direction + state.tabs.length) % state.tabs.length;
    const next = state.tabs[nextIndex]!;
    set({ currentDocumentId: next.documentId, currentVersionId: next.versionId, selection: null });
  },
  setSelection: (selection) => {
    // 只接受当前文档的选区:迟到的旧文档回调不得污染新文档。
    if (selection.documentId !== get().currentDocumentId) return;
    set({ selection });
  },
  clearSelection: () => set({ selection: null }),
  setReaderViewport: (documentId, viewport) => {
    if (documentId !== get().currentDocumentId) return;
    set((state) => ({
      readerViewport: { ...state.readerViewport, [documentId]: viewport },
    }));
  },
  setDraft: (text) => {
    const documentId = get().currentDocumentId;
    if (documentId === null) return;
    set((state) => ({ drafts: { ...state.drafts, [documentId]: text } }));
  },
  setDrawer: (drawer) => set({ drawer }),
  setDockOpen: (dockOpen) => set({ dockOpen }),
  setDockTab: (dockTab) => set({ dockTab, dockOpen: true }),
  addNoteSource: (note) => set({ pendingNoteSource: note }),
  consumeNoteSource: () => {
    const pending = get().pendingNoteSource;
    if (pending !== null) set({ pendingNoteSource: null });
    return pending;
  },
  openArtifactReading: (target) => set({ artifactReading: target }),
  closeArtifactReading: () => set({ artifactReading: null }),
  setPendingArtifactOpen: (entry) => set({ pendingArtifactOpen: entry }),
  consumePendingArtifactOpen: () => {
    const pending = get().pendingArtifactOpen;
    if (pending !== null) set({ pendingArtifactOpen: null });
    return pending;
  },
  toggleDockTab: (tab) => {
    const state = get();
    if (state.dockOpen && state.dockTab === tab) {
      set({ dockOpen: false });
      return;
    }
    set({ dockOpen: true, dockTab: tab });
  },
  // AI 助手/笔记/资料互斥呈现、共享同一右栏宽度。
  setAgentOpen: (open) => set(open ? { dockOpen: true, dockTab: "agent" } : { dockOpen: false }),
  setNotesOpen: (open) => set(open ? { dockOpen: true, dockTab: "notes" } : { dockOpen: false }),
  setFocusMode: (focusMode) => set({ focusMode }),
  setReducedMotion: (reducedMotion) => set({ reducedMotion }),
  setSidebarWidth: (sidebarWidth) => set({ sidebarWidth: clampSidebarWidth(sidebarWidth) }),
  setDockWidth: (dockWidth) => set({ dockWidth: clampDockWidth(dockWidth) }),
  setCompareRatio: (compareRatio) => set({ compareRatio: clampCompareRatio(compareRatio) }),
  setSidebarOpen: (sidebarOpen) => set({ sidebarOpen }),
  setPageTranslationLayout: (pageTranslationLayout) => set({ pageTranslationLayout }),
}));

// 布局偏好落盘:订阅变更并合并 250ms 内的连续拖拽写入。
let persistTimer: ReturnType<typeof setTimeout> | null = null;
useWorkspaceUi.subscribe((state) => {
  if (persistTimer !== null) clearTimeout(persistTimer);
  persistTimer = setTimeout(() => {
    persistTimer = null;
    persistUiPrefs(state);
  }, 250);
});

/** 草稿读取选择器:无当前文档或未写过草稿时为空串。 */
export function selectDraft(state: WorkspaceUiState, documentId: string | null): string {
  if (documentId === null) return "";
  return state.drafts[documentId] ?? "";
}
