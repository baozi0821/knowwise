// 阅读区笔迹撤销/重做:按文档隔离的会话内栈。
// 记录以 anchorId 为身份(删除后可重建同一 anchor/color/noteText),
// 不把本地 revision 当作历史事实;应用时总是先取最新标注再写入。
import { useEffect, useRef, useState } from "react";
import type { AnnotationColor } from "../../shared/generated/AnnotationColor";
import type { AnnotationView } from "../../shared/generated/AnnotationView";
import type { CommandClient } from "../../app/api/client";
import { broadcastAnnotationChange } from "./annotationEvents";

export type InkSnapshot = {
  anchorId: string;
  color: AnnotationColor;
  noteText: string;
};

/** before/after 为 null 表示该标注不存在(删除)。 */
export type InkChange = {
  anchorId: string;
  before: InkSnapshot | null;
  after: InkSnapshot | null;
};

export type InkHistoryEntry = {
  id: number;
  label: string;
  changes: InkChange[];
};

type DocStack = { undo: InkHistoryEntry[]; redo: InkHistoryEntry[] };

const stacks = new Map<string, DocStack>();
const queues = new Map<string, Promise<unknown>>();
let nextEntryId = 1;
const MAX_ENTRIES = 200;

function stackOf(documentId: string): DocStack {
  let stack = stacks.get(documentId);
  if (stack === undefined) {
    stack = { undo: [], redo: [] };
    stacks.set(documentId, stack);
  }
  return stack;
}

const HISTORY_EVENT = "knowwise-ink-history-changed";

function notifyHistory(documentId: string): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent(HISTORY_EVENT, { detail: { documentId } }));
}

/** 历史栈版本:工具栏用它刷新撤销/重做按钮的可用状态。 */
export function useInkHistoryVersion(documentId: string | undefined): number {
  const [version, setVersion] = useState(0);
  useEffect(() => {
    if (documentId === undefined) return;
    const onChange = (event: Event) => {
      const detail = (event as CustomEvent<{ documentId?: string }>).detail;
      if (detail?.documentId === documentId) setVersion((v) => v + 1);
    };
    window.addEventListener(HISTORY_EVENT, onChange);
    return () => window.removeEventListener(HISTORY_EVENT, onChange);
  }, [documentId]);
  return version;
}

export function pushInkHistory(
  documentId: string,
  label: string,
  changes: InkChange[],
): void {
  const meaningful = changes.filter(
    (change) =>
      (change.before === null) !== (change.after === null) ||
      change.before?.noteText !== change.after?.noteText ||
      change.before?.color !== change.after?.color,
  );
  if (meaningful.length === 0) return;
  const stack = stackOf(documentId);
  stack.undo.push({ id: nextEntryId++, label, changes: meaningful });
  if (stack.undo.length > MAX_ENTRIES) stack.undo.shift();
  stack.redo = [];
  notifyHistory(documentId);
}

export function canUndoInk(documentId: string | undefined): boolean {
  return documentId !== undefined && (stacks.get(documentId)?.undo.length ?? 0) > 0;
}

export function canRedoInk(documentId: string | undefined): boolean {
  return documentId !== undefined && (stacks.get(documentId)?.redo.length ?? 0) > 0;
}

export function clearInkHistory(documentId: string | undefined): void {
  if (documentId !== undefined) {
    stacks.delete(documentId);
    notifyHistory(documentId);
  }
}

function enqueue<T>(documentId: string, task: () => Promise<T>): Promise<T> {
  const previous = queues.get(documentId) ?? Promise.resolve();
  const next = previous.catch(() => undefined).then(task);
  queues.set(documentId, next);
  return next;
}

async function applySnapshot(
  client: CommandClient,
  documentId: string,
  change: InkChange,
  desired: InkSnapshot | null,
  byAnchor: Map<string, AnnotationView>,
): Promise<void> {
  const current = byAnchor.get(change.anchorId);
  if (desired === null) {
    if (current === undefined) return;
    await client.call("annotation_delete", {
      annotationId: current.id,
      expectedRevision: current.revision,
    });
    byAnchor.delete(change.anchorId);
    broadcastAnnotationChange({ type: "deleted", annotationId: current.id });
    return;
  }
  if (current === undefined) {
    const created = await client.call<AnnotationView>("annotation_create", {
      documentId,
      anchorId: desired.anchorId,
      color: desired.color,
      noteText: desired.noteText,
      idempotencyKey: client.newIdempotencyKey(),
    });
    byAnchor.set(created.anchorId, created);
    broadcastAnnotationChange({ type: "created", annotation: created });
    return;
  }
  if (current.noteText === desired.noteText && current.color === desired.color) return;
  const updated = await client.call<AnnotationView>("annotation_update", {
    annotationId: current.id,
    color: desired.color,
    noteText: desired.noteText,
    expectedRevision: current.revision,
  });
  byAnchor.set(updated.anchorId, updated);
  broadcastAnnotationChange({ type: "updated", annotation: updated });
}

async function applyEntry(
  client: CommandClient,
  documentId: string,
  entry: InkHistoryEntry,
  direction: "undo" | "redo",
): Promise<void> {
  const list = await client.call<AnnotationView[]>("annotation_list", { documentId });
  const byAnchor = new Map(list.map((annotation) => [annotation.anchorId, annotation]));
  for (const change of entry.changes) {
    await applySnapshot(
      client,
      documentId,
      change,
      direction === "undo" ? change.before : change.after,
      byAnchor,
    );
  }
}

/** 撤销一步;返回被撤销的操作描述(无历史时为 null)。 */
export function undoInk(
  client: CommandClient,
  documentId: string,
): Promise<InkHistoryEntry | null> {
  return enqueue(documentId, async () => {
    const stack = stackOf(documentId);
    const entry = stack.undo.pop();
    if (entry === undefined) return null;
    try {
      await applyEntry(client, documentId, entry, "undo");
      stack.redo.push(entry);
      notifyHistory(documentId);
      return entry;
    } catch (error) {
      stack.undo.push(entry);
      throw error;
    }
  });
}

/** 重做一步;返回被重做的操作描述(无历史时为 null)。 */
export function redoInk(
  client: CommandClient,
  documentId: string,
): Promise<InkHistoryEntry | null> {
  return enqueue(documentId, async () => {
    const stack = stackOf(documentId);
    const entry = stack.redo.pop();
    if (entry === undefined) return null;
    try {
      await applyEntry(client, documentId, entry, "redo");
      stack.undo.push(entry);
      notifyHistory(documentId);
      return entry;
    } catch (error) {
      stack.redo.push(entry);
      throw error;
    }
  });
}

/**
 * 快捷键:Ctrl+Z 撤销,Ctrl+Shift+Z / Ctrl+Y 重做。
 * 只在阅读容器内生效(焦点不在输入框/可编辑区),不劫持笔记编辑器。
 */
export function useInkHistoryHotkeys({
  documentId,
  containerRef,
  client,
  onApplied,
  onError,
}: {
  documentId?: string;
  containerRef: React.RefObject<HTMLElement | null>;
  client?: CommandClient;
  onApplied?: (entry: InkHistoryEntry, direction: "undo" | "redo") => void;
  onError?: (message: string) => void;
}): void {
  const appliedRef = useRef(onApplied);
  appliedRef.current = onApplied;
  const errorRef = useRef(onError);
  errorRef.current = onError;
  const busyRef = useRef(false);

  useEffect(() => {
    if (!documentId || !client) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.isComposing || event.defaultPrevented) return;
      const mod = event.ctrlKey || event.metaKey;
      if (!mod || event.altKey) return;
      const key = event.key.toLowerCase();
      if (key !== "z" && key !== "y") return;
      const direction: "undo" | "redo" =
        key === "y" ? "redo" : event.shiftKey ? "redo" : "undo";
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.closest("input, textarea, select") !== null ||
          target.isContentEditable ||
          target.closest("[contenteditable='true']") !== null)
      ) {
        return;
      }
      const active = document.activeElement;
      const container = containerRef.current;
      const activeElement = active instanceof HTMLElement ? active : null;
      // 编辑器/弹窗内一律不接管(笔记输入、文本框、设置、确认框)。
      if (
        activeElement !== null &&
        activeElement.closest(
          "input, textarea, select, [contenteditable='true'], [role='dialog'], [role='alertdialog']",
        ) !== null
      ) {
        return;
      }
      // 按钮焦点(阅读工具栏/套索操作条)仍视为在阅读区:否则点完画笔、
      // 直接按 Ctrl+Z 会毫无反应。Agent/设置等按钮不在这些容器内。
      const toolbarFocus =
        activeElement !== null &&
        activeElement.closest(".reader-toolbar, .lasso-actions") !== null;
      // 阅读容器的焦点可能落在 main-region / reader-region 本身(点击空白、
      // 程序化切换后),这些仍属于阅读区;笔记与 Agent 面板不在此范围。
      const readerScope =
        activeElement !== null &&
        (activeElement.classList.contains("main-region") ||
          activeElement.closest(".reader-region, .reader-mount, .rich-text-view, .pdf-viewer") !==
            null);
      const activeInside =
        active === null ||
        active === document.body ||
        active === document.documentElement ||
        toolbarFocus ||
        readerScope ||
        (container !== null && active instanceof Node && container.contains(active));
      if (!activeInside) return;
      if (!canUndoInk(documentId) && !canRedoInk(documentId)) return;
      if (busyRef.current) {
        event.preventDefault();
        return;
      }
      event.preventDefault();
      busyRef.current = true;
      const task =
        direction === "undo" ? undoInk(client, documentId) : redoInk(client, documentId);
      void task
        .then((entry) => {
          if (entry !== null) appliedRef.current?.(entry, direction);
        })
        .catch(() => {
          errorRef.current?.(
            direction === "undo" ? "撤销未完成,请重试" : "重做未完成,请重试",
          );
        })
        .finally(() => {
          busyRef.current = false;
        });
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [client, containerRef, documentId]);
}

/** 一次擦除拖动内的变更合并为一条历史(按 anchor 合并 before/after)。 */
export class InkGestureRecorder {
  private readonly changes: InkChange[] = [];

  constructor(private readonly label: string) {}

  record(change: InkChange): void {
    const existing = this.changes.find((item) => item.anchorId === change.anchorId);
    if (existing === undefined) {
      this.changes.push({
        anchorId: change.anchorId,
        before: change.before,
        after: change.after,
      });
      return;
    }
    // 同一条标注在一次拖动里被多次擦除:保留最初的 before 与最新的 after。
    existing.after = change.after;
  }

  get size(): number {
    return this.changes.length;
  }

  /** 提交为一条历史;返回纳入的变更数(0 表示无历史)。 */
  commit(documentId: string): number {
    const count = this.changes.length;
    if (count > 0) pushInkHistory(documentId, this.label, this.changes);
    this.changes.length = 0;
    return count;
  }
}
