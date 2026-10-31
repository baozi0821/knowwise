// 笔记面板(右栏 Dock,OneNote 式三区布局):
//   顶:功能栏(快速笔记 + 新建笔记 + 筛选 chips + 标签筛选 + 搜索 + 更多菜单)
//   主体:左索引列表(约 160px)+ 右「笔记空间」(详情 / 引用出处 / 编辑与预览 / 标签 / 状态)
//   窄宽(容器 < 520px)降级为单栏:列表 → 详情,详情顶部「‹ 列表」返回
// 数据两类:带真实锚点的标注(高亮/下划线/笔记/便签/涂鸦)与普通随笔;
// 变更经 knowwise-annotations-changed 广播失效。写作走 800ms 防抖自动保存,
// 切条目/卸载时 flush;所有写失败都给中文可读提示,不再静默吞错。
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import {
  ArrowLeft,
  Bookmark,
  Camera,
  Check,
  CircleHelp,
  Download,
  FileText,
  Highlighter,
  MapPin,
  MoreHorizontal,
  NotebookPen,
  Pencil,
  PenLine,
  Plus,
  Search,
  Sparkles,
  StickyNote,
  Trash2,
  Share2,
} from "lucide-react";
import { useServices } from "../../app/services";
import { qk } from "../../app/api/keys";
import { toAppError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import type { AppError } from "../../shared/generated/AppError";
import type { AnnotationView } from "../../shared/generated/AnnotationView";
import type { BookmarkView } from "../../shared/generated/BookmarkView";
import type { ClarificationView } from "../../shared/generated/ClarificationView";
import type { DeleteResult } from "../../shared/generated/DeleteResult";
import type { NoteStatus } from "../../shared/generated/NoteStatus";
import type { NoteView } from "../../shared/generated/NoteView";
import type { Page } from "../../shared/generated/Page";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import type { SourceReference } from "../../shared/generated/SourceReference";
import type { ReadingPosition } from "../../shared/generated/ReadingPosition";
import { INK_PREFIX, decodeStyleNote, decodeTextBox, encodeStyleNote } from "../reader/annotations";
import type { AnnotationChangeDetail } from "../reader/annotationEvents";
import { useReaderTools } from "../reader/readerTools";
import { useWorkspaceUi } from "../workspace/uiStore";
import { useQaStore } from "../qa/qaStore";
import { KnowledgeGraph } from "./KnowledgeGraph";
import { Markdown } from "../reader/Markdown";
import { useScreenClip } from "../capture/screenClip";
import { dialogSavePath } from "../../app/ipc/transport";
import {
  createAutosaveController,
  type NoteDraft,
  type SaveOutcome,
} from "./autosave";
import { MAX_NOTE_BODY_CHARS } from "./bodyPolicy";
import { NoteEditor } from "./NoteEditor";
import {
  annotationBody,
  buildNoteItems,
  collectTags,
  filterNoteItems,
  locatorLabel,
  NOTE_KIND_FILTERS,
  NOTE_KIND_LABELS,
  NOTE_SORT_LABELS,
  type NoteItem,
  type NoteKindFilter,
  type NoteSortMode,
} from "./noteItems";
import { addTags, removeTag, TAG_ERROR_LABELS } from "./tags";
import { jumpToLocator } from "./jumpToLocator";
import { BookmarksPanel, ClarificationsPanel } from "./NotesPanels";
import "./notes.css";

/** 自动保存防抖(契约:约 800ms)。 */
export const NOTES_AUTOSAVE_DELAY_MS = 800;
/** 右栏窄于该宽度时切换为单栏 master→detail。 */
export const NOTES_NARROW_BREAKPOINT_PX = 520;

const KIND_ICONS = {
  note: NotebookPen,
  highlight: Highlighter,
  underline: Highlighter,
  ink: PenLine,
  sticky: StickyNote,
  plain: NotebookPen,
} as const;

const NOTE_STATUS_LABELS: Record<NoteStatus, string> = {
  open: "进行中",
  confirmed: "已确认",
  needs_clarification: "待澄清",
};

const SAVE_STATE_LABELS: Record<SaveState, string> = {
  idle: "",
  unsaved: "未保存",
  saving: "保存中…",
  saved: "已保存",
  error: "保存失败",
  conflict: "内容已被其他修改",
};

const timeFormatter = new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
});

function formatTime(ms: number): string {
  return timeFormatter.format(new Date(ms));
}

function itemBody(item: NoteItem): string {
  return item.annotation !== null ? annotationBody(item.annotation) : (item.note?.body ?? "");
}

function itemRevision(item: NoteItem): number {
  return item.annotation?.revision ?? item.note?.revision ?? 0;
}

function errorText(error: unknown): string {
  const presentation = describeAppError(toAppError(error));
  const detail =
    presentation.detailLines.length > 0 ? `(${presentation.detailLines.join(";")})` : "";
  return `${presentation.title}${detail}`;
}

type SaveState = "idle" | "unsaved" | "saving" | "saved" | "error" | "conflict";

type ToastState = { kind: "success" | "error"; message: string };

/** 容器宽度:_520px 以下切单栏;ResizeObserver 优先,浏览器无 RO 时退回 window resize。
 *  `mountKey` 变化(例如从「无文档」空态切到有文档主体)时重新测量:首次挂载
 *  可能还没有可测量的根节点,仅靠 ref 依赖不会重跑 effect,单栏模式就不会生效。 */
function useContainerWidth(ref: RefObject<HTMLElement>, mountKey?: unknown): number {
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const element = ref.current;
    if (element === null) return;
    const measure = (entries?: ResizeObserverEntry[]) => {
      const measured = entries?.[0]?.contentRect.width ?? element.getBoundingClientRect().width;
      setWidth((previous) => (Math.abs(previous - measured) < 1 ? previous : measured));
    };
    measure();
    if (typeof ResizeObserver === "undefined") {
      const onResize = () => measure();
      window.addEventListener("resize", onResize);
      return () => window.removeEventListener("resize", onResize);
    }
    const observer = new ResizeObserver((entries) => measure(entries));
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref, mountKey]);
  return width;
}

function ErrorAlert({
  error,
  onRetry,
}: {
  error: AppError;
  onRetry?: (() => void) | undefined;
}): ReactNode {
  const presentation = describeAppError(error);
  const detail =
    presentation.detailLines.length > 0 ? `(${presentation.detailLines.join(";")})` : "";
  return (
    <p role="alert" className="notes-error">
      {`${presentation.title}${detail}`}
      {onRetry !== undefined ? (
        <button type="button" className="notes-error-retry" onClick={onRetry}>
          重试
        </button>
      ) : null}
    </p>
  );
}

/** 顶部三视图:笔记 / 书签 / 待澄清。 */
type NotesView = "notes" | "bookmarks" | "clarifications" | "graph";

export function NotesDock({
  documentId,
  versionId,
  active = true,
}: {
  documentId: string | null;
  versionId: string | null;
  /** 面板此刻是否显示(右栏标签=笔记且右栏展开)。隐藏时不发起查询。 */
  active?: boolean;
}): ReactNode {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const rootRef = useRef<HTMLElement>(null);
  const indexRef = useRef<HTMLDivElement>(null);
  const width = useContainerWidth(rootRef, documentId);
  const narrow = width > 0 && width < NOTES_NARROW_BREAKPOINT_PX;
  const clip = useScreenClip();
  // 版本号保留在契约中(旧调用方仍传入);书签按 locator 回跳,不依赖版本。
  void versionId;

  const [quickDraft, setQuickDraft] = useState("");
  const [quickBusy, setQuickBusy] = useState(false);
  const [quickError, setQuickError] = useState<AppError | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [filter, setFilter] = useState<NoteKindFilter>("all");
  const [search, setSearch] = useState("");
  const [tagFilter, setTagFilter] = useState("");
  const [statusFilter, setStatusFilter] = useState("");
  const [originFilter, setOriginFilter] = useState("");
  const [timeFilter, setTimeFilter] = useState<"all" | "7d" | "30d">("all");
  const [sort, setSort] = useState<NoteSortMode>("updated");
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [mobileDetail, setMobileDetail] = useState(false);
  // 左索引栏:可折叠 + 可拖拽调宽(与「会话管理」同一种主从结构)。
  const [indexCollapsed, setIndexCollapsed] = useState(false);
  const [indexWidth, setIndexWidth] = useState(190);
  const dragRef = useRef<{ startX: number; startWidth: number } | null>(null);
  const onIndexResizeStart = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    dragRef.current = { startX: event.clientX, startWidth: indexWidth };
    (event.currentTarget as HTMLElement).setPointerCapture?.(event.pointerId);
  }, [indexWidth]);

  const onIndexResizeMove = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (drag === null) return;
    const next = Math.min(360, Math.max(120, drag.startWidth + (event.clientX - drag.startX)));
    setIndexWidth(next);
  }, []);

  const onIndexResizeEnd = useCallback(() => {
    dragRef.current = null;
  }, []);

  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [editTags, setEditTags] = useState<string[]>([]);
  const [editStatus, setEditStatus] = useState<NoteStatus>("open");
  const [tagInput, setTagInput] = useState("");
  const [tagError, setTagError] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [saveError, setSaveError] = useState<AppError | null>(null);
  const [conflictBusy, setConflictBusy] = useState(false);
  const [detailError, setDetailError] = useState<AppError | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [toast, setToast] = useState<ToastState | null>(null);
  const [view, setView] = useState<NotesView>("notes");
  const [bulkMode, setBulkMode] = useState(false);
  const [selectedNoteKeys, setSelectedNoteKeys] = useState<Set<string>>(() => new Set());
  const [bulkConfirm, setBulkConfirm] = useState(false);
  const [bulkDeleting, setBulkDeleting] = useState(false);
  const [askAnnotationBusy, setAskAnnotationBusy] = useState(false);
  const [clarifyDraft, setClarifyDraft] = useState("");
  const [clarifySource, setClarifySource] = useState<{ quote: string; locator: SourceLocator | null } | null>(null);
  const toastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const editTargetRef = useRef<NoteItem | null>(null);
  /** 目标快照:切条目/清空编辑态后,迟到的自动保存仍按 key 找到正确目标。 */
  const saveTargetsRef = useRef(new Map<string, NoteItem>());
  const editStateRef = useRef<{ body: string; tags: string[]; status: NoteStatus }>({
    body: "",
    tags: [],
    status: "open",
  });
  const lastRevisionRef = useRef(0);

  const showToast = useCallback((kind: ToastState["kind"], message: string) => {
    setToast({ kind, message });
    if (toastTimerRef.current !== null) clearTimeout(toastTimerRef.current);
    toastTimerRef.current = setTimeout(() => setToast(null), 4500);
  }, []);

  useEffect(
    () => () => {
      if (toastTimerRef.current !== null) clearTimeout(toastTimerRef.current);
    },
    [],
  );

  // 面板显示时才发请求:面板隐藏(切到 AI 助手/资料)时这些查询不激活,
  // 否则每次切换文档都会连带重拉五份与阅读无关的数据。
  const queryEnabled = documentId !== null && active;
  const annotations = useQuery({
    queryKey: qk.documentAnnotations(documentId ?? "none"),
    queryFn: () => client.call<AnnotationView[]>("annotation_list", { documentId: documentId! }),
    enabled: queryEnabled,
  });
  const notes = useQuery({
    queryKey: qk.documentNotes(documentId ?? "none"),
    queryFn: () => client.call<Page<NoteView>>("note_list", { documentId: documentId! }),
    enabled: queryEnabled,
  });
  const position = useQuery({
    queryKey: qk.readerPosition(documentId ?? "none"),
    queryFn: () =>
      client.call<ReadingPosition | null>("reader_get_position", { documentId: documentId! }),
    enabled: queryEnabled,
  });
  // 书签 / 待澄清:面板渲染与顶部计数共用同一查询缓存。
  const bookmarks = useQuery({
    queryKey: qk.documentBookmarks(documentId ?? "none"),
    queryFn: () => client.call<BookmarkView[]>("bookmark_list", { documentId: documentId! }),
    enabled: queryEnabled,
  });
  const clarifications = useQuery({
    queryKey: qk.documentClarifications(documentId ?? "none"),
    queryFn: () =>
      client.call<Page<ClarificationView>>("clarification_list", { documentId: documentId! }),
    enabled: queryEnabled,
  });
  const clarificationCount = clarifications.data?.items.length ?? 0;

  // 阅读器创建/更新/删除标注 → 立即写入本地缓存(不闪),再后台重拉对齐。
  useEffect(() => {
    const onInvalidate = (event: Event) => {
      if (documentId === null) return;
      const detail = (event as CustomEvent<AnnotationChangeDetail | null | undefined>).detail;
      if (detail != null) {
        queryClient.setQueryData<AnnotationView[]>(
          qk.documentAnnotations(documentId),
          (previous) => {
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
          },
        );
      }
      if (detail == null) {
        // 旧式广播不带增量时才全量重拉;带 detail 的已经就地合并。
        void queryClient.invalidateQueries({ queryKey: qk.documentAnnotations(documentId) });
      }
    };
    window.addEventListener("knowwise-annotations-changed", onInvalidate);
    return () => window.removeEventListener("knowwise-annotations-changed", onInvalidate);
  }, [queryClient, documentId]);

  const items = useMemo(
    () => buildNoteItems(annotations.data ?? [], notes.data?.items ?? []),
    [annotations.data, notes.data],
  );
  const filtered = useMemo(() => {
    const sinceMs =
      timeFilter === "7d"
        ? Date.now() - 7 * 24 * 60 * 60 * 1000
        : timeFilter === "30d"
          ? Date.now() - 30 * 24 * 60 * 60 * 1000
          : null;
    return filterNoteItems(items, {
      kind: filter,
      keyword: search,
      tag: tagFilter === "" ? null : tagFilter,
      status:
        statusFilter === "open" ||
        statusFilter === "confirmed" ||
        statusFilter === "needs_clarification"
          ? statusFilter
          : null,
      origin:
        originFilter === "user" ||
        originFilter === "ai_draft" ||
        originFilter === "user_edited_ai"
          ? originFilter
          : null,
      sinceMs,
      sort,
    });
  }, [items, filter, search, tagFilter, statusFilter, originFilter, timeFilter, sort]);
  const availableTags = useMemo(() => collectTags(items), [items]);
  const selected = useMemo(
    () => items.find((item) => item.key === selectedKey) ?? null,
    [items, selectedKey],
  );

  // —— 自动保存 ——

  const performSave = useCallback(
    async (payload: NoteDraft, idempotencyKey: string): Promise<SaveOutcome> => {
      const target =
        payload.targetKey !== undefined
          ? (saveTargetsRef.current.get(payload.targetKey) ?? null)
          : editTargetRef.current;
      if (documentId === null || target === null) throw new Error("NO_EDIT_TARGET");
      const revision = payload.expectedRevision;
      setSaveState("saving");
      try {
        let nextRevision: number;
        if (target.annotation !== null) {
          const annotation = target.annotation;
          // 编辑的是「备注」:涂鸦/文本框载荷原样保留,高亮/下划线清掉旧备注
          // 文本但保留样式前缀,保证分类仍是高亮/下划线而不是普通笔记。
          const style = decodeStyleNote(annotation.noteText).style;
          const styleOnly =
            !annotation.noteText.startsWith(INK_PREFIX) &&
            decodeTextBox(annotation.noteText) === null &&
            annotation.quoteText !== "便签";
          const updated = await client.call<AnnotationView | null | undefined>(
            "annotation_update",
            {
              annotationId: annotation.id,
              color: annotation.color,
              noteText: styleOnly
                ? encodeStyleNote(style, "")
                : annotation.noteText,
              remark: payload.body,
              expectedRevision: revision,
            },
          );
          nextRevision =
            updated != null && typeof updated.revision === "number"
              ? updated.revision
              : revision + 1;
          await queryClient.invalidateQueries({ queryKey: qk.documentAnnotations(documentId) });
          window.dispatchEvent(new CustomEvent("knowwise-annotations-changed"));
        } else {
          const note = target.note;
          if (note === null) throw new Error("NO_EDIT_TARGET");
          const saved = await client.call<NoteView | null | undefined>("note_save", {
            documentId,
            noteId: note.id,
            body: payload.body,
            origin: note.origin,
            tags: payload.tags,
            status: payload.status ?? note.status,
            sourceRefs: [],
            expectedRevision: revision,
            idempotencyKey,
          });
          nextRevision =
            saved != null && typeof saved.revision === "number"
              ? saved.revision
              : revision + 1;
          await queryClient.invalidateQueries({ queryKey: qk.documentNotes(documentId) });
        }
        if (editTargetRef.current === target) {
          lastRevisionRef.current = nextRevision;
          setSaveState("saved");
          setSaveError(null);
        }
        return { noteId: target.note?.id ?? target.key, revision: nextRevision };
      } catch (error) {
        const appError = toAppError(error);
        if (editTargetRef.current === target) {
          setSaveState(appError.code === "CONFLICT_REVISION_MISMATCH" ? "conflict" : "error");
          setSaveError(appError);
        }
        throw error;
      }
    },
    [client, documentId, queryClient],
  );

  const performSaveRef = useRef(performSave);
  useEffect(() => {
    performSaveRef.current = performSave;
  }, [performSave]);

  const controller = useMemo(() => {
    if (documentId === null) return null;
    return createAutosaveController({
      delayMs: NOTES_AUTOSAVE_DELAY_MS,
      save: (payload, idempotencyKey) => performSaveRef.current(payload, idempotencyKey),
      newIdempotencyKey: () => client.newIdempotencyKey(),
    });
  }, [documentId, client]);

  const controllerRef = useRef(controller);
  controllerRef.current = controller;

  // 卸载 / 切文档:先 flush 再销毁定时器(绝不丢最后一段编辑)。
  useEffect(() => {
    return () => {
      const current = controllerRef.current;
      if (current !== null) {
        void current.flushNow();
        current.dispose();
      }
    };
  }, [controller]);

  const flushEdits = useCallback(async () => {
    const current = controllerRef.current;
    if (current !== null) await current.flushNow();
  }, []);

  const scheduleSave = useCallback(() => {
    const current = controllerRef.current;
    const target = editTargetRef.current;
    if (current === null || target === null) return;
    saveTargetsRef.current.set(target.key, target);
    current.update({
      noteId: target.note?.id,
      annotationId: target.annotation?.id,
      targetKey: target.key,
      body: editStateRef.current.body,
      tags: editStateRef.current.tags,
      status: editStateRef.current.status,
      expectedRevision: lastRevisionRef.current,
    });
    setSaveState("unsaved");
  }, []);

  const openEditor = useCallback((item: NoteItem) => {
    const body = itemBody(item);
    const status: NoteStatus = item.note?.status ?? "open";
    editTargetRef.current = item;
    editStateRef.current = { body, tags: [...item.tags], status };
    lastRevisionRef.current = itemRevision(item);
    setSelectedKey(item.key);
    setDraft(body);
    setEditTags([...item.tags]);
    setEditStatus(status);
    setTagInput("");
    setTagError(null);
    setConfirmDelete(false);
    setDetailError(null);
    setSaveError(null);
    setSaveState("idle");
    setEditing(true);
  }, []);

  /** 冲突:保留我的版本 = 读取最新 revision 后立即写入(不覆盖别人的中间版本)。 */
  async function keepMine(): Promise<void> {
    const target = editTargetRef.current;
    if (target === null || documentId === null) return;
    setConflictBusy(true);
    try {
      let revision = lastRevisionRef.current;
      if (target.annotation !== null) {
        const list = await client.call<AnnotationView[]>("annotation_list", { documentId });
        const latest = list.find((item) => item.id === target.annotation!.id);
        if (latest === undefined) {
          showToast("error", "原条目已被删除;可选择另存为副本");
          return;
        }
        revision = latest.revision;
      } else if (target.note !== null) {
        const latest = await client.call<NoteView>("note_get", { noteId: target.note.id });
        revision = latest.revision;
      }
      lastRevisionRef.current = revision;
      setSaveError(null);
      setSaveState("unsaved");
      scheduleSave();
      await flushEdits();
    } catch (error) {
      setSaveError(toAppError(error));
    } finally {
      setConflictBusy(false);
    }
  }

  /** 冲突:另存为副本 = 新建一条普通笔记,保留本地内容,不改外部版本。 */
  async function saveMineAsCopy(): Promise<void> {
    if (documentId === null) return;
    setConflictBusy(true);
    try {
      const state = editStateRef.current;
      const created = await client.call<NoteView>("note_save", {
        documentId,
        body: state.body,
        origin: "user",
        tags: state.tags,
        status: state.status,
        sourceRefs: [],
        expectedRevision: 0,
        idempotencyKey: client.newIdempotencyKey(),
      });
      controllerRef.current?.discard();
      await queryClient.invalidateQueries({ queryKey: qk.documentNotes(documentId) });
      const item = buildNoteItems([], [created])[0];
      if (item !== undefined) openEditor(item);
      setSaveError(null);
      setSaveState("idle");
      showToast("success", "已另存为新笔记");
    } catch (error) {
      setSaveError(toAppError(error));
      showToast("error", "另存为副本失败,请重试");
    } finally {
      setConflictBusy(false);
    }
  }

  /** 冲突:放弃本地编辑,加载最新内容到编辑器。 */
  async function discardMine(): Promise<void> {
    const target = editTargetRef.current;
    if (target === null || documentId === null) return;
    setConflictBusy(true);
    try {
      if (target.annotation !== null) {
        const list = await client.call<AnnotationView[]>("annotation_list", { documentId });
        const latest = list.find((item) => item.id === target.annotation!.id);
        if (latest === undefined) {
          showToast("error", "原条目已被删除;可选择另存为副本");
          return;
        }
        const body = annotationBody(latest);
        editStateRef.current = { body, tags: [], status: "open" };
        setDraft(body);
        setEditTags([]);
        setEditStatus("open");
        lastRevisionRef.current = latest.revision;
        await queryClient.invalidateQueries({ queryKey: qk.documentAnnotations(documentId) });
      } else if (target.note !== null) {
        const latest = await client.call<NoteView>("note_get", { noteId: target.note.id });
        editStateRef.current = { body: latest.body, tags: [...latest.tags], status: latest.status };
        setDraft(latest.body);
        setEditTags([...latest.tags]);
        setEditStatus(latest.status);
        lastRevisionRef.current = latest.revision;
        await queryClient.invalidateQueries({ queryKey: qk.documentNotes(documentId) });
      }
      controllerRef.current?.discard();
      setSaveError(null);
      setSaveState("idle");
      showToast("success", "已放弃本地修改,显示最新内容");
    } catch (error) {
      setSaveError(toAppError(error));
    } finally {
      setConflictBusy(false);
    }
  }

  const selectItem = useCallback(
    (key: string) => {
      if (key === selectedKey && editing) return;
      void flushEdits();
      editTargetRef.current = null;
      setSelectedKey(key);
      setEditing(false);
      setConfirmDelete(false);
      setDetailError(null);
      setSaveError(null);
      setSaveState("idle");
      setTagInput("");
      setTagError(null);
    },
    [editing, flushEdits, selectedKey],
  );

  // 切换文档:清空选择与编辑态(controller 的清理会先 flush 旧文档的编辑)。
  useEffect(() => {
    editTargetRef.current = null;
    saveTargetsRef.current.clear();
    setSelectedKey(null);
    setEditing(false);
    setMobileDetail(false);
    setSaveState("idle");
    setSaveError(null);
    setDetailError(null);
    setConfirmDelete(false);
    setTagError(null);
  }, [documentId]);

  // 选中项消失(例如被删)时退出编辑态。
  useEffect(() => {
    if (selected === null) {
      setEditing(false);
      editTargetRef.current = null;
      setSaveState("idle");
    }
  }, [selected]);

  // 划词「笔记」/便签 → 选中并进入编辑(等待列表数据就绪)。
  const focusAnnotationId = useReaderTools((state) => state.focusAnnotationId);
  const consumeFocus = useReaderTools((state) => state.consumeFocusAnnotation);
  const pendingFocusRef = useRef<string | null>(null);

  useEffect(() => {
    if (focusAnnotationId === null) return;
    pendingFocusRef.current = focusAnnotationId;
    consumeFocus();
  }, [focusAnnotationId, consumeFocus]);

  useEffect(() => {
    const key = pendingFocusRef.current;
    if (key === null) return;
    const item = items.find((candidate) => candidate.key === key);
    if (item === undefined) return;
    pendingFocusRef.current = null;
    openEditor(item);
    setMobileDetail(true);
  }, [items, focusAnnotationId, openEditor]);

  // 页边笔记标记 → 打开并聚焦对应笔记。
  const focusNoteId = useReaderTools((state) => state.focusNoteId);
  const consumeFocusNote = useReaderTools((state) => state.consumeFocusNote);
  const pendingNoteFocusRef = useRef<string | null>(null);
  useEffect(() => {
    if (focusNoteId === null) return;
    pendingNoteFocusRef.current = focusNoteId;
    consumeFocusNote();
  }, [focusNoteId, consumeFocusNote]);
  useEffect(() => {
    const noteId = pendingNoteFocusRef.current;
    if (noteId === null) return;
    if (!items.some((item) => item.note?.id === noteId)) return;
    pendingNoteFocusRef.current = null;
    openNoteById(noteId);
  }, [items, focusNoteId]);

  // 划词「待澄清」→ 切到待澄清视图并预填原文/位置。
  const pendingClarification = useReaderTools((state) => state.pendingClarification);
  useEffect(() => {
    if (pendingClarification === null) return;
    useReaderTools.getState().consumePendingClarification();
    setView("clarifications");
    setMobileDetail(false);
    setClarifySource({
      quote: pendingClarification.quote,
      locator: pendingClarification.locator,
    });
    setClarifyDraft(`关于「${pendingClarification.quote.slice(0, 120)}」:`);
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(() => document.getElementById("clarify-input")?.focus());
    }
  }, [pendingClarification]);

  // 更多菜单:点击外部 / Esc 关闭。
  useEffect(() => {
    if (!menuOpen) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as HTMLElement | null;
      if (target === null || target.closest(".notes-more") === null) setMenuOpen(false);
    };
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    window.addEventListener("mousedown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [menuOpen]);

  // —— 写操作 ——

  async function createNote(
    body: string,
    afterCreate?: (created: NoteView) => void,
    options: {
      tags?: string[];
      status?: NoteStatus;
      locator?: SourceLocator | null;
      displayName?: string;
    } = {},
  ) {
    if (documentId === null) return;
    const locator =
      options.locator !== undefined ? options.locator : (position.data?.locator ?? null);
    const sourceRefs: SourceReference[] =
      locator != null
        ? [
            {
              sourceType: "selection",
              sourceId: `pos-${client.newIdempotencyKey()}`,
              sourceRole: "original",
              displayName:
                options.displayName ?? locatorLabel(locator) ?? "当前位置",
              citationIds: [],
              snapshotAvailable: true,
            },
          ]
        : [];
    const created = await client.call<NoteView>("note_save", {
      documentId,
      body,
      origin: "user",
      tags: options.tags ?? [],
      status: options.status ?? "open",
      sourceRefs,
      expectedRevision: 0,
      idempotencyKey: client.newIdempotencyKey(),
    });
    // 先本地插入缓存,保证新笔记立刻出现在列表里(随后 invalidate 再对齐服务端)。
    queryClient.setQueryData<Page<NoteView>>(qk.documentNotes(documentId), (previous) => ({
      ...(previous ?? {}),
      items: [...(previous?.items ?? []).filter((item) => item.id !== created.id), created],
    }));
    await queryClient.invalidateQueries({ queryKey: qk.documentNotes(documentId) });
    afterCreate?.(created);
  }

  /** 快速笔记/新建笔记失败后的重试入口(保留最后一段输入,不静默)。 */
  const quickRetryRef = useRef<null | (() => Promise<void>)>(null);

  async function saveQuickNote() {
    const body = quickDraft.trim();
    if (body === "" || documentId === null) return;
    setQuickBusy(true);
    setQuickError(null);
    quickRetryRef.current = saveQuickNote;
    try {
      await createNote(body, (created) => {
        setQuickDraft("");
        const item: NoteItem = buildNoteItems([], [created])[0]!;
        openEditor(item);
        showToast("success", "已记入笔记");
      });
      quickRetryRef.current = null;
    } catch (error) {
      setQuickError(toAppError(error));
    } finally {
      setQuickBusy(false);
    }
  }

  async function submitQuick(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await saveQuickNote();
  }

  async function startNewNote() {
    if (documentId === null) return;
    setQuickBusy(true);
    setQuickError(null);
    const retry = async () => {
      await startNewNote();
    };
    quickRetryRef.current = retry;
    try {
      await createNote("新笔记", (created) => {
        const item: NoteItem = buildNoteItems([], [created])[0]!;
        openEditor(item);
      });
      quickRetryRef.current = null;
    } catch (error) {
      setQuickError(toAppError(error));
    } finally {
      setQuickBusy(false);
    }
  }

  /** 打开刚创建的笔记(列表缓存尚未刷新也能直接进入)。 */
  function openCreatedNote(created: NoteView): void {
    const item = buildNoteItems([], [created])[0];
    if (item === undefined) return;
    setView("notes");
    openEditor(item);
    setMobileDetail(true);
  }

  function openNoteById(noteId: string): void {
    const item = items.find((candidate) => candidate.note?.id === noteId);
    if (item === undefined) {
      showToast("error", "关联笔记不存在或已被删除");
      return;
    }
    setView("notes");
    openEditor(item);
    setMobileDetail(true);
  }

  /** 书签 → 笔记:把位置与显示名带进一条新笔记,形成可回看的连接。 */
  async function createNoteFromBookmark(bookmark: BookmarkView): Promise<void> {
    if (documentId === null) return;
    const where = locatorLabel(bookmark.locator) ?? "当前位置";
    try {
      await createNote(
        `### 书签:${bookmark.label || where}\n\n> ${where}\n\n`,
        openCreatedNote,
        {
          tags: ["书签"],
          locator: bookmark.locator,
          displayName: where,
        },
      );
      showToast("success", "已从书签创建笔记");
    } catch (error) {
      showToast("error", `创建笔记失败:${errorText(error)}`);
    }
  }

  /** 新增待澄清(可带划词来源)。 */
  async function addClarification(): Promise<void> {
    if (documentId === null) return;
    const question = clarifyDraft.trim();
    if (question === "") return;
    try {
      await client.call("clarification_create", {
        documentId,
        questionText: question,
        sourceSnapshot: {
          text: clarifySource?.quote ?? "",
          ...(clarifySource?.locator != null ? { locator: clarifySource.locator } : {}),
        },
        idempotencyKey: client.newIdempotencyKey(),
      });
      setClarifyDraft("");
      setClarifySource(null);
      await queryClient.invalidateQueries({ queryKey: qk.documentClarifications(documentId) });
      showToast("success", "已记入待澄清");
    } catch (error) {
      showToast("error", `待澄清未保存:${errorText(error)}`);
    }
  }

  /** 待澄清 → 解答案 → 笔记:状态变为 resolved 并关联新笔记。 */
  async function resolveClarification(item: ClarificationView, answer: string): Promise<void> {
    if (documentId === null) return;
    const text = answer.trim();
    if (text === "") return;
    const quote = item.sourceSnapshot.text;
    const body = quote === ""
      ? `### ${item.questionText}\n\n${text}\n`
      : `### ${item.questionText}\n\n${text}\n\n> ${quote}\n`;
    try {
      const created = await client.call<NoteView>("note_save", {
        documentId,
        body,
        origin: "user",
        tags: ["待澄清"],
        status: "confirmed",
        sourceRefs:
          item.sourceSnapshot.locator != null
            ? [
                {
                  sourceType: "selection",
                  sourceId: `clarify-${client.newIdempotencyKey()}`,
                  sourceRole: "original",
                  displayName: locatorLabel(item.sourceSnapshot.locator) ?? "待澄清来源",
                  citationIds: [],
                  snapshotAvailable: true,
                },
              ]
            : [],
        expectedRevision: 0,
        idempotencyKey: client.newIdempotencyKey(),
      });
      await client.call("clarification_update", {
        clarificationId: item.id,
        status: "resolved",
        resolutionRef: { refType: "note", refId: created.id },
        expectedRevision: item.revision,
      });
      await queryClient.invalidateQueries({ queryKey: qk.documentNotes(documentId) });
      await queryClient.invalidateQueries({ queryKey: qk.documentClarifications(documentId) });
      showToast("success", "已解决,并保存为笔记");
    } catch (error) {
      showToast("error", `解决失败:${errorText(error)}`);
    }
  }

  async function setClarificationStatus(
    item: ClarificationView,
    status: "pending" | "deferred",
  ): Promise<void> {
    if (documentId === null) return;
    try {
      await client.call("clarification_update", {
        clarificationId: item.id,
        status,
        expectedRevision: item.revision,
      });
      await queryClient.invalidateQueries({ queryKey: qk.documentClarifications(documentId) });
    } catch (error) {
      showToast("error", `状态未更新:${errorText(error)}`);
    }
  }

  async function setClarificationRemark(
    item: ClarificationView,
    remark: string,
  ): Promise<void> {
    if (documentId === null) return;
    await client.call<ClarificationView>("clarification_set_remark", {
      clarificationId: item.id,
      remark,
      expectedRevision: item.revision,
    });
    await queryClient.invalidateQueries({ queryKey: qk.documentClarifications(documentId) });
    showToast("success", "备注已保存");
  }

  async function removeClarification(item: ClarificationView): Promise<void> {
    if (documentId === null) return;
    try {
      await client.call("clarification_delete", {
        clarificationId: item.id,
        expectedRevision: item.revision,
      });
      await queryClient.invalidateQueries({ queryKey: qk.documentClarifications(documentId) });
    } catch (error) {
      showToast("error", `删除失败:${errorText(error)}`);
    }
  }

  function askAiAboutClarification(item: ClarificationView): void {
    const quote = item.sourceSnapshot.text;
    useWorkspaceUi.getState().setDraft(
      quote === ""
        ? `${item.questionText}\n\n`
        : `关于「${quote.slice(0, 200)}」:${item.questionText}\n\n`,
    );
    useWorkspaceUi.getState().setAgentOpen(true);
  }

  /**
   * 标注「问 AI」:先确保存在一条与该标注同 anchor 的笔记(已有则复用,
   * 不产生重复笔记),再把笔记作为本轮来源。标注/笔记不是原文事实,
   * 草稿与来源标题都明确写出这一点。
   */
  async function askAiAboutAnnotation(annotation: AnnotationView): Promise<void> {
    if (documentId === null || askAnnotationBusy) return;
    setAskAnnotationBusy(true);
    try {
      let note = notes.data?.items.find(
        (candidate) => candidate.anchorId === annotation.anchorId,
      );
      if (note === undefined) {
        const quote =
          annotation.quoteText !== "" && annotation.quoteText !== "便签"
            ? annotation.quoteText
            : "";
        // 可读备注优先;涂鸦载荷永不当正文。原文引用另外放在 blockquote。
        const noteText = annotationBody(annotation);
        const body = [
          "### 标注笔记",
          "",
          quote !== "" ? `> ${quote}` : "",
          quote !== "" ? "" : "",
          noteText !== "" ? noteText : "(这条标注没有备注;以下提问基于标注位置。)",
          "",
          "> 说明:这是个人标注/笔记,不是原文事实。",
          "",
        ]
          .filter((line, index) => !(line === "" && index === 2 && quote === ""))
          .join("\n");
        const sourceRefs: SourceReference[] = [
          {
            sourceType: "annotation",
            sourceId: annotation.id,
            sourceRole: "user_note",
            displayName: "标注",
            citationIds: [],
            snapshotAvailable: true,
          },
          ...(annotation.locator !== null
            ? [
                {
                  sourceType: "selection" as const,
                  sourceId: `anchor-${annotation.anchorId}`,
                  sourceRole: "original" as const,
                  displayName: locatorLabel(annotation.locator) ?? "标注位置",
                  citationIds: [],
                  snapshotAvailable: true,
                },
              ]
            : []),
        ];
        note = await client.call<NoteView>("note_save", {
          documentId,
          anchorId: annotation.anchorId,
          body,
          origin: "user",
          tags: ["标注"],
          status: "open",
          sourceRefs,
          expectedRevision: 0,
          idempotencyKey: client.newIdempotencyKey(),
        });
        queryClient.setQueryData<Page<NoteView>>(
          qk.documentNotes(documentId),
          (previous) => ({
            ...(previous ?? {}),
            items: [
              ...(previous?.items ?? []).filter((item) => item.id !== note!.id),
              note!,
            ],
          }),
        );
        void queryClient.invalidateQueries({ queryKey: qk.documentNotes(documentId) });
      }
      const label = annotation.quoteText !== "" && annotation.quoteText !== "便签"
        ? annotation.quoteText.slice(0, 24)
        : "标注";
      useWorkspaceUi.getState().addNoteSource({
        id: note.id,
        title: `标注笔记 · ${label}(非原文事实)`,
        documentId,
      });
      useWorkspaceUi.getState().setDraft(
        "请基于引用的这条标注笔记(个人标注/笔记,不是原文事实)回答;哪些结论需要回到原文核实,请明确标出:",
      );
      useWorkspaceUi.getState().setAgentOpen(true);
      showToast("success", "已把标注笔记加入本轮来源");
    } catch (error) {
      showToast("error", `标注笔记未建立:${errorText(error)}`);
    } finally {
      setAskAnnotationBusy(false);
    }
  }

  function toggleNoteSelected(key: string): void {
    setSelectedNoteKeys((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
    setBulkConfirm(false);
  }

  async function deleteSelectedNotes(): Promise<void> {
    if (documentId === null || selectedNoteKeys.size === 0) return;
    setBulkDeleting(true);
    await flushEdits();
    const targets = items.filter(
      (item) => selectedNoteKeys.has(item.key) && item.note !== null,
    );
    let failed = 0;
    for (const item of targets) {
      try {
        await client.call<DeleteResult>("note_delete", {
          noteId: item.note!.id,
          expectedRevision: item.note!.revision,
        });
      } catch {
        failed += 1;
      }
    }
    await queryClient.invalidateQueries({ queryKey: qk.documentNotes(documentId) });
    if (selected !== null && selectedNoteKeys.has(selected.key)) {
      setSelectedKey(null);
      setEditing(false);
      editTargetRef.current = null;
    }
    setSelectedNoteKeys(new Set());
    setBulkMode(false);
    setBulkConfirm(false);
    setBulkDeleting(false);
    if (failed > 0) {
      showToast("error", `${failed} 条笔记删除失败,请重试`);
    } else {
      showToast("success", `已删除 ${targets.length} 条笔记`);
    }
  }

  async function deleteSelected() {
    if (selected === null || documentId === null) return;
    setBusy(true);
    setDetailError(null);
    try {
      await flushEdits();
      if (selected.annotation !== null) {
        await client.call<DeleteResult>("annotation_delete", {
          annotationId: selected.annotation.id,
          expectedRevision: selected.annotation.revision,
        });
        await queryClient.invalidateQueries({ queryKey: qk.documentAnnotations(documentId) });
        window.dispatchEvent(new CustomEvent("knowwise-annotations-changed"));
      } else if (selected.note !== null) {
        await client.call<DeleteResult>("note_delete", {
          noteId: selected.note.id,
          expectedRevision: selected.note.revision,
        });
        await queryClient.invalidateQueries({ queryKey: qk.documentNotes(documentId) });
      }
      setSelectedKey(null);
      setEditing(false);
      editTargetRef.current = null;
      setConfirmDelete(false);
      showToast("success", "已删除");
    } catch (error) {
      setDetailError(toAppError(error));
    } finally {
      setBusy(false);
    }
  }

  async function exportMarkdown() {
    setMenuOpen(false);
    if (documentId === null) return;
    setExporting(true);
    try {
      const path = await dialogSavePath({
        defaultPath: "笔记.md",
        filters: [{ name: "Markdown", extensions: ["md", "markdown"] }],
      });
      if (path === null) {
        showToast("success", "已取消导出");
        return;
      }
      const result = await client.call<{ bytesWritten: number; path: string }>(
        "notes_export_markdown",
        {
          documentId,
          path,
          scope: "document",
          includeAnnotations: true,
        },
      );
      showToast("success", `已导出 Markdown:${result.path}`);
    } catch (error) {
      showToast("error", `导出失败:${errorText(error)}`);
    } finally {
      setExporting(false);
    }
  }

  async function insertScreenClipFromMenu() {
    setMenuOpen(false);
    if (documentId === null) return;
    if (selected === null) {
      showToast("error", "请先在列表中选择一条笔记,或在「新建」后截图");
      return;
    }
    if (!clip.supported) {
      showToast("error", "当前环境不支持屏幕截图(仅桌面应用可用)");
      return;
    }
    const url = await clip.requestClip();
    if (url === null) {
      showToast("success", "已取消截图");
      return;
    }
    // 标注正文后端上限 2000 字,放不下 data-URI 截图:自动存进新普通笔记,
    // 不把"插入 → 保存失败"留给用户。
    if (selected.annotation !== null) {
      try {
        await createNote(`![截图](${url})`, (created) => {
          const item = buildNoteItems([], [created])[0]!;
          openEditor(item);
          showToast("success", "标注正文放不下截图,已存入新笔记");
        });
      } catch (error) {
        showToast("error", `截图保存失败:${errorText(error)}`);
      }
      return;
    }
    const imageBudget = MAX_NOTE_BODY_CHARS - 32;
    if (url.length > imageBudget) {
      showToast("error", "截图压缩后仍然过大,未插入;可缩小框选范围后重试");
      return;
    }
    if (!editing) openEditor(selected);
    const base = editing ? editStateRef.current.body : itemBody(selected);
    const trimmed = base.replace(/\s+$/, "");
    if (trimmed.length + url.length + 16 > MAX_NOTE_BODY_CHARS) {
      showToast("error", "笔记正文已接近上限,截图放不下;可先精简正文或新建笔记");
      return;
    }
    const next = `${trimmed}${trimmed === "" ? "" : "\n\n"}![截图](${url})\n`;
    setDraft(next);
    editStateRef.current.body = next;
    scheduleSave();
    showToast("success", "截图已插入笔记");
  }

  // —— 编辑器事件 ——

  function handleDraftChange(next: string) {
    setDraft(next);
    editStateRef.current.body = next;
    scheduleSave();
  }

  function changeStatus(next: NoteStatus) {
    setEditStatus(next);
    editStateRef.current.status = next;
    scheduleSave();
  }

  function applyTags(next: string[]) {
    setEditTags(next);
    editStateRef.current.tags = next;
    scheduleSave();
  }

  function submitTag() {
    const result = addTags(editTags, tagInput);
    if (!result.ok) {
      setTagError(TAG_ERROR_LABELS[result.reason]);
      return;
    }
    setTagError(null);
    setTagInput("");
    applyTags(result.tags);
  }

  function finishEditing() {
    void flushEdits();
    editTargetRef.current = null;
    setEditing(false);
  }

  function handleIndexKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Enter") {
      if (selectedKey !== null) {
        event.preventDefault();
        setMobileDetail(true);
      }
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    if (filtered.length === 0) return;
    const currentIndex = filtered.findIndex((item) => item.key === selectedKey);
    const delta = event.key === "ArrowDown" ? 1 : -1;
    const start = currentIndex === -1 ? (delta === 1 ? -1 : filtered.length) : currentIndex;
    const nextIndex = Math.min(filtered.length - 1, Math.max(0, start + delta));
    const next = filtered[nextIndex];
    if (next === undefined) return;
    selectItem(next.key);
    const focus = () => {
      const options = indexRef.current?.querySelectorAll<HTMLElement>('[role="option"]');
      options?.[nextIndex]?.focus();
    };
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(focus);
    else setTimeout(focus, 0);
  }

  if (documentId === null) {
    return (
      <section className="notes-dock dock-notes" aria-label="笔记面板">
        <div className="notes-no-document">
          <NotebookPen className="w-6 h-6" aria-hidden />
          <p>打开一个文档后,这里就是你的笔记本。</p>
          <p className="notes-hint">划词做标注,或直接用上方快速笔记记一笔。</p>
        </div>
      </section>
    );
  }

  const detailVisible = selected !== null && (!narrow || mobileDetail);
  const bodyPane = narrow ? (detailVisible ? "detail" : "list") : "split";
  const saveLabel = SAVE_STATE_LABELS[saveState];

  return (
    <section
      className={`notes-dock dock-notes${narrow ? " notes-narrow" : ""}`}
      aria-label="笔记面板"
      ref={rootRef}
    >
      <header className="notes-toolbar">
        <div className="notes-views" role="tablist" aria-label="笔记视图">
          <button
            type="button"
            role="tab"
            aria-selected={view === "notes"}
            aria-label="笔记"
            className={`notes-view-tab${view === "notes" ? " active" : ""}`}
            onClick={() => setView("notes")}
          >
            <NotebookPen className="w-3.5 h-3.5" aria-hidden />
            <span>笔记</span>
            <span className="notes-view-count">{items.length}</span>
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={view === "bookmarks"}
            aria-label="书签"
            className={`notes-view-tab${view === "bookmarks" ? " active" : ""}`}
            onClick={() => setView("bookmarks")}
          >
            <Bookmark className="w-3.5 h-3.5" aria-hidden />
            <span>书签</span>
            <span className="notes-view-count">{bookmarks.data?.length ?? 0}</span>
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={view === "clarifications"}
            aria-label="待澄清"
            className={`notes-view-tab${view === "clarifications" ? " active" : ""}`}
            onClick={() => setView("clarifications")}
          >
            <CircleHelp className="w-3.5 h-3.5" aria-hidden />
            <span>待澄清</span>
            <span className="notes-view-count">{clarificationCount}</span>
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={view === "graph"}
            aria-label="知识图"
            className={`notes-view-tab${view === "graph" ? " active" : ""}`}
            onClick={() => setView("graph")}
          >
            <Share2 className="w-3.5 h-3.5" aria-hidden />
            <span>知识图</span>
          </button>
        </div>
        {view === "notes" ? (
        <>
        <form className="notes-quick" aria-label="快速笔记" onSubmit={(event) => void submitQuick(event)}>
          <input
            type="text"
            value={quickDraft}
            placeholder="快速记一笔…(Enter 保存)"
            aria-label="快速笔记"
            onChange={(event) => setQuickDraft(event.target.value)}
            disabled={quickBusy}
          />
          <button
            type="submit"
            className="btn small primary notes-quick-submit"
            disabled={quickDraft.trim() === "" || quickBusy}
          >
            <Plus className="w-3.5 h-3.5" aria-hidden />
            记下
          </button>
          <button
            type="button"
            className="btn small ghost notes-new-note"
            title="新建一条空白笔记并开始编辑"
            aria-label="新建笔记"
            disabled={quickBusy}
            onClick={() => void startNewNote()}
          >
            <FileText className="w-3.5 h-3.5" aria-hidden />
            新建
          </button>
        </form>
        <div className="notes-toolbar-row">
          <div className="notes-filter" role="tablist" aria-label="筛选">
            {NOTE_KIND_FILTERS.map((entry) => (
              <button
                key={entry.id}
                type="button"
                role="tab"
                aria-selected={filter === entry.id}
                className={`notes-chip${filter === entry.id ? " active" : ""}`}
                onClick={() => setFilter(entry.id)}
              >
                {entry.label}
              </button>
            ))}
          </div>
          <select
            className="notes-tag-filter"
            aria-label="按标签筛选"
            value={tagFilter}
            onChange={(event) => setTagFilter(event.target.value)}
          >
            <option value="">全部标签</option>
            {availableTags.map((tag) => (
              <option key={tag} value={tag}>
                {tag}
              </option>
            ))}
          </select>
          <select
            className="notes-tag-filter"
            aria-label="按状态筛选"
            value={statusFilter}
            onChange={(event) => setStatusFilter(event.target.value)}
          >
            <option value="">全部状态</option>
            <option value="open">待整理</option>
            <option value="confirmed">已确认</option>
            <option value="needs_clarification">待澄清</option>
          </select>
          <select
            className="notes-tag-filter"
            aria-label="按来源筛选"
            value={originFilter}
            onChange={(event) => setOriginFilter(event.target.value)}
          >
            <option value="">全部来源</option>
            <option value="user">用户笔记</option>
            <option value="ai_draft">AI 草稿</option>
            <option value="user_edited_ai">AI 草稿(已编辑)</option>
          </select>
          <select
            className="notes-tag-filter"
            aria-label="按更新时间筛选"
            value={timeFilter}
            onChange={(event) =>
              setTimeFilter(event.target.value === "7d" ? "7d" : event.target.value === "30d" ? "30d" : "all")
            }
          >
            <option value="all">全部时间</option>
            <option value="7d">最近 7 天</option>
            <option value="30d">最近 30 天</option>
          </select>
          <label className="notes-search">
            <Search className="w-3.5 h-3.5" aria-hidden />
            <input
              type="search"
              value={search}
              placeholder="搜索笔记"
              aria-label="搜索笔记"
              onChange={(event) => setSearch(event.target.value)}
            />
          </label>
          <div className="notes-more">
            <button
              type="button"
              className={`icon-btn small${menuOpen ? " active" : ""}`}
              title="更多操作"
              aria-label="更多操作"
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              onClick={() => setMenuOpen((open) => !open)}
            >
              <MoreHorizontal className="w-4 h-4" />
            </button>
            {menuOpen ? (
              <div className="notes-menu" role="menu" aria-label="更多操作">
                <button
                  type="button"
                  role="menuitem"
                  disabled={exporting}
                  onClick={() => void exportMarkdown()}
                >
                  <Download className="w-3.5 h-3.5" aria-hidden />
                  {exporting ? "导出中…" : "导出 Markdown"}
                </button>
                <button
                  type="button"
                  role="menuitem"
                  disabled={!clip.supported}
                  title={
                    clip.supported
                      ? "框选屏幕区域并插入当前笔记"
                      : "当前环境不支持屏幕截图(仅桌面应用可用)"
                  }
                  onClick={() => void insertScreenClipFromMenu()}
                >
                  <Camera className="w-3.5 h-3.5" aria-hidden />
                  截图插入
                </button>
                <div className="notes-menu-sep" role="separator" />
                <span className="notes-menu-label">排序</span>
                {(Object.keys(NOTE_SORT_LABELS) as NoteSortMode[]).map((mode) => (
                  <button
                    key={mode}
                    type="button"
                    role="menuitemradio"
                    aria-checked={sort === mode}
                    onClick={() => {
                      setSort(mode);
                      setMenuOpen(false);
                    }}
                  >
                    <Check className={`w-3.5 h-3.5${sort === mode ? "" : " notes-menu-check-hidden"}`} aria-hidden />
                    {NOTE_SORT_LABELS[mode]}
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        </div>
        </>
        ) : null}
      </header>

      {quickError !== null ? (
        <ErrorAlert
          error={quickError}
          onRetry={
            quickRetryRef.current === null
              ? undefined
              : () => void quickRetryRef.current?.()
          }
        />
      ) : null}

      {view === "notes" ? (
      <div className={`notes-body pane-${bodyPane}`}>
        <div
          className={`notes-index${narrow && mobileDetail ? " hidden-in-narrow" : ""}${
            indexCollapsed && !narrow ? " is-collapsed" : ""
          }`}
          ref={indexRef}
          style={!narrow && !indexCollapsed ? { flexBasis: indexWidth, width: indexWidth } : undefined}
          onKeyDown={handleIndexKeyDown}
        >
          <div className="notes-index-head">
            <button
              type="button"
              className="icon-btn small"
              aria-label={indexCollapsed ? "展开笔记列表" : "收起笔记列表"}
              aria-expanded={!indexCollapsed}
              title={indexCollapsed ? "展开笔记列表" : "收起笔记列表"}
              onClick={() => setIndexCollapsed((value) => !value)}
            >
              {indexCollapsed ? "›" : "‹"}
            </button>
            {!indexCollapsed ? <span className="notes-index-title">笔记</span> : null}
          </div>
          <div className="notes-bulk-bar" role="group" aria-label="批量管理笔记">
            {!bulkMode ? (
              <button
                type="button"
                className="btn tiny ghost"
                onClick={() => {
                  setBulkMode(true);
                  setSelectedNoteKeys(new Set());
                }}
              >
                批量删除
              </button>
            ) : (
              <>
                <label className="notes-bulk-select-all">
                  <input
                    type="checkbox"
                    checked={
                      filtered.some((item) => item.note !== null) &&
                      filtered
                        .filter((item) => item.note !== null)
                        .every((item) => selectedNoteKeys.has(item.key))
                    }
                    onChange={() => {
                      const noteItems = filtered.filter((item) => item.note !== null);
                      const all = noteItems.every((item) => selectedNoteKeys.has(item.key));
                      setSelectedNoteKeys(all ? new Set() : new Set(noteItems.map((item) => item.key)));
                      setBulkConfirm(false);
                    }}
                  />
                  全选当前笔记
                </label>
                <span className="notes-bulk-count">已选 {selectedNoteKeys.size} 条</span>
                {selectedNoteKeys.size > 0 ? (
                  bulkConfirm ? (
                    <span className="notes-bulk-confirm" role="alertdialog" aria-label="确认批量删除笔记">
                      <span>删除所选 {selectedNoteKeys.size} 条笔记?不可恢复。</span>
                      <button
                        type="button"
                        className="btn tiny danger"
                        disabled={bulkDeleting}
                        onClick={() => void deleteSelectedNotes()}
                      >
                        {bulkDeleting ? "删除中…" : "确认删除"}
                      </button>
                      <button
                        type="button"
                        className="btn tiny ghost"
                        disabled={bulkDeleting}
                        onClick={() => setBulkConfirm(false)}
                      >
                        取消
                      </button>
                    </span>
                  ) : (
                    <button
                      type="button"
                      className="btn tiny danger-ghost"
                      onClick={() => setBulkConfirm(true)}
                    >
                      删除所选
                    </button>
                  )
                ) : null}
                <button
                  type="button"
                  className="btn tiny ghost"
                  onClick={() => {
                    setBulkMode(false);
                    setSelectedNoteKeys(new Set());
                    setBulkConfirm(false);
                  }}
                >
                  退出批量
                </button>
              </>
            )}
          </div>
          <div className="notes-index-list" role="listbox" aria-label="笔记索引">
            {annotations.isLoading || notes.isLoading ? (
              <p className="notes-hint center">加载中…</p>
            ) : null}
            {annotations.isError ? <ErrorAlert error={toAppError(annotations.error)} /> : null}
            {notes.isError ? <ErrorAlert error={toAppError(notes.error)} /> : null}
            {filtered.length === 0 && !annotations.isLoading && !notes.isLoading ? (
              <div className="notes-empty">
                <NotebookPen className="w-6 h-6" aria-hidden />
                <p>{search.trim() === "" && tagFilter === "" ? "还没有笔记" : "没有匹配的笔记"}</p>
                <p className="notes-hint">划词点「笔记」,或在上方快速记一笔。</p>
              </div>
            ) : null}
            {filtered.map((item) => {
              const KindIcon = KIND_ICONS[item.kind];
              const isSelected = item.key === selectedKey;
              return (
                <div
                  key={item.key}
                  role="option"
                  aria-selected={isSelected}
                  tabIndex={isSelected ? 0 : -1}
                  data-note-key={item.key}
                  className={`notes-item kind-${item.kind}${isSelected ? " selected" : ""}`}
                  onClick={() => {
                    selectItem(item.key);
                    if (narrow) setMobileDetail(true);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      selectItem(item.key);
                      if (narrow) setMobileDetail(true);
                    }
                  }}
                >
                  <span className={`notes-item-bar kind-${item.kind}`} aria-hidden />
                  {bulkMode && item.note !== null ? (
                    <label
                      className="notes-select"
                      onClick={(event) => event.stopPropagation()}
                    >
                      <input
                        type="checkbox"
                        checked={selectedNoteKeys.has(item.key)}
                        onChange={() => toggleNoteSelected(item.key)}
                        aria-label={`选择笔记:${item.title}`}
                      />
                    </label>
                  ) : null}
                  <span className="notes-item-main">
                    <span className="notes-item-title">
                      <KindIcon className={`notes-item-icon kind-${item.kind}`} aria-hidden />
                      {item.title}
                    </span>
                    {item.summary !== "" ? (
                      <span className="notes-item-summary">{item.summary}</span>
                    ) : null}
                    <span className="notes-item-meta">
                      <span className={`notes-kind-tag kind-${item.kind}`}>
                        {NOTE_KIND_LABELS[item.kind]}
                      </span>
                      {item.tags.slice(0, 3).map((tag) => (
                        <span key={tag} className="notes-tag">
                          {tag}
                        </span>
                      ))}
                      <span className="notes-item-time">{formatTime(item.updatedAt)}</span>
                    </span>
                    {item.location !== null && item.locator !== null ? (
                      <button
                        type="button"
                        className="notes-item-loc"
                        title="回跳到原文位置"
                        onClick={(event) => {
                          event.stopPropagation();
                          jumpToLocator(documentId, item.locator);
                        }}
                      >
                        <MapPin className="w-3 h-3" aria-hidden />
                        {item.location}
                      </button>
                    ) : null}
                    {item.annotation !== null ? (
                      <button
                        type="button"
                        className="notes-item-loc flex items-center gap-1 text-purple-600 dark:text-purple-400 hover:text-purple-700"
                        title="把该标注转成关联笔记并作为本轮来源(不重复建笔记)"
                        disabled={askAnnotationBusy}
                        onClick={(event) => {
                          event.stopPropagation();
                          const annotation = item.annotation;
                          if (annotation !== null) void askAiAboutAnnotation(annotation);
                        }}
                      >
                        <Sparkles className="w-3 h-3 text-purple-500" aria-hidden />
                        <span>问 AI</span>
                      </button>
                    ) : item.note !== null ? (
                    <button
                      type="button"
                      className="notes-item-loc flex items-center gap-1 text-purple-600 dark:text-purple-400 hover:text-purple-700"
                      title="把这则笔记作为本轮来源,向 AI 提问"
                      onClick={(event) => {
                        event.stopPropagation();
                        // 与「引用资料」同一条链路:笔记作为本轮来源
                        // (sourceType=note),回答必须引用它,且不能被
                        // 说成原文事实。只把文字塞进草稿会让模型把笔记
                        // 当成自己的话复述,来源与可核验性都会丢。
                        // 只有普通笔记能作为"笔记来源";标注类条目的正文挂在
                        // annotation 上,不能冒充笔记。
                        const note = item.note;
                        if (note !== null) {
                          useWorkspaceUi.getState().addNoteSource({
                            id: note.id,
                            title: item.title.slice(0, 40) || "笔记",
                            documentId: note.documentId,
                          });
                        }
                        useWorkspaceUi
                          .getState()
                          .setDraft(
                            "请基于引用的这则笔记(AI 草稿/个人笔记,不是原文事实)梳理要点,并明确标出哪些结论需要回到原文核实:",
                          );
                        useWorkspaceUi.getState().setAgentOpen(true);
                      }}
                    >
                      <Sparkles className="w-3 h-3 text-purple-500" aria-hidden />
                      <span>问 AI</span>
                    </button>
                    ) : null}
                  </span>
                </div>
              );
            })}
          </div>
        </div>

        {!narrow && !indexCollapsed ? (
          <div
            className="notes-index-resizer"
            role="separator"
            aria-orientation="vertical"
            aria-label="调整笔记列表宽度"
            onPointerDown={onIndexResizeStart}
            onPointerMove={onIndexResizeMove}
            onPointerUp={onIndexResizeEnd}
            onPointerCancel={onIndexResizeEnd}
          />
        ) : null}
        <div className={`notes-workspace${narrow && !detailVisible ? " hidden-in-narrow" : ""}`}>
          {selected === null ? (
            <div className="notes-workspace-empty">
              <NotebookPen className="w-7 h-7" aria-hidden />
              <p>选择左侧一条笔记开始阅读与编辑</p>
              <p className="notes-hint">这里是笔记空间:引用出处、Markdown、手绘、表格与截图。</p>
            </div>
          ) : (
            <>
              <header className="notes-workspace-head">
                {narrow ? (
                  <button
                    type="button"
                    className="notes-back"
                    aria-label="返回列表"
                    onClick={() => setMobileDetail(false)}
                  >
                    <ArrowLeft className="w-3.5 h-3.5" aria-hidden />‹ 列表
                  </button>
                ) : null}
                <span className={`notes-kind-chip kind-${selected.kind}`}>
                  {NOTE_KIND_LABELS[selected.kind]}
                </span>
                {selected.location !== null && selected.locator !== null ? (
                  <button
                    type="button"
                    className="notes-loc"
                    title="回跳到原文位置"
                    onClick={() => jumpToLocator(documentId, selected.locator)}
                  >
                    <MapPin className="w-3 h-3" aria-hidden />
                    {selected.location}
                  </button>
                ) : null}
                {selected.note !== null ? (
                  <span className={`notes-status kind-${selected.note.status}`}>
                    {NOTE_STATUS_LABELS[selected.note.status]}
                  </span>
                ) : null}
                {editing && saveLabel !== "" ? (
                  <span className={`notes-save-state ${saveState}`} role="status">
                    {saveState === "saving" ? (
                      <span className="notes-saving-dot" aria-hidden />
                    ) : null}
                    {saveLabel}
                  </span>
                ) : null}
                <span className="notes-workspace-actions">
                  {editing ? (
                    <button
                      type="button"
                      className="btn small"
                      aria-label="完成编辑"
                      onClick={finishEditing}
                    >
                      <Check className="w-3 h-3" aria-hidden />
                      完成
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="btn small"
                      aria-label="编辑笔记"
                      onClick={() => openEditor(selected)}
                    >
                      <Pencil className="w-3 h-3" aria-hidden />
                      编辑
                    </button>
                  )}
                  <button
                    type="button"
                    className="btn small danger-ghost"
                    aria-label="删除笔记"
                    disabled={busy}
                    onClick={() => setConfirmDelete(true)}
                  >
                    <Trash2 className="w-3 h-3" aria-hidden />
                    删除
                  </button>
                </span>
              </header>

              {confirmDelete ? (
                <div className="notes-confirm" role="alertdialog" aria-label="确认删除">
                  <span>确定删除这条{NOTE_KIND_LABELS[selected.kind]}吗?删除后不可恢复。</span>
                  <button
                    type="button"
                    className="btn small danger"
                    disabled={busy}
                    onClick={() => void deleteSelected()}
                  >
                    删除
                  </button>
                  <button
                    type="button"
                    className="btn small ghost"
                    onClick={() => setConfirmDelete(false)}
                  >
                    取消
                  </button>
                </div>
              ) : null}

              {detailError !== null ? <ErrorAlert error={detailError} /> : null}

              {selected.quote !== null && selected.locator !== null ? (
                <div className="notes-quote-block">
                  <span className="notes-quote-label">原文引用 · 点击回跳</span>
                  <button
                    type="button"
                    className="notes-quote"
                    title="点击回跳原文"
                    onClick={() => jumpToLocator(documentId, selected.locator)}
                  >
                    {selected.quote}
                  </button>
                </div>
              ) : null}

              {editing ? (
                <div className="notes-editor-space">
                  {selected.annotation !== null ? (
                    <p className="notes-hint" role="note">
                      编辑的是这条标注的备注;高亮/下划线样式、涂鸦笔迹与文本框内容都不会被改写。
                    </p>
                  ) : null}
                  <NoteEditor
                    key={selected.key}
                    value={draft}
                    onChange={handleDraftChange}
                    ariaLabel={
                      selected.annotation !== null ? "标注备注(Markdown)" : "笔记内容(Markdown)"
                    }
                    maxLength={
                      selected.annotation !== null ? 2000 : MAX_NOTE_BODY_CHARS
                    }
                    rows={12}
                  />
                  {selected.note !== null ? (
                    <div className="notes-fields">
                      <label className="notes-field">
                        <span>状态</span>
                        <select
                          aria-label="笔记状态"
                          value={editStatus}
                          onChange={(event) => changeStatus(event.target.value as NoteStatus)}
                        >
                          {(Object.keys(NOTE_STATUS_LABELS) as NoteStatus[]).map((status) => (
                            <option key={status} value={status}>
                              {NOTE_STATUS_LABELS[status]}
                            </option>
                          ))}
                        </select>
                      </label>
                      <div className="notes-tag-editor" aria-label="标签">
                        {editTags.map((tag) => (
                          <span key={tag} className="notes-tag editable">
                            {tag}
                            <button
                              type="button"
                              aria-label={`删除标签 ${tag}`}
                              onClick={() => applyTags(removeTag(editTags, tag))}
                            >
                              ×
                            </button>
                          </span>
                        ))}
                        <input
                          type="text"
                          value={tagInput}
                          placeholder="添加标签,回车确认"
                          aria-label="添加标签"
                          onChange={(event) => setTagInput(event.target.value)}
                          onKeyDown={(event) => {
                            if (event.key === "Enter" && !event.nativeEvent.isComposing) {
                              event.preventDefault();
                              submitTag();
                            }
                          }}
                        />
                      </div>
                    </div>
                  ) : null}
                  {tagError !== null ? (
                    <p role="alert" className="notes-error">
                      {tagError}
                    </p>
                  ) : null}
                  {saveError !== null ? (
                    saveState === "conflict" ? (
                      <div className="notes-conflict" role="alert">
                        <ErrorAlert error={saveError} />
                        <div className="notes-conflict-actions" role="group" aria-label="笔记冲突处理">
                          <button
                            type="button"
                            className="btn small"
                            disabled={conflictBusy}
                            onClick={() => void keepMine()}
                          >
                            保留我的版本
                          </button>
                          <button
                            type="button"
                            className="btn small ghost"
                            disabled={conflictBusy}
                            onClick={() => void saveMineAsCopy()}
                          >
                            另存为副本
                          </button>
                          <button
                            type="button"
                            className="btn small ghost"
                            disabled={conflictBusy}
                            onClick={() => void discardMine()}
                          >
                            放弃我的修改
                          </button>
                        </div>
                      </div>
                    ) : (
                      <ErrorAlert error={saveError} onRetry={() => scheduleSave()} />
                    )
                  ) : null}
                </div>
              ) : (
                <div className="notes-view note-md">
                  {selected.tags.length > 0 ? (
                    <div className="notes-tag-view">
                      {selected.tags.map((tag) => (
                        <span key={tag} className="notes-tag">
                          {tag}
                        </span>
                      ))}
                    </div>
                  ) : null}
                  {itemBody(selected).trim() !== "" ? (
                    <Markdown>{itemBody(selected)}</Markdown>
                  ) : (
                    <p className="notes-hint">还没有内容,点「编辑」开始写笔记。</p>
                  )}
                </div>
              )}

              <footer className="notes-view-footer">
                最后更新 {formatTime(selected.updatedAt)}
                {selected.annotation !== null ? " · 来自标注" : ""}
              </footer>
            </>
          )}
        </div>
      </div>
      ) : view === "bookmarks" ? (
        <BookmarksPanel
          documentId={documentId}
          items={bookmarks.data ?? []}
          loading={bookmarks.isLoading}
          error={bookmarks.isError ? toAppError(bookmarks.error) : null}
          positionLocator={position.data?.locator ?? null}
          showToast={showToast}
          onChanged={() => {
            void queryClient.invalidateQueries({ queryKey: qk.documentBookmarks(documentId) });
          }}
          onCreateNote={(bookmark) => void createNoteFromBookmark(bookmark)}
        />
      ) : view === "graph" ? (
        <KnowledgeGraph
          documentId={documentId}
          client={client}
          items={items}
          bookmarks={bookmarks.data ?? []}
          conversationId={useQaStore.getState().currentConversationId}
          onToast={showToast}
        />
      ) : (
        <ClarificationsPanel
          documentId={documentId}
          items={clarifications.data?.items ?? []}
          loading={clarifications.isLoading}
          error={clarifications.isError ? toAppError(clarifications.error) : null}
          draft={clarifyDraft}
          onDraftChange={setClarifyDraft}
          source={clarifySource}
          onClearSource={() => setClarifySource(null)}
          onAdd={addClarification}
          onResolve={resolveClarification}
          onDefer={(item) => setClarificationStatus(item, "deferred")}
          onReopen={(item) => setClarificationStatus(item, "pending")}
          onRemove={removeClarification}
          onAskAi={askAiAboutClarification}
          onOpenNote={openNoteById}
          onRemark={setClarificationRemark}
        />
      )}

      {toast !== null ? (
        <div
          className={`notes-toast ${toast.kind}`}
          role={toast.kind === "error" ? "alert" : "status"}
        >
          {toast.message}
        </div>
      ) : null}
    </section>
  );
}
