// 任务中心:文档 → 会话 → 任务 三栏主从。
//
// 约定:
// - 任务数据只来自真实 TaskView;会话归属复用 TaskView.conversationId /
//   conversationTitle,不按任务逐条查询;文档标题来自 document_list_recent;
//   选中文档后一次 conversation_list 补齐该文档的真实会话(含暂无任务的会话);
// - 「未关联对话」是伪会话:该文档下没有会话归属的任务都进入这里;
// - 批量选择/全选/批量删除只作用于当前会话任务列表;
// - 进度只用真实数据(状态/阶段/时间戳),不造假百分比;
// - 取消走真实任务取消令牌(ai_cancel / agent_cancel / generation_cancel);
// - 失败原因来自 TaskView.error(稳定错误码 + 可见文案);
// - 「查看结果」按 resultRef 跳转消息或产物;不可跳转时明确说明。
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronRight, ExternalLink, ListTodo, RotateCcw, Trash2, X } from "lucide-react";
import type { TaskView } from "../../shared/generated/TaskView";
import type { DocumentSummary } from "../../shared/generated/DocumentSummary";
import type { ConversationView } from "../../shared/generated/ConversationView";
import type { TaskRetryResult } from "../../shared/generated/TaskRetryResult";
import { useServices } from "../../app/services";
import { describeAppError } from "../../app/api/errors";
import { ApiError } from "../../app/api/client";
import { phaseLabel, qaStatusText } from "../qa/presentation";
import { useQaStore } from "../qa/qaStore";
import { useWorkspaceUi } from "./uiStore";

export const TASK_TYPE_LABELS: Record<string, string> = {
  ask_question: "问答",
  generate_artifact: "生成资料",
  agent_run: "研究",
  parse_document: "解析 / OCR",
  build_embedding: "建立索引",
};

const UNLINKED_CONVERSATION_KEY = "__unlinked__";
const UNLINKED_DOCUMENT_KEY = "__no_document__";

/** 已用时间:运行中按当前时间,终态按 updatedAt - createdAt。 */
export function elapsedLabel(task: TaskView, now: number): string {
  const running = task.status === "running" || task.status === "queued";
  const end = running ? now : task.updatedAt;
  const ms = Math.max(0, end - task.createdAt);
  if (ms < 1000) return "<1 秒";
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return `${minutes} 分 ${rest} 秒`;
}

/** 失败原因:优先展示可见文案,并保留错误码供诊断。 */
export function taskFailureText(task: TaskView): string | null {
  if (task.status !== "failed" && task.status !== "interrupted") return null;
  const code = task.error?.code;
  if (code === undefined) return "任务失败(未提供原因)";
  const presented = describeAppError(task.error!);
  return `${presented.title}(${code})`;
}

/** 可取消:只有运行中/排队中。 */
export function canCancelTask(task: TaskView): boolean {
  return task.status === "running" || task.status === "queued";
}

/** 可重试:失败/取消/中断,且是问答或研究(生成类走产物中心重新生成)。 */
export function canRetryTask(task: TaskView): boolean {
  const retryable = task.status === "failed" || task.status === "canceled" || task.status === "interrupted";
  return retryable && (task.taskType === "ask_question" || task.taskType === "agent_run");
}

type ConversationGroup = {
  key: string;
  conversationId: string | null;
  title: string;
  tasks: TaskView[];
  /** 该会话最近一条任务的更新时间;无任务为 0。 */
  latestTaskAt: number;
  /** 真实会话的最近更新时间(用于空会话排序)。 */
  updatedAt: number;
};

type DocumentGroup = {
  key: string;
  documentId: string | null;
  title: string;
  conversations: ConversationGroup[];
  latest: number;
};

function groupTasksByDocument(
  items: TaskView[],
  documentTitles: Map<string, string>,
): DocumentGroup[] {
  const byDocument = new Map<string, DocumentGroup>();
  for (const task of items) {
    const documentKey = task.documentId ?? UNLINKED_DOCUMENT_KEY;
    let documentGroup = byDocument.get(documentKey);
    if (documentGroup === undefined) {
      documentGroup = {
        key: documentKey,
        documentId: task.documentId ?? null,
        title:
          task.documentId === undefined || task.documentId === null
            ? "未关联文档"
            : (documentTitles.get(task.documentId) ?? `文档 ${task.documentId.slice(0, 8)}`),
        conversations: [],
        latest: 0,
      };
      byDocument.set(documentKey, documentGroup);
    }
    const conversationKey = task.conversationId ?? UNLINKED_CONVERSATION_KEY;
    let conversation = documentGroup.conversations.find((entry) => entry.key === conversationKey);
    if (conversation === undefined) {
      conversation = {
        key: conversationKey,
        conversationId: task.conversationId ?? null,
        title: task.conversationTitle?.trim() || "未关联对话",
        tasks: [],
        latestTaskAt: 0,
        updatedAt: 0,
      };
      documentGroup.conversations.push(conversation);
    }
    conversation.tasks.push(task);
    conversation.latestTaskAt = Math.max(conversation.latestTaskAt, task.updatedAt);
    documentGroup.latest = Math.max(documentGroup.latest, task.updatedAt);
  }
  for (const documentGroup of byDocument.values()) {
    documentGroup.conversations.sort(compareConversations);
  }
  return [...byDocument.values()].sort((a, b) => b.latest - a.latest);
}

function compareConversations(a: ConversationGroup, b: ConversationGroup): number {
  if (a.tasks.length > 0 && b.tasks.length === 0) return -1;
  if (b.tasks.length > 0 && a.tasks.length === 0) return 1;
  if (a.latestTaskAt !== b.latestTaskAt) return b.latestTaskAt - a.latestTaskAt;
  if (a.updatedAt !== b.updatedAt) return b.updatedAt - a.updatedAt;
  return a.title.localeCompare(b.title, "zh-CN");
}

/** 选中文档的会话列表:TaskView 归属优先,再用一次 conversation_list 补齐空会话。 */
function mergeConversations(
  documentGroup: DocumentGroup | null,
  conversations: ConversationView[] | undefined,
): ConversationGroup[] {
  if (documentGroup === null) return [];
  const merged = new Map<string, ConversationGroup>();
  for (const conversation of documentGroup.conversations) {
    merged.set(conversation.key, {
      ...conversation,
      tasks: [...conversation.tasks],
    });
  }
  for (const conversation of conversations ?? []) {
    const existing = merged.get(conversation.id);
    if (existing !== undefined) {
      existing.updatedAt = Math.max(existing.updatedAt, conversation.updatedAt);
      if (existing.title === "未关联对话" && conversation.title.trim().length > 0) {
        existing.title = conversation.title;
      }
      continue;
    }
    merged.set(conversation.id, {
      key: conversation.id,
      conversationId: conversation.id,
      title: conversation.title.trim() || "未命名会话",
      tasks: [],
      latestTaskAt: 0,
      updatedAt: conversation.updatedAt,
    });
  }
  if (!merged.has(UNLINKED_CONVERSATION_KEY)) {
    merged.set(UNLINKED_CONVERSATION_KEY, {
      key: UNLINKED_CONVERSATION_KEY,
      conversationId: null,
      title: "未关联对话",
      tasks: [],
      latestTaskAt: 0,
      updatedAt: 0,
    });
  }
  return [...merged.values()].sort(compareConversations);
}

function defaultConversation(groups: ConversationGroup[]): ConversationGroup | null {
  return groups.find((group) => group.tasks.length > 0) ?? groups[0] ?? null;
}

export function TaskCenterPanel({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}): ReactNode {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const [bulkConfirm, setBulkConfirm] = useState(false);
  const [bulkDeleting, setBulkDeleting] = useState(false);
  const [selectedDocumentKey, setSelectedDocumentKey] = useState<string | null>(null);
  const [selectedConversationKey, setSelectedConversationKey] = useState<string | null>(null);
  // 运行中的已用时间需要每秒刷新;终态不需要。
  const [now, setNow] = useState(() => Date.now());
  const selectAllRef = useRef<HTMLInputElement | null>(null);
  const wasOpenRef = useRef(false);

  const documents = useQuery({
    queryKey: ["documents", "task-center"],
    queryFn: async () => {
      const page = await client.call<{ items: DocumentSummary[] }>("document_list_recent", {
        limit: 100,
      });
      return page.items;
    },
    enabled: open,
    staleTime: 60_000,
  });

  const tasks = useQuery({
    // 与顶栏角标(limit 30)分开缓存:首屏必须是完整的 50 条,
    // 否则文档标题回退占位符、较早任务短暂不可达。
    queryKey: ["tasks", "center", 50],
    queryFn: () =>
      client.call<{ items: TaskView[] }>("task_list", { activeOnly: false, limit: 50 }),
    // 面板关闭时不必轮询;打开且无活跃任务时降频,只在有活跃任务时保持 3s。
    refetchInterval: (query) => {
      if (!open) return false;
      const running = (query.state.data?.items ?? []).some((task) => canCancelTask(task));
      return running ? 3000 : 15_000;
    },
    enabled: open,
  });

  const items = tasks.data?.items ?? [];
  const hasRunning = items.some((task) => canCancelTask(task));

  const documentTitles = useMemo(() => {
    const map = new Map<string, string>();
    for (const document of documents.data ?? []) {
      map.set(document.id, document.displayName || document.id.slice(0, 8));
    }
    return map;
  }, [documents.data]);

  const documentGroups = useMemo(
    () => groupTasksByDocument(items, documentTitles),
    [items, documentTitles],
  );

  // 每次打开都回到默认:最近有任务的文档及其最近会话。
  useEffect(() => {
    if (open && !wasOpenRef.current) {
      setSelectedDocumentKey(null);
      setSelectedConversationKey(null);
      setSelectedIds(new Set());
      setBulkConfirm(false);
    }
    wasOpenRef.current = open;
  }, [open]);

  // 文档选择:null = 跟随默认(最近有任务的文档);选中项消失时回退默认。
  const activeDocument =
    documentGroups.find((group) => group.key === selectedDocumentKey) ??
    documentGroups[0] ??
    null;
  useEffect(() => {
    if (!open || selectedDocumentKey === null) return;
    if (!documentGroups.some((group) => group.key === selectedDocumentKey)) {
      setSelectedDocumentKey(null);
      setSelectedConversationKey(null);
    }
  }, [open, documentGroups, selectedDocumentKey]);

  const conversations = useQuery({
    queryKey: ["conversation", "list", "task-center", activeDocument?.documentId ?? "none"],
    queryFn: () =>
      client.call<{ items: ConversationView[] }>("conversation_list", {
        documentId: activeDocument!.documentId!,
        limit: 50,
      }),
    enabled: open && activeDocument?.documentId != null,
    refetchInterval: open ? (hasRunning ? 3000 : 15_000) : false,
    staleTime: 10_000,
  });

  const conversationGroups = useMemo(
    () => mergeConversations(activeDocument, conversations.data?.items),
    [activeDocument, conversations.data],
  );

  // 会话选择:null = 跟随默认(该文档最近有任务的会话);消失时回退默认。
  const activeConversation =
    conversationGroups.find((group) => group.key === selectedConversationKey) ??
    defaultConversation(conversationGroups);
  useEffect(() => {
    if (!open || selectedConversationKey === null) return;
    if (!conversationGroups.some((group) => group.key === selectedConversationKey)) {
      setSelectedConversationKey(null);
    }
  }, [open, conversationGroups, selectedConversationKey]);

  const currentTasks = activeConversation?.tasks ?? [];
  const currentTaskIds = useMemo(
    () => currentTasks.map((task) => task.id),
    [currentTasks],
  );
  const currentTaskIdKey = currentTaskIds.join("|");
  const deletableTasks = currentTasks.filter((task) => !canCancelTask(task));
  const selectedInView = useMemo(
    () => currentTasks.filter((task) => selectedIds.has(task.id)),
    [currentTasks, selectedIds],
  );

  // 切换文档/会话后清空选择,批量操作不跨列表。
  useEffect(() => {
    setSelectedIds(new Set());
    setBulkConfirm(false);
  }, [activeDocument?.key, activeConversation?.key]);

  // 刷新后任务被移除/变为运行时,剔除失效选择。
  useEffect(() => {
    const valid = new Set(currentTaskIds);
    setSelectedIds((previous) => {
      if (previous.size === 0) return previous;
      const next = new Set<string>();
      let changed = false;
      for (const id of previous) {
        const task = currentTasks.find((entry) => entry.id === id);
        if (task !== undefined && valid.has(id) && !canCancelTask(task)) next.add(id);
        else changed = true;
      }
      return changed ? next : previous;
    });
    // currentTaskIdKey 覆盖任务集合变化;currentTasks 随查询刷新而变化。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentTaskIdKey]);

  useEffect(() => {
    if (!open || !hasRunning) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [open, hasRunning]);

  const allDeletableSelected =
    deletableTasks.length > 0 && deletableTasks.every((task) => selectedIds.has(task.id));
  const someDeletableSelected =
    !allDeletableSelected && deletableTasks.some((task) => selectedIds.has(task.id));
  useEffect(() => {
    if (selectAllRef.current !== null) selectAllRef.current.indeterminate = someDeletableSelected;
  }, [someDeletableSelected, currentTaskIdKey]);

  function selectDocument(group: DocumentGroup): void {
    setSelectedDocumentKey(group.key);
    setSelectedConversationKey(null);
    setBulkConfirm(false);
  }

  function selectConversation(group: ConversationGroup): void {
    // 选择会话即锁定当前文档,避免轮询排序把文档从脚下换掉。
    if (activeDocument !== null) setSelectedDocumentKey(activeDocument.key);
    setSelectedConversationKey(group.key);
    setBulkConfirm(false);
  }

  async function cancelTask(task: TaskView): Promise<void> {
    setBusyId(task.id);
    setError(null);
    setNotice(null);
    try {
      // 按任务类型走对应取消入口(都落到同一取消令牌)。
      if (task.taskType === "agent_run") {
        await client.call("agent_cancel", {
          taskId: task.id,
          expectedRevision: task.stateVersion,
        });
      } else if (task.taskType === "generate_artifact") {
        await client.call("generation_cancel", { taskId: task.id });
      } else if (task.taskType === "parse_document" || task.taskType === "build_embedding") {
        await client.call("parse_cancel", {
          taskId: task.id,
          expectedRevision: task.stateVersion,
        });
      } else {
        await client.call("ai_cancel", {
          taskId: task.id,
          expectedRevision: task.stateVersion,
        });
      }
      setNotice("已请求取消;已完成的内容会保留。");
      await queryClient.invalidateQueries({ queryKey: ["tasks"] });
    } catch (caught) {
      if (caught instanceof ApiError) {
        setError(describeAppError(caught.appError).title);
      } else {
        setError("取消失败,请重试");
      }
    } finally {
      setBusyId(null);
    }
  }

  async function retryTask(task: TaskView): Promise<void> {
    setBusyId(task.id);
    setError(null);
    setNotice("正在用原问题重新提交…");
    try {
      // 真正重发:后端用原问题与原快照新建一轮消息,不覆盖旧结果。
      const result = await client.call<TaskRetryResult>("task_retry", {
        taskId: task.id,
        idempotencyKey: client.newIdempotencyKey(),
      });
      // 新任务创建后立即反馈并关闭;消息/会话缓存刷新放到后台,
      // 否则 await 这些查询会把面板和“正在重新提交”提示挂住十几秒。
      setNotice("已重新提交;新回答会出现在对话中。");
      onClose();
      window.dispatchEvent(
        new CustomEvent("knowwise-reveal-message", {
          detail: {
            messageId: result.assistantMessageId,
            conversationId: result.conversationId,
            documentId: result.documentId,
          },
        }),
      );
      void queryClient.invalidateQueries({ queryKey: ["tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["messages"] });
      void queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
    } catch (caught) {
      if (caught instanceof ApiError) {
        setError(describeAppError(caught.appError).title);
      } else {
        setError("重试未发起,请检查原问题与文档是否仍可用");
      }
    } finally {
      setBusyId(null);
    }
  }

  function toggleSelected(task: TaskView): void {
    if (canCancelTask(task)) return;
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(task.id)) next.delete(task.id);
      else next.add(task.id);
      return next;
    });
    setBulkConfirm(false);
  }

  function toggleSelectAll(): void {
    const visible = new Set(deletableTasks.map((task) => task.id));
    setSelectedIds((prev) => {
      const next = new Set(prev);
      // 只改当前会话列表内的选择,不影响其他列表(理论上已清空)。
      for (const id of visible) {
        if (allDeletableSelected) next.delete(id);
        else next.add(id);
      }
      return next;
    });
    setBulkConfirm(false);
  }

  async function deleteSelectedTasks(): Promise<void> {
    const targets = selectedInView.filter((task) => !canCancelTask(task));
    if (targets.length === 0) return;
    setBulkDeleting(true);
    setError(null);
    const failures: string[] = [];
    for (const task of targets) {
      try {
        await client.call("task_delete", { taskId: task.id });
      } catch {
        failures.push(task.id);
      }
    }
    setSelectedIds(new Set(failures));
    setBulkConfirm(false);
    setBulkDeleting(false);
    await queryClient.invalidateQueries({ queryKey: ["tasks"] });
    if (failures.length > 0) {
      setError(`${failures.length} 个任务删除失败,已保留选择可重试`);
    } else {
      setNotice("已删除所选任务。");
    }
  }

  async function deleteTask(taskId: string): Promise<void> {
    setBusyId(taskId);
    setError(null);
    try {
      await client.call("task_delete", { taskId });
      await queryClient.invalidateQueries({ queryKey: ["tasks"] });
    } catch {
      setError("删除任务失败");
    } finally {
      setBusyId(null);
    }
  }

  function openResult(task: TaskView): void {
    const ref = task.resultRef;
    if (ref === undefined || ref === null) {
      setError("该任务没有可打开的结果");
      return;
    }
    if (ref.kind === "artifact_version") {
      window.dispatchEvent(
        new CustomEvent("knowwise-open-artifacts", { detail: { artifactVersionId: ref.id } }),
      );
      onClose();
      return;
    }
    window.dispatchEvent(
      new CustomEvent("knowwise-open-message", { detail: { messageId: ref.id } }),
    );
    onClose();
  }

  /** 空态动作:回到文档阅读区并打开对应会话(AI 助手)。 */
  function goToConversation(): void {
    const ui = useWorkspaceUi.getState();
    const documentId = activeDocument?.documentId ?? ui.currentDocumentId;
    if (documentId === null) {
      ui.setSidebarOpen(true);
      onClose();
      return;
    }
    const tab = ui.tabs.find((entry) => entry.documentId === documentId);
    ui.openDocument(documentId, tab?.versionId ?? null);
    if (activeConversation?.conversationId !== null && activeConversation !== null) {
      useQaStore.getState().setConversation(activeConversation.conversationId, documentId);
    } else if (documentId !== null) {
      useQaStore.getState().setConversation(null, documentId);
    }
    ui.setDockTab("agent");
    onClose();
  }

  function renderTaskRow(task: TaskView): ReactNode {
    const failure = taskFailureText(task);
    const running = canCancelTask(task);
    return (
      <li key={task.id} className={`task-row status-${task.status}`} data-task-id={task.id}>
        <label className="task-select" title={running ? "运行中任务不可删除" : "选择任务"}>
          <input
            type="checkbox"
            checked={selectedIds.has(task.id)}
            disabled={running}
            onChange={() => toggleSelected(task)}
            aria-label={`选择任务:${TASK_TYPE_LABELS[task.taskType] ?? task.taskType}`}
          />
        </label>
        <div className="task-row-main">
          <span className="task-row-type">
            {TASK_TYPE_LABELS[task.taskType] ?? task.taskType}
          </span>
          <span className="task-row-status">{running ? phaseLabel(task) : qaStatusText(task)}</span>
          <span className="task-row-elapsed">{elapsedLabel(task, now)}</span>
        </div>
        {failure ? (
          <p className="task-row-error" role="status">
            {failure}
          </p>
        ) : null}
        <div className="task-row-actions">
          {running ? (
            <button
              type="button"
              className="btn tiny"
              aria-label="取消任务"
              disabled={busyId === task.id}
              onClick={() => void cancelTask(task)}
            >
              取消
            </button>
          ) : null}
          {task.resultRef !== undefined && task.resultRef !== null ? (
            <button type="button" className="btn tiny ghost" onClick={() => openResult(task)}>
              <ExternalLink className="w-3 h-3" aria-hidden="true" />
              <span>查看结果</span>
            </button>
          ) : null}
          {canRetryTask(task) ? (
            <button
              type="button"
              className="btn tiny ghost"
              aria-label="重试任务"
              disabled={busyId === task.id}
              onClick={() => void retryTask(task)}
            >
              <RotateCcw className="w-3 h-3" aria-hidden="true" />
              <span>重试</span>
            </button>
          ) : null}
          {!running ? (
            <button
              type="button"
              className="btn tiny danger-ghost"
              aria-label="删除任务"
              disabled={busyId === task.id}
              onClick={() => void deleteTask(task.id)}
            >
              <Trash2 className="w-3 h-3" aria-hidden="true" />
            </button>
          ) : null}
        </div>
      </li>
    );
  }

  if (!open) return null;

  const isEmpty = items.length === 0 && !tasks.isLoading;
  const hasConversation = activeConversation !== null;

  return (
    <div className="task-center" role="dialog" aria-label="任务中心" aria-modal="false">
      <header className="task-center-head">
        <ListTodo className="w-4 h-4" aria-hidden="true" />
        <strong>任务中心</strong>
        <span className="task-center-count">
          {tasks.isLoading ? "加载中…" : `${items.length} 个任务${hasRunning ? " · 运行中" : ""}`}
        </span>
        <button type="button" className="icon-btn" onClick={onClose} aria-label="关闭任务中心">
          <X className="w-4 h-4" aria-hidden="true" />
        </button>
      </header>

      {items.length > 0 ? (
        <div className="task-current-bar" aria-live="polite">
          <span className="task-current">
            <span className="task-current-label">当前文档</span>
            <strong title={activeDocument?.title ?? ""}>
              {activeDocument?.title ?? "无"}
            </strong>
          </span>
          <ChevronRight className="task-current-arrow w-3.5 h-3.5" aria-hidden="true" />
          <span className="task-current">
            <span className="task-current-label">当前会话</span>
            <strong title={activeConversation?.title ?? ""}>
              {activeConversation?.title ?? "无"}
            </strong>
          </span>
        </div>
      ) : null}

      {error ? (
        <p className="task-center-error" role="alert">
          {error}
        </p>
      ) : null}
      {notice ? (
        <p className="task-center-notice" role="status">
          {notice}
        </p>
      ) : null}

      {tasks.isLoading ? <p role="status">加载任务…</p> : null}
      {isEmpty ? (
        <div className="task-empty-state" role="status">
          <p className="task-empty-title">暂无任务</p>
          <p className="task-empty-hint">
            解析、问答、研究、生成与导出都会出现在这里。打开文档提问后即可在此按文档与会话查看。
          </p>
          <button type="button" className="btn" onClick={goToConversation}>
            去阅读区提问
          </button>
        </div>
      ) : null}

      {items.length > 0 ? (
        <div className="task-center-body">
          <section className="task-pane task-doc-pane" aria-label="文档列表">
            <header className="task-pane-head">
              <span className="task-pane-title">文档</span>
              <span className="task-pane-count">{documentGroups.length}</span>
            </header>
            <ul className="task-doc-list">
              {documentGroups.map((group) => {
                const active = group.key === activeDocument?.key;
                const count = group.conversations.reduce(
                  (total, conversation) => total + conversation.tasks.length,
                  0,
                );
                return (
                  <li key={group.key}>
                    <button
                      type="button"
                      className={`task-doc-item${active ? " active" : ""}`}
                      aria-pressed={active}
                      aria-label={`文档:${group.title}`}
                      title={group.title}
                      onClick={() => selectDocument(group)}
                    >
                      <span className="task-doc-title">{group.title}</span>
                      <span className="task-item-count">{count}</span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>

          <section className="task-pane task-conv-pane" aria-label="会话列表">
            <header className="task-pane-head">
              <span className="task-pane-title">会话</span>
              <span className="task-pane-count">{conversationGroups.length}</span>
            </header>
            <ul className="task-conv-list">
              {conversationGroups.map((group) => {
                const active = group.key === activeConversation?.key;
                return (
                  <li key={group.key}>
                    <button
                      type="button"
                      className={`task-conv-item${active ? " active" : ""}${
                        group.conversationId === null ? " unlinked" : ""
                      }`}
                      aria-pressed={active}
                      aria-label={`会话:${group.title}`}
                      title={group.title}
                      onClick={() => selectConversation(group)}
                    >
                      <span className="task-conv-title">{group.title}</span>
                      <span className="task-item-count">{group.tasks.length}</span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>

          <section
            className="task-pane task-task-pane"
            aria-label={`当前会话任务:${activeConversation?.title ?? "无"}`}
          >
            <header className="task-pane-head">
              <span className="task-pane-title">任务</span>
              <span className="task-pane-count">{currentTasks.length}</span>
              <span className="task-pane-subtitle" title={activeConversation?.title ?? ""}>
                {activeConversation?.title ?? "无会话"}
              </span>
            </header>
            {currentTasks.length > 0 ? (
              <div className="task-bulk-bar" role="group" aria-label="批量管理当前会话任务">
                <label className="task-bulk-select-all">
                  <input
                    ref={selectAllRef}
                    type="checkbox"
                    aria-label="全选可删除任务"
                    checked={allDeletableSelected}
                    onChange={toggleSelectAll}
                    disabled={deletableTasks.length === 0}
                  />
                  全选可删除任务
                </label>
                <span className="task-bulk-count">已选 {selectedInView.length} 项</span>
                {selectedInView.length > 0 ? (
                  bulkConfirm ? (
                    <span
                      className="task-confirm-inline"
                      role="alertdialog"
                      aria-label="确认批量删除任务"
                    >
                      <span>删除所选 {selectedInView.length} 个任务记录?</span>
                      <button
                        type="button"
                        className="btn tiny danger"
                        disabled={bulkDeleting}
                        onClick={() => void deleteSelectedTasks()}
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
                      aria-label="批量删除任务"
                      onClick={() => setBulkConfirm(true)}
                    >
                      <Trash2 className="w-3 h-3" aria-hidden="true" />
                      批量删除
                    </button>
                  )
                ) : null}
              </div>
            ) : null}
            {currentTasks.length > 0 ? (
              <ul className="task-center-list">{currentTasks.map(renderTaskRow)}</ul>
            ) : (
              <div className="task-empty-state compact" role="status">
                <p className="task-empty-title">该会话暂无任务</p>
                <p className="task-empty-hint">
                  {activeConversation?.conversationId === null
                    ? "该文档下没有未关联对话的任务。"
                    : "可以回到该会话继续提问,新任务会出现在这里。"}
                </p>
                {hasConversation ? (
                  <button type="button" className="btn tiny" onClick={goToConversation}>
                    {activeConversation?.conversationId === null
                      ? "去阅读区提问"
                      : "打开该会话"}
                  </button>
                ) : null}
              </div>
            )}
          </section>
        </div>
      ) : null}
    </div>
  );
}
