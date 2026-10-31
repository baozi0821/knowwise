// M13 拥有容器 / M09 拥有问答编排:Agent 面板 = 会话列表(可折叠)+
// 消息区(flex 滚动、接近底部时自动滚到底)+ 输入区(固定底部、自适应
// 高度、Enter 发送 / Shift+Enter 换行、截图/本地图片附件)+ 任务中心。
// 提问流程不变:suggest → freeze → ai_ask(范围默认取建议项,全文兜底需确认)。
//
// 图片多模态(M15):图片先经 canvas 压缩(长边 ≤1568/JPEG q80),再上传为
// attachmentId;消息正文只保存 `attachment:<id>` 轻量引用。模型输入通过
// attachmentIds → ProviderMessage.attachments 传字节,绝不把 base64 写进正文。
// 远程图片外发需要用户明确授权;未就绪时禁用含图片的发送并说明原因。
//
// 引用点击经 evidence_resolve_citation 解析定位后派发 knowwise-goto,由
// 阅读器(M03)滚动定位;失败如实呈现,不编造回答。
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  BookMarked,
  BookOpen,
  Camera,
  Check,
  Columns2,
  Download,
  FileText,
  FolderTree,
  History,
  ImagePlus,
  Languages,
  ListChecks,
  MessageSquare,
  PanelLeftClose,
  PanelLeftOpen,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  Send,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";
import type { AgentRunStepsView } from "../../shared/generated/AgentRunStepsView";
import type { AgentStepView } from "../../shared/generated/AgentStepView";
import type { AttachmentThumbnail } from "../../shared/generated/AttachmentThumbnail";
import type { ConversationView } from "../../shared/generated/ConversationView";
import type { ContextSnapshotView } from "../../shared/generated/ContextSnapshotView";
import type { ContextSuggestion } from "../../shared/generated/ContextSuggestion";
import type { ScopeDraft } from "../../shared/generated/ScopeDraft";
import type { MessageContent } from "../../shared/generated/MessageContent";
import type { TaskView } from "../../shared/generated/TaskView";
import type { ArtifactView } from "../../shared/generated/ArtifactView";
import type { ModelCapabilityView } from "../../shared/generated/ModelCapabilityView";
import type { AttachmentView } from "../../shared/generated/AttachmentView";
import type { VisionStatusView } from "../../shared/generated/VisionStatusView";
import { useServices } from "../../app/services";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import { qk } from "../../app/api/keys";
import { dialogPickImages, dialogSavePath } from "../../app/ipc/transport";
import { ConversationExportDialog } from "../context/ConversationExport";
import { AllConversationsDialog } from "./AllConversationsDialog";
import {
  EMPTY_PER_TYPE_COUNTS,
  QuizCountControls,
  isPerTypeExplicit,
  quizConfigIssue,
  quizTotal,
} from "./QuizCountControls";
import type { QuizPerTypeCounts } from "../../shared/generated/QuizPerTypeCounts";
import {
  CitationPicker,
  CitedArtifactChips,
  mergeCitedArtifacts,
  type CitedArtifact,
} from "./CitationPicker";
import { useScreenClip } from "../capture/screenClip";
import {
  MAX_ATTACHMENT_BYTES,
  MAX_ATTACHMENTS,
  MAX_TOTAL_ATTACHMENT_BYTES,
  type ComposerAttachment,
  compressImageDataUrl,
  readLocalImage,
  totalAttachmentBytes,
  uploadImageAttachment,
} from "./imageAttachment";
import {
  CopyAnswerButton,
  MessageView,
  agentAnswerPlainText,
  answerPlainText,
  resolveCitationAndGoto,
} from "../qa/AnswerView";
import { useQaStore } from "../qa/qaStore";
import { EvidenceLensPanel } from "../evidence/EvidenceLensPanel";
import {
  EMPTY_STREAM,
  reduceStream,
  type StreamEventV1,
  type StreamState,
} from "../qa/stream";
import { pickDefaultScope } from "../context/scope";
import type { CoverageSummary } from "../../shared/generated/CoverageSummary";
import type { ModelSettingsView } from "../../shared/generated/ModelSettingsView";
import type { InsightCardAction } from "../../shared/generated/InsightCardAction";
import { InsightCards } from "../insights/InsightCards";
import { AgentToolMenu, type AgentToolGroup } from "./AgentToolMenu";
import { handleTrapKey } from "./focusTrap";
import { selectDraft, useWorkspaceUi } from "./uiStore";
import { usePollingGate } from "../../app/usePollingActive";
import "../qa/qa.css";

export { pickDefaultScope };

const AGENT_STEP_STATUS_TEXT: Record<string, string> = {
  running: "执行中",
  awaiting_confirmation: "等待确认",
  ok: "完成",
  failed: "失败",
  canceled: "已取消",
};

const CONFIRMATION_TOOL_LABELS: Record<string, string> = {
  create_artifact: "创建产物",
  create_quiz: "生成题目",
  export_file: "导出文件",
};

function fileBasename(path: string | null): string {
  if (!path) return "";
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] ?? "";
}

/** 单次提问最多附带的图片数量(截图 + 本地图片共用同一上限)。 */
export const MAX_SCREENSHOTS = MAX_ATTACHMENTS;

type MessagesPage = {
  items: Array<import("../../shared/generated/MessageView").MessageView>;
  nextCursor?: string;
};

type Attachment = ComposerAttachment;

/**
 * 展示/落库文本:用户原文 + 附件轻量引用(空行分隔)。
 * 图片字节绝不内联;正文只出现 `attachment:<id>`,回看时再经 IPC 取图。
 */
export function buildUserMessageText(text: string, attachmentIds: string[]): string {
  const parts = [text.trim()];
  for (const id of attachmentIds) {
    parts.push(`![截图](attachment:${id})`);
  }
  return parts.filter((part) => part.length > 0).join("\n\n");
}

/**
 * 模型输入 question:纯文本 + 图片数量提示;**不含** base64。
 * 真正的图片由 attachmentIds 另行传给模型。
 */
export function buildAskQuestion(text: string, imageCount: number): string {
  const base = text.trim();
  if (imageCount <= 0) return base;
  return `${base}\n\n(用户附了 ${imageCount} 张图片,请结合图片内容回答)`;
}

/** 从消息正文提取 attachment:<id> 引用(只解析引用,不解析 base64)。 */
export function attachmentIdsFromText(text: string): string[] {
  const ids: string[] = [];
  const pattern = /!\[[^\]]*\]\(attachment:([A-Za-z0-9_-]+)\)/g;
  for (const match of text.matchAll(pattern)) {
    ids.push(match[1]!);
  }
  return ids;
}

const VISION_REASON_TEXT: Record<NonNullable<VisionStatusView["reason"]>, string> = {
  generate_reference_missing: "未配置生成模型,暂时无法把图片发给模型。",
  provider_unavailable: "当前模型提供方不可用,暂时无法发送图片。",
  remote_vision_disabled: "尚未允许把图片发送到远程模型。",
  vision_grant_required: "当前文档尚未授权把图片发送给该模型。",
};

function visionBlockedText(status: VisionStatusView | undefined, failed: boolean): string | null {
  if (failed) return "无法确认图片授权状态,已禁用图片发送。";
  if (!status) return "正在检查图片发送授权…";
  if (status.ready) return null;
  return status.reason ? VISION_REASON_TEXT[status.reason] : "图片发送未就绪。";
}

/** 仅失败/取消/中断的普通问答可重试;深度研究任务暂不支持 ai_retry。 */
function canRetryMessage(
  taskStatus: string | undefined,
  taskType?: string,
): boolean {
  if (taskType === "agent_run") return false;
  return taskStatus === "failed" || taskStatus === "canceled" || taskStatus === "interrupted";
}

/** 错误中文呈现:保留既有「提问未发出」兜底,服务端错误按统一展示映射。 */
function describeSendError(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    const presentation = describeAppError(error.appError);
    return (
      presentation.title + (presentation.actionLabel ? `(${presentation.actionLabel})` : "")
    );
  }
  return fallback;
}

type ToolKind = "quiz" | "summary" | "translation_page" | "translation_full";

const SUMMARY_TEMPLATE_LABELS: Record<string, string> = {
  study_note: "学习笔记",
  executive_summary: "执行摘要",
  faq: "FAQ",
  comparison: "对比表",
  timeline: "时间线",
};

function summaryTemplateLabel(template: string): string {
  return SUMMARY_TEMPLATE_LABELS[template] ?? "文档总结";
}

/** 会话栏展开状态:只保存可重建的界面偏好,不保存任何会话内容。 */
const AGENT_CONV_PREFS_KEY = "knowwise.agent.conversations.v1";

export function loadConversationSidebarCollapsed(): boolean {
  try {
    const raw = globalThis.localStorage?.getItem(AGENT_CONV_PREFS_KEY);
    if (!raw) return false;
    const parsed = JSON.parse(raw) as { collapsed?: unknown };
    return parsed.collapsed === true;
  } catch {
    return false;
  }
}

/** 会话列表相对时间;无效时间戳不虚构,返回空串。 */
export function relativeTimeLabel(timestamp: number, now = Date.now()): string {
  if (!Number.isFinite(timestamp) || timestamp <= 0) return "";
  const diff = Math.max(0, now - timestamp);
  const minute = 60_000;
  const hour = 60 * minute;
  const day = 24 * hour;
  if (diff < minute) return "刚刚";
  if (diff < hour) return `${Math.floor(diff / minute)} 分钟前`;
  if (diff < day) return `${Math.floor(diff / hour)} 小时前`;
  if (diff < 7 * day) return `${Math.floor(diff / day)} 天前`;
  const date = new Date(timestamp);
  const sameYear = date.getFullYear() === new Date(now).getFullYear();
  return sameYear
    ? `${date.getMonth() + 1} 月 ${date.getDate()} 日`
    : `${date.getFullYear()}/${date.getMonth() + 1}/${date.getDate()}`;
}

/** 自定义页码校验(1 ≤ start ≤ end ≤ pageCount);返回错误文案或范围。 */
export function validatePageRange(
  startText: string,
  endText: string,
  pageCount?: number,
): { range?: { start: number; end: number }; error?: string } {
  const start = Number.parseInt(startText, 10);
  const end = Number.parseInt(endText, 10);
  if (!Number.isFinite(start) || !Number.isFinite(end)) {
    return { error: "请输入有效页码。" };
  }
  if (start < 1 || end < start || (pageCount !== undefined && end > pageCount)) {
    return {
      error:
        pageCount !== undefined
          ? `页码范围需满足 1 ≤ 起始 ≤ 结束 ≤ ${pageCount}。`
          : "页码范围需满足 1 ≤ 起始 ≤ 结束。",
    };
  }
  return { range: { start, end } };
}

/** 覆盖真相:已读页/总页、完整/部分;没有页信息时不虚构页码。 */
export function coverageLabel(coverage?: CoverageSummary): string | null {
  if (!coverage) return null;
  const read = new Set<number>();
  const all = new Set<number>();
  const collect = (regions: string[], into: Set<number>) => {
    for (const region of regions) {
      const match = /^page:(\d+)$/.exec(region);
      if (match) into.add(Number(match[1]));
    }
  };
  collect(coverage.completedRegions, read);
  collect(coverage.completedRegions, all);
  collect(coverage.partialRegions, all);
  collect(coverage.missingRegions, all);
  const completeness = coverage.scopeCovered ? "完整" : "部分";
  if (all.size > 0) {
    return `已读 ${read.size}/${all.size} 页 · ${completeness}`;
  }
  return `范围${completeness}`;
}

export function AgentRegion({
  agentInputRef,
  active = true,
}: {
  agentInputRef?: React.Ref<HTMLTextAreaElement>;
  /** 面板此刻是否显示(右栏标签=AI 助手且右栏展开)。隐藏时不发起文档级查询。 */
  active?: boolean;
}) {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const documentId = useWorkspaceUi((s) => s.currentDocumentId);
  const versionId = useWorkspaceUi((s) => s.currentVersionId);
  const draft = useWorkspaceUi((s) => selectDraft(s, s.currentDocumentId));
  const setDraft = useWorkspaceUi((s) => s.setDraft);
  const viewport = useWorkspaceUi((s) =>
    s.currentDocumentId ? s.readerViewport[s.currentDocumentId] : undefined,
  );
  // 提问范围:当前页 ±N(默认 ±1)、自定义页码、本节或全文;可锁定页码。
  const [pageWindow, setPageWindow] = useState(1);
  const [pageLock, setPageLock] = useState<number | null>(null);
  const [fullScope, setFullScope] = useState(false);
  const [customRange, setCustomRange] = useState<{ start: number; end: number } | null>(null);
  const [customStart, setCustomStart] = useState("");
  const [customEnd, setCustomEnd] = useState("");
  const [scopeError, setScopeError] = useState<string | null>(null);
  const [sectionScope, setSectionScope] = useState(false);
  const [researchKeepBase, setResearchKeepBase] = useState(false);
  const [deepResearch, setDeepResearch] = useState(false);
  const [toolBusy, setToolBusy] = useState<string | null>(null);
  const [generationTask, setGenerationTask] = useState<{
    taskId: string; documentId: string; kind: "quiz" | "summary" | "translation_page" | "translation_full";
  } | null>(null);
  const [generationCancelBusy, setGenerationCancelBusy] = useState(false);
  const [toolError, setToolError] = useState<string | null>(null);
  const [toolNotice, setToolNotice] = useState<string | null>(null);
  const [summaryTemplate, setSummaryTemplate] = useState("study_note");
  const [toolConfirm, setToolConfirm] = useState<{
    kind: ToolKind;
    label: string;
    range: string;
    estimate: string;
  } | null>(null);
  const [showRawReasoning, setShowRawReasoning] = useState(true);
  const effectivePage = pageLock ?? viewport?.pageNumber;
  const headingPath = viewport?.headingPath ?? [];
  // 深度研究默认扩展一页,但完整展示 diff 并允许改回。
  const researchExpanded =
    deepResearch &&
    !researchKeepBase &&
    !fullScope &&
    customRange === null &&
    !sectionScope &&
    effectivePage !== undefined;
  const effectiveScopeWindow = researchExpanded ? Math.min(2, pageWindow + 1) : pageWindow;
  const conversationId = useQaStore((s) => s.currentConversationId);
  const setConversation = useQaStore((s) => s.setConversation);
  const [sendError, setSendError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [stream, setStream] = useState<StreamState>(EMPTY_STREAM);
  const streamRef = useRef<StreamState>(EMPTY_STREAM);
  const streamRevisionRef = useRef<number>(0);
  // 真实 answer.delta 次数按消息记:完成为 0–1 次时如实标注“一次性返回”,
  // 不把提供方的非增量输出伪装成逐块流式。
  const answerDeltaCountRef = useRef<Map<string, number>>(new Map());
  const [nonStreamingMessages, setNonStreamingMessages] = useState<Set<string>>(
    () => new Set(),
  );
  const [activeAgentTaskId, setActiveAgentTaskId] = useState<string | null>(null);
  const [activeAgentRevision, setActiveAgentRevision] = useState(0);
  const [agentCancelBusy, setAgentCancelBusy] = useState(false);
  const [confirmationBusy, setConfirmationBusy] = useState(false);
  const [confirmationPath, setConfirmationPath] = useState<string | null>(null);
  const [confirmationError, setConfirmationError] = useState<string | null>(null);


  const [convSidebarCollapsed, setConvSidebarCollapsed] = useState(
    loadConversationSidebarCollapsed,
  );
  const [convDrawerOpen, setConvDrawerOpen] = useState(false);
  const [narrowAgent, setNarrowAgent] = useState(false);
  const [exportConversationId, setExportConversationId] = useState<string | null>(null);
  const [allConvsOpen, setAllConvsOpen] = useState(false);
  const [gotoToast, setGotoToast] = useState<string | null>(null);
  const [lastToolKind, setLastToolKind] = useState<ToolKind | null>(null);
  const agentRootRef = useRef<HTMLElement | null>(null);
  const convTriggerRef = useRef<HTMLButtonElement | null>(null);
  const gotoToastTimerRef = useRef<number | null>(null);

  // 会话栏偏好:折叠状态刷新后保留;抽屉是窄栏临时状态,不落盘。
  useEffect(() => {
    try {
      globalThis.localStorage?.setItem(
        AGENT_CONV_PREFS_KEY,
        JSON.stringify({ collapsed: convSidebarCollapsed }),
      );
    } catch {
      // 存储不可用只影响下次启动的布局记忆。
    }
  }, [convSidebarCollapsed]);

  // 右栏 < 520px 时会话栏降级为抽屉,主聊天区不被挤坏。
  useEffect(() => {
    const element = agentRootRef.current;
    if (!element) return;
    const apply = (width: number) => {
      if (width > 0) setNarrowAgent(width < 520);
    };
    apply(element.getBoundingClientRect().width);
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) apply(entry.contentRect.width);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [documentId]);

  const closeConversationDrawer = useCallback(() => {
    setConvDrawerOpen(false);
    convTriggerRef.current?.focus();
  }, []);

  // 窄栏抽屉:Esc 关闭并把焦点还给「会话」按钮。
  useEffect(() => {
    if (!convDrawerOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      closeConversationDrawer();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [closeConversationDrawer, convDrawerOpen]);

  useEffect(() => () => {
    if (gotoToastTimerRef.current !== null) {
      window.clearTimeout(gotoToastTimerRef.current);
    }
  }, []);

  // 成功提示是次要信息:短暂停留后自动消失,不长期占用行内空间。
  useEffect(() => {
    if (toolNotice === null) return;
    const timer = window.setTimeout(() => setToolNotice(null), 6000);
    return () => window.clearTimeout(timer);
  }, [toolNotice]);

  const showGotoToast = useCallback((text: string) => {
    if (gotoToastTimerRef.current !== null) {
      window.clearTimeout(gotoToastTimerRef.current);
    }
    setGotoToast(text);
    gotoToastTimerRef.current = window.setTimeout(() => {
      gotoToastTimerRef.current = null;
      setGotoToast(null);
    }, 2800);
  }, []);

  // 切换文档时自动切换到该文档最近选择的会话
  const prevDocIdRef = useRef<string | null>(null);
  useEffect(() => {
    if (documentId === null) return;
    if (prevDocIdRef.current === documentId) return;
    const firstMount = prevDocIdRef.current === null;
    prevDocIdRef.current = documentId;
    const state = useQaStore.getState();
    const remembered = state.getConversationForDocument(documentId);
    // 首次挂载时调用方可能已选中当前文档的会话(测试/启动恢复);
    // 之后切换文档不得继承上一文档的会话,等该文档最近会话返回再选中。
    const initial = remembered ?? (firstMount ? state.currentConversationId : null);
    if (initial) {
      autoSelectConversationRef.current = null;
      setConversation(initial, documentId);
    } else {
      autoSelectConversationRef.current = documentId;
      setConversation(null, documentId);
    }
  }, [documentId, setConversation]);
  // 切换文档/会话时收起上一轮的生成状态:提示与错误属于上一条对话,
  // 跟着新会话显示会让人误以为本次生成了练习题/总结。
  useEffect(() => {
    setGenerationTask(null);
    setToolBusy(null);
    setToolError(null);
    setToolNotice(null);
    setToolConfirm(null);
    setLastToolKind(null);
  }, [documentId, conversationId]);

  const [attachments, setAttachments] = useState<Attachment[]>([]);
  // 用户上滚后停止跟随,显示「回到最新」。
  const [atBottom, setAtBottom] = useState(true);
  // 本轮引用的资料(artifact_version 来源);清空随发送成功。
  const [citedArtifacts, setCitedArtifacts] = useState<CitedArtifact[]>([]);
  const [citationPickerOpen, setCitationPickerOpen] = useState(false);
  // 笔记「问 AI」:把笔记投递成本轮引用来源(与引用资料同一链路)。
  // 订阅投递值而不是只在挂载时读一次:Agent 面板通常已经挂载,
  // 旧实现从笔记面板点「问 AI」时永远不会被消费。
  const pendingNoteSource = useWorkspaceUi((s) => s.pendingNoteSource);
  useEffect(() => {
    if (pendingNoteSource === null) return;
    const pending = useWorkspaceUi.getState().consumeNoteSource();
    if (pending === null) return;
    setCitedArtifacts((prev) => {
      const exists = prev.some(
        (item) => item.sourceType === "note" && item.versionId === pending.id,
      );
      if (exists) return prev;
      return [
        ...prev,
        {
          artifactId: "",
          versionId: pending.id,
          title: pending.title,
          kind: "note",
          sourceType: "note" as const,
        },
      ];
    });
  }, [pendingNoteSource]);
  // 分题型出题数量(全 0 = 混合自动)。
  const [quizCounts, setQuizCounts] = useState<QuizPerTypeCounts>(EMPTY_PER_TYPE_COUNTS);
  // 面板隐藏或页面不可见时把 800ms 轮询降为 5s:后台任务照常执行,
  // 终态仍会被发现并完成收尾(自动切资料页/真实错误提示),但不刷无意义 IPC。
  const pollGate = usePollingGate();
  // 摘要/出题与翻译共用同一轮询形状,但翻译走独立的只读命令。
  const translationPoll = generationTask?.kind === "translation_page"
    || generationTask?.kind === "translation_full";
  const generationStatus = useQuery({
    queryKey: ["generation", generationTask?.taskId ?? "none"],
    queryFn: () => translationPoll
      ? client.call<{ task: TaskView; artifact: ArtifactView | null }>(
          "translation_get", { taskId: generationTask!.taskId },
        )
      : client.call<{ task: TaskView; artifact: ArtifactView | null }>(
          "generation_get", { taskId: generationTask!.taskId },
        ),
    enabled: generationTask !== null,
    refetchInterval: (query) => {
      const status = query.state.data?.task.status;
      const terminal = status !== undefined && !["queued", "running"].includes(status);
      return pollGate(terminal ? 0 : 800, terminal ? 0 : 5000);
    },
    retry: false,
  });
  useEffect(() => {
    const result = generationStatus.data;
    if (!generationTask || !result || ["queued", "running"].includes(result.task.status)) return;
    const { task, artifact } = result;
    void queryClient.invalidateQueries({ queryKey: ["artifacts"] });
    void queryClient.invalidateQueries({ queryKey: ["tasks"] });
    if (task.status === "succeeded" && artifact) {
      // 出题结果不再塞进聊天流(会把历史消息挤出视野),统一在主阅读区打开;
      // 跨文档的结果只提示,不抢当前文档的阅读区。
      if (generationTask.documentId === useWorkspaceUi.getState().currentDocumentId) {
        window.dispatchEvent(
          new CustomEvent("knowwise-open-artifacts", {
            detail: { artifactId: artifact.id, read: generationTask.kind !== "quiz" },
          }),
        );
      }
      setToolNotice(
        generationTask.kind === "quiz"
          ? "练习题已生成，已在资料中打开;可查看题目、作答与导出。"
          : generationTask.kind === "summary"
            ? "总结已生成，可在资料标签阅览和导出。"
            : "翻译已生成，可在资料标签阅览;未完成段落保留原文并标注原因。",
      );
    } else if (task.status === "canceled") {
      setToolNotice("已取消生成，未保存新的产物。");
    } else {
      const presentation = task.error ? describeAppError(task.error) : null;
      const details =
        presentation !== null && presentation.detailLines.length > 0
          ? `(${presentation.detailLines.join(";")})`
          : "";
      setToolError(
        presentation
          ? `${presentation.title}${details}(${presentation.actionLabel ?? "请重试"});未保存任何产物。`
          : "生成未完成,请在任务中心查看原因后重试;未保存任何产物。",
      );
    }
    setToolBusy(null);
    setGenerationTask(null);
  }, [generationStatus.data, generationTask, queryClient]);
  async function cancelGeneration(): Promise<void> {
    if (!generationTask || generationCancelBusy) return;
    setGenerationCancelBusy(true);
    try {
      const cancelCommand = translationPoll ? "translation_cancel" : "generation_cancel";
      await client.call<TaskView>(cancelCommand, { taskId: generationTask.taskId });
      await generationStatus.refetch();
    } catch (error) {
      setToolError(describeSendError(error, "取消未完成，请在任务中心检查状态。"));
    } finally {
      setGenerationCancelBusy(false);
    }
  }
  const [captureError, setCaptureError] = useState<string | null>(null);
  const [capturing, setCapturing] = useState(false);
  const [attaching, setAttaching] = useState(false);
  const [allowingVision, setAllowingVision] = useState(false);
  const [retryingId, setRetryingId] = useState<string | null>(null);
  // 任务中心「查看结果」/「重试」定位:先切会话,消息加载后滚动并短暂高亮。
  const [revealTarget, setRevealTarget] = useState<{
    messageId: string;
    conversationId: string;
    documentId: string;
  } | null>(null);
  const [highlightedMessageId, setHighlightedMessageId] = useState<string | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const [conversationBusy, setConversationBusy] = useState(false);

  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const messagesRef = useRef<HTMLDivElement | null>(null);
  /** 文档切换后待自动选中最近会话(只在该文档没有记忆会话时)。 */
  const autoSelectConversationRef = useRef<string | null>(null);
  /** 用户是否贴近底部:只有贴近时才因新消息自动滚动(不打断回看)。 */
  const stickToBottomRef = useRef(true);

  const { supported: clipSupported, requestClip } = useScreenClip();

  const suggestedPrompts = useQuery({
    queryKey: ["document", "suggested-prompts", documentId ?? "none"],
    queryFn: () =>
      client.call<{
        documentId: string;
        domainLabel: string;
        items: Array<{ id: string; icon: string; prompt: string }>;
      }>("document_get_suggested_prompts", { documentId: documentId! }),
    enabled: documentId !== null && active,
    staleTime: 60_000,
  });

  // 读取全局设置:默认模式、默认页范围、是否显示原始推理(P0 1E)。
  const appSettings = useQuery({
    queryKey: ["settings"],
    queryFn: () => client.call<ModelSettingsView>("settings_get", {}),
    staleTime: 30_000,
  });
  const settingsAppliedRef = useRef(false);
  const runtimeSettings = appSettings.data?.settings;
  const qualityRef =
    runtimeSettings?.qualityReference ??
    runtimeSettings?.generateReference ??
    runtimeSettings?.fastReference ??
    null;
  // 质量模式真实能力:工具调用未声明时如实提示“仅单步”,不假装多步。
  const qualityCapability = useQuery<ModelCapabilityView>({
    queryKey: [
      "model-capability",
      qualityRef?.providerId ?? "none",
      qualityRef?.modelId ?? "none",
    ],
    queryFn: () =>
      client.call<ModelCapabilityView>("model_capability_get", {
        providerId: qualityRef!.providerId,
        modelId: qualityRef!.modelId,
      }),
    enabled: deepResearch && qualityRef !== null,
    staleTime: 60_000,
    retry: false,
  });
  useEffect(() => {
    if (!runtimeSettings || settingsAppliedRef.current) return;
    settingsAppliedRef.current = true;
    setShowRawReasoning(runtimeSettings.showRawReasoning);
    const window = Number(runtimeSettings.defaultPageWindow);
    if (Number.isFinite(window)) setPageWindow(Math.min(2, Math.max(0, window)));
    if (runtimeSettings.defaultMode === "quality") setDeepResearch(true);
  }, [runtimeSettings]);

  const conversations = useQuery({
    queryKey: ["conversation", "list", documentId ?? "none"],
    queryFn: () =>
      client.call<{ items: ConversationView[] }>("conversation_list", {
        documentId: documentId!,
        limit: 20,
      }),
    enabled: documentId !== null && active,
  });

  useEffect(() => {
    if (documentId === null || conversationId !== null) return;
    if (autoSelectConversationRef.current !== documentId) return;
    const latest = conversations.data?.items[0];
    if (!latest) return;
    autoSelectConversationRef.current = null;
    setConversation(latest.id, documentId);
  }, [conversations.data, documentId, conversationId, setConversation]);

  const messages = useQuery({
    queryKey: ["messages", conversationId ?? "none"],
    queryFn: () =>
      client.call<MessagesPage>("conversation_get_messages", {
        conversationId: conversationId!,
        limit: 50,
      }),
    enabled: conversationId !== null,
    refetchInterval: (query) => {
      const pending = (query.state.data?.items ?? []).some(
        (m) => m.role === "assistant" && m.answerStatus === "pending",
      );
      return pending ? pollGate(2000, 5000) : false;
    },
  });

  /**
   * 真实流事件 reducer:只做临时展示;终态后刷新消息再由数据库接管。
   *
   * 高频 delta 合并到下一个动画帧再 setState:provider 常按 10–30ms 吐出小块,
   * 每个事件各一次渲染会让长答案输出期持续重排。合并只改变"何时画",
   * 不改变事件内容与顺序(run.finished/run.failed 立即应用,不等下一帧)。
   */
  const pendingStreamEventsRef = useRef<StreamEventV1[]>([]);
  const streamRafRef = useRef(0);
  const flushStreamEvents = useCallback(() => {
    streamRafRef.current = 0;
    const queued = pendingStreamEventsRef.current;
    if (queued.length === 0) return;
    pendingStreamEventsRef.current = [];
    setStream((previous) => {
      let next = previous;
      for (const event of queued) next = reduceStream(next, event);
      streamRef.current = next;
      return next;
    });
  }, []);
  useEffect(
    () => () => {
      if (streamRafRef.current !== 0) cancelAnimationFrame(streamRafRef.current);
      streamRafRef.current = 0;
    },
    [],
  );
  const handleStreamEvent = useCallback(
    (raw: unknown) => {
      const event = raw as StreamEventV1;
      if (!event || event.schemaVersion !== "stream_event.v1") return;
      // 仅调试/验收开关:记录事件种类/长度/时间,不记录正文与密钥。
      const debugHost = globalThis as unknown as {
        __KW_STREAM_DEBUG__?: boolean;
        __kwStreamLog?: Array<Record<string, unknown>>;
      };
      if (debugHost.__KW_STREAM_DEBUG__ === true) {
        const log = debugHost.__kwStreamLog ?? [];
        log.push({
          at: Date.now(),
          kind: event.kind,
          seq: event.seq,
          taskId: event.taskId ?? null,
          messageId: event.messageId ?? null,
          textLen: event.text?.length ?? 0,
          elapsedMs: event.elapsedMs ?? null,
        });
        debugHost.__kwStreamLog = log.slice(-500);
      }
      const messageKey = event.messageId ?? streamRef.current.messageId;
      if (event.kind === "run.started" && messageKey !== null) {
        answerDeltaCountRef.current.set(messageKey, 0);
      } else if (event.kind === "answer.delta" && messageKey !== null) {
        answerDeltaCountRef.current.set(
          messageKey,
          (answerDeltaCountRef.current.get(messageKey) ?? 0) + 1,
        );
      }
      const terminal = event.kind === "run.finished" || event.kind === "run.failed";
      pendingStreamEventsRef.current.push(event);
      if (terminal) {
        // 终态必须立刻可见:先取消挂起的帧,再同步应用。
        if (streamRafRef.current !== 0) {
          cancelAnimationFrame(streamRafRef.current);
          streamRafRef.current = 0;
        }
        flushStreamEvents();
        if (
          event.kind === "run.finished" &&
          messageKey !== null &&
          (answerDeltaCountRef.current.get(messageKey) ?? 0) <= 1
        ) {
          setNonStreamingMessages((previous) => {
            if (previous.has(messageKey)) return previous;
            const next = new Set(previous);
            next.add(messageKey);
            return next;
          });
        }
        void queryClient.invalidateQueries({ queryKey: ["messages"] });
        void queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
        return;
      }
      if (streamRafRef.current === 0) {
        streamRafRef.current = requestAnimationFrame(flushStreamEvents);
      }
    },
    [queryClient, flushStreamEvents],
  );

  // 消息落定(不再有 pending)后清空临时流,避免与最终答案重复展示。
  useEffect(() => {
    const items = messages.data?.items ?? [];
    const pending = items.some(
      (message) => message.role === "assistant" && message.answerStatus === "pending",
    );
    const settled =
      streamRef.current.status === "finished" || streamRef.current.status === "failed";
    if (!pending && settled) {
      streamRef.current = EMPTY_STREAM;
      setStream(EMPTY_STREAM);
    }
  }, [messages.data]);

  // 图片发送就绪度:远程需全局开关 + 文档 Vision 授权;本地按现有规则。
  const vision = useQuery<VisionStatusView>({
    queryKey: ["vision", "status", documentId ?? "none"],
    queryFn: () =>
      client.call<VisionStatusView>("vision_get_status", { documentId: documentId! }),
    enabled: documentId !== null && active,
    retry: false,
  });

  // P2 深度研究:步骤时间线轮询(只含工具名/状态/摘要)。
  const agentSteps = useQuery<AgentRunStepsView>({
    queryKey: qk.agentSteps(activeAgentTaskId ?? "none"),
    queryFn: () =>
      client.call<AgentRunStepsView>("agent_get_steps", {
        taskId: activeAgentTaskId!,
      }),
    enabled: activeAgentTaskId !== null,
    refetchInterval: pollGate(1500, 5000),
    retry: false,
  });
  const agentRunning =
    activeAgentTaskId !== null &&
    (agentSteps.data === undefined ||
      agentSteps.data.status === "running" ||
      agentSteps.isLoading);
  const pendingConfirmation: AgentStepView | null =
    agentSteps.data?.steps.find(
      (step) => step.status === "awaiting_confirmation",
    ) ?? null;
  // 真实出现工具步骤即证伪“仅单步”:立即刷新能力表(后端已自动修正)。
  const qualityToolStepSeen =
    agentSteps.data?.steps.some((step) => step.kind === "tool") ?? false;
  useEffect(() => {
    if (!qualityToolStepSeen) return;
    void queryClient.invalidateQueries({ queryKey: ["model-capability"] });
  }, [qualityToolStepSeen, queryClient]);

  useEffect(() => {
    setConfirmationPath(null);
    setConfirmationError(null);
  }, [pendingConfirmation?.stepId]);

  useEffect(() => {
    if (activeAgentTaskId === null) return;
    if (agentSteps.isError) {
      void queryClient.invalidateQueries({ queryKey: ["messages"] });
      setActiveAgentTaskId(null);
      setActiveAgentRevision(0);
      setSendError("研究步骤读取失败,请查看消息状态或重试");
      return;
    }
    const status = agentSteps.data?.status;
    if (status === "succeeded" || status === "failed" || status === "canceled") {
      void queryClient.invalidateQueries({ queryKey: ["messages"] });
      setActiveAgentTaskId(null);
      setActiveAgentRevision(0);
    }
  }, [activeAgentTaskId, agentSteps.data?.status, agentSteps.isError, queryClient]);

  const messageAttachmentIds = useMemo(() => {
    const ids = new Set<string>();
    for (const message of messages.data?.items ?? []) {
      if (message.content.schemaVersion !== "user_message.v1") continue;
      for (const id of attachmentIdsFromText(message.content.text)) ids.add(id);
    }
    return [...ids];
  }, [messages.data]);

  const thumbnailQueries = useQueries({
    queries: messageAttachmentIds.map((id) => ({
      queryKey: ["attachment", "thumbnail", id],
      queryFn: () =>
        client.call<AttachmentThumbnail>("attachment_thumbnail", { attachmentId: id }),
      staleTime: 5 * 60_000,
      retry: false,
    })),
  });

  const attachmentImages = useMemo(() => {
    const map: Record<string, string> = {};
    thumbnailQueries.forEach((query) => {
      if (query.data) map[query.data.id] = query.data.dataUrl;
    });
    return map;
  }, [thumbnailQueries]);

  // 图片记忆:最近 4 轮用户图片可选重发(默认不自动附带)。
  const recentImageIds = useMemo(() => {
    const items = messages.data?.items ?? [];
    const selected = new Set(attachments.map((item) => item.id));
    const ids: string[] = [];
    let turns = 0;
    for (let index = items.length - 1; index >= 0; index -= 1) {
      const message = items[index];
      if (
        message === undefined ||
        message.role !== "user" ||
        message.content.schemaVersion !== "user_message.v1"
      ) {
        continue;
      }
      turns += 1;
      for (const id of attachmentIdsFromText(message.content.text)) {
        if (!selected.has(id) && !ids.includes(id)) ids.push(id);
      }
      if (turns >= 4 || ids.length >= MAX_SCREENSHOTS) break;
    }
    return ids.slice(0, MAX_SCREENSHOTS);
  }, [messages.data, attachments]);

  useEffect(() => {
    const onReveal = (event: Event) => {
      const detail = (
        event as CustomEvent<{
          messageId?: string;
          conversationId?: string;
          documentId?: string;
        }>
      ).detail;
      if (!detail?.messageId || !detail.conversationId || !detail.documentId) return;
      setRevealTarget({
        messageId: detail.messageId,
        conversationId: detail.conversationId,
        documentId: detail.documentId,
      });
    };
    window.addEventListener("knowwise-reveal-message", onReveal);
    return () => window.removeEventListener("knowwise-reveal-message", onReveal);
  }, []);

  useEffect(() => {
    if (!revealTarget || documentId === null) return;
    if (revealTarget.documentId !== documentId) return;
    // 目标可能属于其他会话:先切过去;滚动在高亮 effect 中等待消息加载。
    setConversation(revealTarget.conversationId, documentId);
    stickToBottomRef.current = false;
  }, [revealTarget, documentId, setConversation]);

  useEffect(() => {
    if (!revealTarget || !messages.data) return;
    if (conversationId !== revealTarget.conversationId) return;
    const element = document.getElementById(`message-${revealTarget.messageId}`);
    if (!element) return;
    const reduced =
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    element.scrollIntoView({
      block: "center",
      behavior: reduced ? "auto" : "smooth",
    });
    setHighlightedMessageId(revealTarget.messageId);
    setRevealTarget(null);
  }, [messages.data, conversationId, revealTarget]);

  useEffect(() => {
    if (highlightedMessageId === null) return;
    const timer = window.setTimeout(() => setHighlightedMessageId(null), 3600);
    return () => window.clearTimeout(timer);
  }, [highlightedMessageId]);

  const tasks = useQuery({
    queryKey: qk.tasks({
      documentId,
      activeOnly: false,
    }),
    queryFn: () =>
      client.call<{ items: TaskView[] }>("task_list", {
        documentId: documentId ?? undefined,
        activeOnly: false,
        limit: 30,
      }),
  });

  const docMetaQuery = useQuery({
    queryKey: ["document", documentId],
    queryFn: async () => {
      if (!documentId) return null;
      return client.call<{ id: string; displayName: string; format?: string }>(
        "document_get",
        { id: documentId },
      );
    },
    enabled: documentId !== null,
    staleTime: 60_000,
  });

  const docTitle = useMemo(() => {
    const raw = docMetaQuery.data?.displayName ?? "";
    if (!raw) return "当前文档";
    const cleaned = raw.replace(/\.[a-zA-Z0-9]+$/, "").replace(/^[\d_-]+/, "").trim();
    return cleaned || raw;
  }, [docMetaQuery.data?.displayName]);

  // 外部 ref 与内部 ref 合并(键盘快捷键聚焦输入框)。
  const setTextareaRef = useCallback(
    (node: HTMLTextAreaElement | null) => {
      textareaRef.current = node;
      if (typeof agentInputRef === "function") {
        agentInputRef(node);
      } else if (agentInputRef !== null && agentInputRef !== undefined) {
        (agentInputRef as { current: HTMLTextAreaElement | null }).current = node;
      }
    },
    [agentInputRef],
  );

  // 输入区自适应高度(3 行起,最多 240px;jsdom 下 scrollHeight 为 0,跳过)。
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    const next = el.scrollHeight;
    el.style.height = next > 0 ? `${Math.min(Math.max(next, 76), 240)}px` : "";
  }, [draft]);

  // 切换文档:旧文档的附件 ID 不再适用,直接清空输入区附件。
  useEffect(() => {
    setAttachments([]);
    setCaptureError(null);
  }, [documentId]);

  // 切换会话:默认回到底部。
  useEffect(() => {
    stickToBottomRef.current = true;
  }, [conversationId]);

  // 新消息(或轮询更新)时,仅当用户接近底部才自动滚动。
  useLayoutEffect(() => {
    const el = messagesRef.current;
    if (!el || !stickToBottomRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [messages.data, conversationId]);

  // 流式输出期间持续跟随:逐字出现的正文会把内容撑高,只有等 messages
  // 数据变化才滚动会让新字堆在视口下方。这里按帧对齐(值不变时写入是
  // 无操作),用户主动上滚即停止跟随(stickToBottomRef 由滚动事件维护)。
  useEffect(() => {
    if (stream.status !== "running") return;
    let raf = 0;
    const follow = () => {
      const el = messagesRef.current;
      if (el !== null && stickToBottomRef.current) {
        const bottom = el.scrollHeight - el.clientHeight;
        if (Math.abs(el.scrollTop - bottom) > 1) el.scrollTop = bottom;
      }
      raf = requestAnimationFrame(follow);
    };
    raf = requestAnimationFrame(follow);
    return () => cancelAnimationFrame(raf);
  }, [stream.status]);

  function handleMessagesScroll(): void {
    const el = messagesRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    stickToBottomRef.current = nearBottom;
    setAtBottom(nearBottom);
  }

  function attachmentError(error: unknown, fallback: string): string {
    if (error instanceof Error) {
      if (error.message === "attachment_too_large") {
        return `单张图片压缩后不能超过 ${Math.round(MAX_ATTACHMENT_BYTES / 1_000_000)}MB。`;
      }
      if (error.message === "attachment_total_too_large") {
        return "图片总大小超过 5MB,请减少图片后重试。";
      }
      if (error.message === "attachment_image_decode_failed") {
        return "图片无法解码,请换一张图片。";
      }
    }
    if (error instanceof ApiError) {
      return describeAppError(error.appError).title;
    }
    return fallback;
  }

  async function appendImage(dataUrl: string, name?: string): Promise<void> {
    if (documentId === null) throw new Error("attachment_no_document");
    if (attachments.length >= MAX_ATTACHMENTS) {
      throw new Error("attachment_count_exceeded");
    }
    const compressed = await compressImageDataUrl(dataUrl);
    if (compressed.byteSize > MAX_ATTACHMENT_BYTES) {
      throw new Error("attachment_too_large");
    }
    if (totalAttachmentBytes(attachments) + compressed.byteSize > MAX_TOTAL_ATTACHMENT_BYTES) {
      throw new Error("attachment_total_too_large");
    }
    const uploaded = await uploadImageAttachment(client, documentId, compressed, name);
    setAttachments((prev) =>
      prev.length >= MAX_ATTACHMENTS ? prev : [...prev, uploaded],
    );
  }

  async function handleScreenshot(): Promise<void> {
    if (!clipSupported || capturing || attaching) return;
    if (attachments.length >= MAX_SCREENSHOTS) {
      setCaptureError(`最多添加 ${MAX_SCREENSHOTS} 张图片。`);
      return;
    }
    setCapturing(true);
    setCaptureError(null);
    try {
      const dataUrl = await requestClip();
      if (dataUrl === null) return; // 用户取消 / 环境不支持:保持安静
      if (!dataUrl.startsWith("data:image/")) {
        setCaptureError("截图数据无效,请重试。");
        return;
      }
      await appendImage(dataUrl, "截图");
    } catch (error) {
      setCaptureError(attachmentError(error, "截图处理失败,请重试。"));
    } finally {
      setCapturing(false);
    }
  }

  async function handlePickImages(): Promise<void> {
    if (attaching || capturing) return;
    const remaining = MAX_ATTACHMENTS - attachments.length;
    if (remaining <= 0) {
      setCaptureError(`最多添加 ${MAX_SCREENSHOTS} 张图片。`);
      return;
    }
    setAttaching(true);
    setCaptureError(null);
    try {
      const paths = await dialogPickImages();
      if (paths.length === 0) return; // 用户取消
      let added = 0;
      for (const path of paths) {
        if (added >= remaining) {
          setCaptureError(`最多添加 ${MAX_SCREENSHOTS} 张图片,超出部分未加入。`);
          break;
        }
        try {
          const local = await readLocalImage(client, path);
          await appendImage(local.dataUrl);
          added += 1;
        } catch (error) {
          setCaptureError(attachmentError(error, "图片读取或上传失败,请重试。"));
        }
      }
    } catch {
      setCaptureError("无法打开图片选择对话框。");
    } finally {
      setAttaching(false);
    }
  }

  function removeAttachment(id: string): void {
    setAttachments((prev) => prev.filter((item) => item.id !== id));
    setCaptureError(null);
    // 未发送的附件同时清理落盘文件;已绑定的会被后端拒绝并交由级联清理。
    void client.call("attachment_delete", { attachmentId: id }).catch(() => undefined);
  }

  /** 图片记忆:复制历史图片为新的未绑定附件;用户点击才发生,默认不重发。 */
  async function reuseImage(sourceId: string): Promise<void> {
    if (attaching || attachments.length >= MAX_SCREENSHOTS) return;
    if (totalAttachmentBytes(attachments) >= MAX_TOTAL_ATTACHMENT_BYTES) {
      setCaptureError("图片总大小超过 5MB,请先移除部分图片。");
      return;
    }
    setAttaching(true);
    setCaptureError(null);
    try {
      const cloned = await client.call<AttachmentView>("attachment_clone", {
        attachmentId: sourceId,
      });
      const dataUrl = attachmentImages[sourceId];
      if (!dataUrl) {
        setCaptureError("该历史图片已不可用,请重新添加。");
        return;
      }
      setAttachments((prev) => {
        if (prev.length >= MAX_ATTACHMENTS) return prev;
        if (totalAttachmentBytes(prev) + cloned.byteSize > MAX_TOTAL_ATTACHMENT_BYTES) {
          return prev;
        }
        return [
          ...prev,
          {
            id: cloned.id,
            dataUrl,
            byteSize: cloned.byteSize,
            name: cloned.name ?? undefined,
          },
        ];
      });
    } catch (error) {
      setCaptureError(attachmentError(error, "历史图片复用失败,请重新添加。"));
    } finally {
      setAttaching(false);
    }
  }

  async function allowRemoteVision(): Promise<void> {
    if (documentId === null || allowingVision) return;
    setAllowingVision(true);
    setCaptureError(null);
    try {
      await client.call<VisionStatusView>("vision_allow_remote", {
        documentId,
        idempotencyKey: client.newIdempotencyKey(),
      });
      await queryClient.invalidateQueries({ queryKey: ["vision", "status"] });
      await queryClient.invalidateQueries({ queryKey: ["privacy", "status"] });
      await queryClient.invalidateQueries({ queryKey: ["settings"] });
      await queryClient.invalidateQueries({ queryKey: ["model"] });
    } catch (error) {
      setCaptureError(attachmentError(error, "图片授权失败,请重试。"));
    } finally {
      setAllowingVision(false);
    }
  }

  function startRename(conversation: ConversationView): void {
    setRenamingId(conversation.id);
    setRenameDraft(conversation.title || "新会话");
    setConfirmDeleteId(null);
  }

  async function commitRename(conversation: ConversationView): Promise<void> {
    const title = renameDraft.trim();
    setRenamingId(null);
    if (title === "" || title === conversation.title) return;
    setConversationBusy(true);
    try {
      await client.call("conversation_rename", {
        conversationId: conversation.id,
        title,
      });
      await queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
    } catch (error) {
      setSendError(describeSendError(error, "会话重命名失败"));
    } finally {
      setConversationBusy(false);
    }
  }

  /** 显式新建会话:走真实 conversation_create,而不是只在本地清空选择。 */
  async function createConversation(): Promise<void> {
    if (documentId === null || conversationBusy) return;
    setConversationBusy(true);
    setSendError(null);
    try {
      const created = await client.call<ConversationView>("conversation_create", {
        documentId,
        title: "",
        idempotencyKey: client.newIdempotencyKey(),
      });
      setConversation(created.id, documentId);
      queryClient.removeQueries({ queryKey: ["messages"] });
      await queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
      setConvDrawerOpen(false);
    } catch (error) {
      setSendError(describeSendError(error, "新建会话失败"));
    } finally {
      setConversationBusy(false);
    }
  }

  async function deleteConversation(conversation: ConversationView): Promise<void> {
    setConversationBusy(true);
    setSendError(null);
    try {
      await client.call("conversation_delete", { conversationId: conversation.id });
      setConfirmDeleteId(null);
      if (conversation.id === conversationId) {
        setConversation(null);
        queryClient.removeQueries({ queryKey: ["messages"] });
      }
      await queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
    } catch (error) {
      setSendError(describeSendError(error, "会话删除失败"));
    } finally {
      setConversationBusy(false);
    }
  }

  /** 流式命令会在后台启动后立刻返回 TaskView:马上把真实 taskId/revision
   *  写进流状态,取消按钮才不必等第一条 stage 事件。 */
  function bindStreamTask(task: TaskView): void {
    streamRevisionRef.current = task.stateVersion;
    if (streamRef.current.status === "running") {
      streamRef.current = { ...streamRef.current, taskId: task.id };
      setStream(streamRef.current);
    }
  }

  /** 真实 Tauri 环境走 Channel;测试/浏览器回退到同步命令。 */
  function sendAskCommand(payload: Record<string, unknown>): Promise<TaskView> {
    if (!client.supportsStreaming) {
      return client
        .call<TaskView>(deepResearch ? "agent_run" : "ai_ask", payload)
        .then((task) => {
          // 无 Channel 环境:同步命令返回时消息已落定,清空临时流。
          streamRef.current = EMPTY_STREAM;
          setStream(EMPTY_STREAM);
          return task;
        });
    }
    return client.callStreamed<TaskView>(
      deepResearch ? "agent_run_streamed" : "ai_ask_streamed",
      payload,
      handleStreamEvent,
    );
  }

  /** 冻结当前范围(与提问同源;工具栏产物/出题使用)。 */
  async function freezeToolScope() {
    if (documentId === null || versionId === null) throw new Error("missing_document");
    const suggestion = await client.call<ContextSuggestion>("context_suggest", {
      documentId,
      versionId,
      pageNumber: effectivePage,
      nearbyPages: effectiveScopeWindow,
      pageCount: viewport?.pageCount,
    });
    const scope = scopeForSuggestion(suggestion);
    return client.call<ContextSnapshotView>("context_freeze", {
      documentId,
      scopeDraft: scope,
      idempotencyKey: `${client.newIdempotencyKey()}:tool-freeze`,
    });
  }

  function estimateTool(kind: ToolKind): { range: string; estimate: string } {
    if (kind === "translation_full") {
      const pages = viewport?.pageCount;
      return {
        range: "全文",
        estimate: `${pages !== null && pages !== undefined ? `${pages} 页` : "全文页数"} · 约 20–120 秒 · 分批模型调用`,
      };
    }
    if (kind === "translation_page") {
      return {
        range: currentScopeLabel(),
        estimate: "约 1–3 段/页 · 约 8–30 秒 · 需要一次模型调用",
      };
    }
    const range = currentScopeLabel();
    const pages =
      customRange !== null
        ? customRange.end - customRange.start + 1
        : effectivePage !== undefined && !fullScope && !sectionScope
          ? effectiveScopeWindow * 2 + 1
          : viewport?.pageCount ?? null;
    const pageText = pages !== null ? `${pages} 页` : "范围相关页数";
    const seconds = kind === "quiz" ? "约 10–40 秒" : "约 8–30 秒";
    return {
      range,
      estimate: `${pageText} · ${seconds} · 需要一次模型调用`,
    };
  }

  function requestTool(kind: ToolKind): void {
    if (toolBusy !== null) return;
    setToolError(null);
    setToolNotice(null);
    const { range, estimate } = estimateTool(kind);
    const label =
      kind === "quiz"
        ? "生成混合练习题(quiz.v2)"
        : kind === "summary"
          ? "总结成文档"
          : kind === "translation_page"
            ? "生成当前页/本节译文产物"
            : "翻译整篇文档";
    setToolNotice(null);
    setToolConfirm({
      kind,
      label,
      range,
      // 成本/时间预估开关关闭时不显示估算,仍保留真实范围。
      estimate: runtimeSettings?.showCostEstimate === false ? "" : estimate,
    });
  }

  async function freezeTranslationScope(full: boolean): Promise<ContextSnapshotView> {
    if (documentId === null || versionId === null) throw new Error("missing_document");
    let scope: ScopeDraft;
    if (full) {
      scope = { kind: "full_document", confirmed: true };
    } else if (effectivePage !== undefined && effectivePage >= 1) {
      scope = { kind: "pages", startPage: effectivePage, endPage: effectivePage };
    } else if (headingPath.length > 0) {
      scope = { kind: "section", headingPath };
    } else {
      const suggestion = await client.call<ContextSuggestion>("context_suggest", {
        documentId,
        versionId,
      });
      scope = pickDefaultScope(suggestion.candidates);
    }
    return client.call<ContextSnapshotView>("context_freeze", {
      documentId,
      scopeDraft: scope,
      idempotencyKey: `${client.newIdempotencyKey()}:tool-freeze`,
    });
  }

  async function runTool(kind: ToolKind): Promise<void> {
    if (documentId === null || versionId === null) return;
    setToolConfirm(null);
    setToolBusy(kind);
    setLastToolKind(kind);
    setToolError(null);
    setToolNotice(null);
    let backgroundStarted = false;
    try {
      const snapshot =
        kind === "translation_page"
          ? await freezeTranslationScope(false)
          : kind === "translation_full"
            ? await freezeTranslationScope(true)
            : await freezeToolScope();
      if (kind === "quiz") {
        const total = quizTotal(quizCounts);
        const task = await client.call<TaskView>("quiz_generate_v2", {
          documentId,
          snapshotId: snapshot.id,
          config: {
            // 显式分题型:总数取各题型之和;否则 5 题混合自动。
            count: isPerTypeExplicit(quizCounts) ? total : 5,
            types: [],
            difficulties: ["medium"],
            style: "",
            perTypeCounts: quizCounts,
          },
          idempotencyKey: client.newIdempotencyKey(),
        });
        setGenerationTask({ taskId: task.id, documentId, kind });
        backgroundStarted = true;
        return;
      } else if (kind === "summary") {
        const task = await client.call<TaskView>("artifact_generate", {
          documentId,
          snapshotId: snapshot.id,
          title: `${summaryTemplateLabel(summaryTemplate)} · ${snapshot.summary.name}`,
          template: summaryTemplate,
          idempotencyKey: client.newIdempotencyKey(),
        });
        setGenerationTask({ taskId: task.id, documentId, kind });
        backgroundStarted = true;
        return;
      } else {
        // 整篇翻译是 150–300s 长任务:提交即返回,由翻译轮询跟踪终态,
        // 不占用 IPC、不阻塞阅读与切文档。
        const task = await client.call<TaskView>("translation_generate", {
          documentId,
          snapshotId: snapshot.id,
          targetLanguage: runtimeSettings?.translationTargetLanguage ?? "zh-CN",
          style: "faithful",
          glossary: (runtimeSettings?.translationGlossary ?? []).map((entry) => ({
            source: entry.source,
            target: entry.target,
          })),
          title: `${kind === "translation_page" ? "当前页/本节译文" : "整篇翻译"} · ${snapshot.summary.name}`,
          idempotencyKey: client.newIdempotencyKey(),
        });
        setGenerationTask({ taskId: task.id, documentId, kind });
        backgroundStarted = true;
        return;
      }
      window.dispatchEvent(new CustomEvent("knowwise-open-artifacts"));
    } catch (error) {
      if (error instanceof ApiError && error.appError.code === "IPC_TIMEOUT") {
        // 提交本身超时:任务可能已在后台创建;不误报失败,提示到任务中心核对。
        await Promise.all([
          queryClient.invalidateQueries({ queryKey: ["artifacts", documentId] }),
          queryClient.invalidateQueries({ queryKey: ["tasks"] }),
        ]);
        setToolNotice("暂时无法确认生成是否已提交,请到任务中心核对;避免重复提交造成重复生成。");
      } else if (error instanceof ApiError && error.appError.code === "QUIZ_QUESTION_INVALID") {
        setToolError("模型没有返回可恢复的题目内容，请缩小范围或重试。");
      } else if (kind === "translation_page" || kind === "translation_full") {
        setToolError(describeSendError(error, "翻译未完成;已保留原文,可缩小范围重试"));
      } else {
        setToolError(describeSendError(error, "产物生成失败"));
      }
    } finally {
      if (!backgroundStarted) setToolBusy(null);
    }
  }

  /** 发送时冻结的范围:全文明示 → 用户选择优先;PDF 用当前页 ±N。 */
  function scopeForSuggestion(suggestion: ContextSuggestion): ScopeDraft {
    if (fullScope) {
      return { kind: "full_document", confirmed: true };
    }
    if (customRange !== null) {
      return { kind: "pages", startPage: customRange.start, endPage: customRange.end };
    }
    if (sectionScope && headingPath.length > 0) {
      return { kind: "section", headingPath };
    }
    if (effectivePage !== undefined && effectivePage >= 1) {
      const startPage = Math.max(1, effectivePage - effectiveScopeWindow);
      const endPage =
        viewport?.pageCount !== undefined
          ? Math.min(viewport.pageCount, effectivePage + effectiveScopeWindow)
          : effectivePage + effectiveScopeWindow;
      return { kind: "pages", startPage, endPage };
    }
    // MD/DOCX 默认围绕所处标题路径(本节),不虚构页码,也不静默扩到全文。
    if (headingPath.length > 0) {
      return { kind: "section", headingPath };
    }
    return pickDefaultScope(suggestion.candidates);
  }

  /** 当前范围文案(与冻结前的 scopeDraft 完全一致,不虚构页码)。 */
  function currentScopeLabel(): string {
    if (fullScope) return "全文";
    if (customRange !== null) {
      return customRange.start === customRange.end
        ? `第 ${customRange.start} 页`
        : `第 ${customRange.start}–${customRange.end} 页`;
    }
    if (sectionScope && headingPath.length > 0) {
      return `本节 · ${headingPath[headingPath.length - 1]}`;
    }
    if (effectivePage !== undefined && effectivePage >= 1) {
      const start = Math.max(1, effectivePage - effectiveScopeWindow);
      const end =
        viewport?.pageCount !== undefined
          ? Math.min(viewport.pageCount, effectivePage + effectiveScopeWindow)
          : effectivePage + effectiveScopeWindow;
      return start === end ? `p.${start}` : `p.${start}–${end}`;
    }
    if (headingPath.length > 0) {
      return `本节 · ${headingPath[headingPath.length - 1]}`;
    }
    return "当前位置/本节";
  }

  function applyCustomRange(): void {
    const result = validatePageRange(customStart, customEnd, viewport?.pageCount);
    if (result.error !== undefined || result.range === undefined) {
      setScopeError(result.error ?? "页码范围无效。");
      return;
    }
    setScopeError(null);
    setFullScope(false);
    setSectionScope(false);
    setCustomRange(result.range);
  }

  async function handleSuggestedClick(promptText: string) {
    if (documentId === null || sending || agentRunning || streamRef.current.status === "running")
      return;
    setSending(true);
    setSendError(null);
    try {
      const idempotencyKey = client.newIdempotencyKey();
      const suggestion = await client.call<ContextSuggestion>("context_suggest", {
        documentId,
        versionId: versionId ?? undefined,
        pageNumber: effectivePage,
        nearbyPages: pageWindow,
        pageCount: viewport?.pageCount,
      });
      const scope = scopeForSuggestion(suggestion);
      const snapshot = await client.call<ContextSnapshotView>("context_freeze", {
        documentId,
        scopeDraft: scope,
        idempotencyKey: `${idempotencyKey}:freeze`,
      });
      streamRef.current = {
        ...EMPTY_STREAM,
        status: "running",
        mode: deepResearch ? "quality" : "fast",
        startedAt: Date.now(),
        lastEventAt: Date.now(),
      };
      setStream(streamRef.current);
      const task = await sendAskCommand({
        conversationId: conversationId ?? "",
        snapshotId: snapshot.id,
        question: promptText,
        displayText: promptText,
        attachmentIds: [],
        locale: "zh-CN",
        idempotencyKey,
      });
      bindStreamTask(task);
      if (deepResearch) {
        setActiveAgentTaskId(task.id);
        setActiveAgentRevision(task.stateVersion);
      }
      if (conversationId === null) {
        const page = await queryClient.fetchQuery({
          queryKey: ["conversation", "list", documentId, "after-ask", task.id],
          queryFn: () =>
            client.call<{ items: ConversationView[] }>("conversation_list", {
              documentId,
              limit: 1,
            }),
        });
        const latest = page.items[0];
        if (latest) setConversation(latest.id, documentId);
      }
      stickToBottomRef.current = true;
      await queryClient.invalidateQueries({ queryKey: ["messages"] });
      await queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
    } catch (error) {
      setSendError(describeSendError(error, "提问未发出"));
      // 后端可能已写入可见兜底通知;失败后也刷新消息,避免空白。
      await queryClient.invalidateQueries({ queryKey: ["messages"] });
    } finally {
      setSending(false);
    }
  }

  async function handleAsk(): Promise<void> {
    const text = draft.trim();
    if (
      documentId === null ||
      text.length === 0 ||
      sending ||
      agentRunning ||
      streamRef.current.status === "running"
    )
      return;
    const attachmentIds = attachments.map((item) => item.id);
    // 未确认授权时绝不把图片发给模型;纯文本仍可发送(用户可移除图片)。
    if (attachments.length > 0 && vision.data?.ready !== true) {
      setSendError(visionBlockedText(vision.data, vision.isError) ?? "图片发送未就绪。");
      return;
    }
    setSending(true);
    setSendError(null);
    let askStarted = false;
    try {
      const idempotencyKey = client.newIdempotencyKey();
      // 1) 范围建议 → 冻结快照(M07 不可变快照)。
      const suggestion = await client.call<ContextSuggestion>("context_suggest", {
        documentId,
        versionId: versionId ?? undefined,
        pageNumber: effectivePage,
        nearbyPages: pageWindow,
        pageCount: viewport?.pageCount,
      });
      const scope = mergeCitedArtifacts(scopeForSuggestion(suggestion), citedArtifacts);
      const snapshot = await client.call<ContextSnapshotView>("context_freeze", {
        documentId,
        scopeDraft: scope,
        idempotencyKey: `${idempotencyKey}:freeze`,
      });
      // 2) 提问:正文只带 attachment:<id> 引用;question 保持纯文本。
      askStarted = true;
      streamRef.current = {
        ...EMPTY_STREAM,
        status: "running",
        mode: deepResearch ? "quality" : "fast",
        startedAt: Date.now(),
        lastEventAt: Date.now(),
      };
      setStream(streamRef.current);
      const task = await sendAskCommand({
        conversationId: conversationId ?? "",
        snapshotId: snapshot.id,
        question: buildAskQuestion(text, attachments.length),
        displayText: buildUserMessageText(text, attachmentIds),
        attachmentIds,
        locale: "zh-CN",
        idempotencyKey,
      });
      bindStreamTask(task);
      if (deepResearch) {
        setActiveAgentTaskId(task.id);
        setActiveAgentRevision(task.stateVersion);
      }
      // 3) 新会话:取本文档最新会话作为当前会话(创建发生于后端)。
      if (conversationId === null) {
        const page = await queryClient.fetchQuery({
          queryKey: ["conversation", "list", documentId, "after-ask", task.id],
          queryFn: () =>
            client.call<{ items: ConversationView[] }>("conversation_list", {
              documentId,
              limit: 1,
            }),
        });
        const latest = page.items[0];
        if (latest) setConversation(latest.id);
      }
      // 4) 发送成功才清空草稿、附件与引用资料。
      setDraft("");
      setAttachments([]);
      setCitedArtifacts([]);
      setCitationPickerOpen(false);
      setCaptureError(null);
      stickToBottomRef.current = true;
      await queryClient.invalidateQueries({ queryKey: ["messages"] });
      await queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
    } catch (error) {
      setSendError(describeSendError(error, "提问未发出"));
      if (askStarted) {
        // ai_ask 已在后端写入用户消息并绑定附件;前端不可复用这些 ID。
        setAttachments([]);
        await queryClient.invalidateQueries({ queryKey: ["messages"] });
      }
    } finally {
      setSending(false);
    }
  }

  async function cancelAgent(): Promise<void> {
    if (activeAgentTaskId === null || agentCancelBusy) return;
    setAgentCancelBusy(true);
    setSendError(null);
    try {
      const task = await client.call<TaskView>("agent_cancel", {
        taskId: activeAgentTaskId,
        expectedRevision: activeAgentRevision,
      });
      setActiveAgentRevision(task.stateVersion);
      await agentSteps.refetch();
    } catch (error) {
      setSendError(describeSendError(error, "研究取消失败"));
    } finally {
      setAgentCancelBusy(false);
    }
  }

  /** 兜底通知动作:设置/授权/重试;不可执行时保持静默。 */
  function handleNoticeAction(
    actionId: string,
    message: MessagesPage["items"][number],
  ): void {
    if (actionId === "retry") {
      void handleRetry(message);
      return;
    }
    if (actionId === "open_settings") {
      window.dispatchEvent(
        new CustomEvent("knowwise-open-settings", { detail: "models" }),
      );
      return;
    }
    if (actionId === "grant") {
      // 隐私分区已移除:授权入口统一落到模型分区(默认全部允许;
      // 若仍失败,说明提供方/密钥配置有问题,而不是缺授权)。
      window.dispatchEvent(
        new CustomEvent("knowwise-open-settings", { detail: "models" }),
      );
      return;
    }
    if (actionId === "switch_mode") {
      setDeepResearch(true);
    }
  }

  /** 取消当前运行:质量模式走 Agent 取消,快速模式走 ai_cancel。 */
  async function cancelActive(): Promise<void> {
    if (activeAgentTaskId !== null) {
      await cancelAgent();
      return;
    }
    const taskId = streamRef.current.taskId;
    if (!taskId) {
      // 不能静默 return:用户点了取消却毫无反馈,会被误认为按钮坏了。
      setSendError("任务尚未建立,暂时无法取消;请稍后重试,或到任务中心取消。");
      return;
    }
    setSendError(null);
    try {
      // 用后端最新 revision 取消:流事件与任务中心可能已推进版本。
      const latest = await client.call<TaskView>("ai_get_task", { taskId });
      if (latest.status !== "running" && latest.status !== "queued") {
        await queryClient.invalidateQueries({ queryKey: ["messages"] });
        return;
      }
      const canceled = await client.call<TaskView>("ai_cancel", {
        taskId,
        expectedRevision: latest.stateVersion,
      });
      streamRevisionRef.current = canceled.stateVersion;
      streamRef.current = {
        ...streamRef.current,
        notice: "已请求取消;已完成的内容会保留。",
      };
      setStream(streamRef.current);
      await queryClient.invalidateQueries({ queryKey: ["messages"] });
    } catch (error) {
      setSendError(describeSendError(error, "取消失败"));
    }
  }

  async function chooseConfirmationPath(): Promise<void> {
    if (pendingConfirmation === null) return;
    const confirmation = pendingConfirmation.confirmation;
    const format = confirmation?.format;
    const filters =
      format === "docx"
        ? [{ name: "Word 文档", extensions: ["docx"] }]
        : [{ name: "Markdown", extensions: ["md", "markdown"] }];
    setConfirmationError(null);
    try {
      const path = await dialogSavePath({
        title: "选择导出位置",
        defaultPath: confirmation?.suggestedFilename ?? "导出.md",
        filters,
      });
      if (path !== null) setConfirmationPath(path);
    } catch (error) {
      setConfirmationError(describeSendError(error, "选择保存位置失败"));
    }
  }

  async function approveConfirmation(): Promise<void> {
    if (
      pendingConfirmation === null ||
      activeAgentTaskId === null ||
      confirmationBusy
    ) {
      return;
    }
    const requiresPath = pendingConfirmation.confirmation?.requiresPath === true;
    if (requiresPath && confirmationPath === null) {
      setConfirmationError("请先选择保存位置");
      return;
    }
    setConfirmationBusy(true);
    setConfirmationError(null);
    try {
      await client.call<boolean>("agent_confirm_tool", {
        taskId: activeAgentTaskId,
        stepId: pendingConfirmation.stepId,
        decision: "approve",
        path: requiresPath ? confirmationPath ?? undefined : undefined,
      });
      await agentSteps.refetch();
    } catch (error) {
      setConfirmationError(describeSendError(error, "确认失败"));
    } finally {
      setConfirmationBusy(false);
    }
  }

  async function rejectConfirmation(): Promise<void> {
    if (
      pendingConfirmation === null ||
      activeAgentTaskId === null ||
      confirmationBusy
    ) {
      return;
    }
    setConfirmationBusy(true);
    setConfirmationError(null);
    try {
      await client.call<boolean>("agent_confirm_tool", {
        taskId: activeAgentTaskId,
        stepId: pendingConfirmation.stepId,
        decision: "reject",
      });
      await agentSteps.refetch();
    } catch (error) {
      setConfirmationError(describeSendError(error, "拒绝失败"));
    } finally {
      setConfirmationBusy(false);
    }
  }

  async function handleRetry(message: MessagesPage["items"][number]): Promise<void> {
    if (message.taskId === undefined || retryingId !== null) return;
    setRetryingId(message.id);
    setSendError(null);
    try {
      // 重试复用同 Task 新 Attempt:原问题由同对话最近一条用户消息提供
      // (图片引用/旧 data-URI 在这里剥离;重试本轮不重发图片)。
      const items = messages.data?.items ?? [];
      const currentIndex = items.findIndex((item) => item.id === message.id);
      const previousUser = [...items.slice(0, Math.max(0, currentIndex))]
        .reverse()
        .find((item) => item.role === "user");
      const question =
        previousUser?.content.schemaVersion === "user_message.v1"
          ? previousUser.content.text
              .replace(/!\[[^\]]*\]\((?:data:[^)]+|attachment:[^)]+)\)/g, "")
              .trim()
          : "";
      await client.call<TaskView>("ai_retry", {
        taskId: message.taskId,
        question: question.length > 0 ? question : undefined,
        idempotencyKey: client.newIdempotencyKey(),
      });
      stickToBottomRef.current = true;
      await queryClient.invalidateQueries({ queryKey: ["messages"] });
    } catch (error) {
      setSendError(describeSendError(error, "重试未发出"));
    } finally {
      setRetryingId(null);
    }
  }

  function handleInputKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>): void {
    // Enter 发送;Shift+Enter 换行;输入法组合中不发送。
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return;
    event.preventDefault();
    void handleAsk();
  }

  const onCitationClick = (citationId: string) => {
    if (documentId === null) return;
    void resolveCitationAndGoto(client, documentId, citationId).then((resolved) => {
      if (!resolved) showGotoToast("无法定位该引用;原文可能已更新。");
    });
  };

  const conversationItems = conversations.data?.items ?? [];
  const messagesItems = messages.data?.items ?? [];
  const imageSendBlocked = attachments.length > 0 && vision.data?.ready !== true;
  const visionNotice =
    attachments.length > 0 ? visionBlockedText(vision.data, vision.isError) : null;
  const canAllowRemoteVision =
    vision.data?.processingLocation === "remote" &&
    (vision.data.reason === "remote_vision_disabled" ||
      vision.data.reason === "vision_grant_required");

  async function handleCardAction(action: InsightCardAction): Promise<void> {
    if (action.tool === "ask") {
      const prompt = action.args.prompt;
      if (prompt) void handleSuggestedClick(prompt);
      return;
    }
    if (action.tool !== "show_evidence") return;
    let locator: unknown = null;
    const raw = action.args.locator;
    if (raw) {
      try {
        locator = JSON.parse(raw);
      } catch {
        locator = null;
      }
    }
    // 缓存卡片/旧卡片可能缺少 locator:用 blockId 向后端解析真实位置。
    if (!locator && action.args.blockId && documentId) {
      try {
        locator = await client.call("evidence_resolve_block", {
          blockId: action.args.blockId,
        });
      } catch {
        locator = null;
      }
    }
    if (!locator) {
      showGotoToast("无法定位该原文;来源块可能已被新解析代次替换。");
      return;
    }
    window.dispatchEvent(
      new CustomEvent("knowwise-goto", { detail: { documentId, locator } }),
    );
  }

  const renderWelcomeCard = () => (
    <div className="chat-welcome">
      <div className="chat-welcome-head">
        <span className="chat-welcome-mark" aria-hidden="true">
          <Sparkles className="w-4 h-4" />
        </span>
        <div className="chat-welcome-titles">
          <p className="chat-welcome-title">已载入《{docTitle}》</p>
          {suggestedPrompts.data?.domainLabel ? (
            <p className="chat-welcome-domain">{suggestedPrompts.data.domainLabel}</p>
          ) : null}
        </div>
      </div>

      {documentId !== null && versionId !== null ? (
        <InsightCards
          documentId={documentId}
          versionId={versionId}
          page={effectivePage}
          headingPath={headingPath}
          kinds={["question_starters"]}
          onAction={(action) => void handleCardAction(action)}
          compact
        />
      ) : null}

      <p className="chat-welcome-hint">直接提问,或用「工具」发起出题、总结与翻译。</p>
    </div>
  );

  const isPdf = docMetaQuery.data?.format === "pdf";
  const toolGroups: AgentToolGroup[] = [
    {
      label: "问答",
      items: [
        {
          id: "explain",
          label: "解释本页",
          hint: currentScopeLabel(),
          icon: BookOpen,
          onSelect: () =>
            void handleSuggestedClick(
              "请用不超过 400 字解释本文当前范围内的核心内容,并给出引用。",
            ),
        },
        {
          id: "evidence",
          label: "找证据",
          icon: Search,
          onSelect: () =>
            void handleSuggestedClick(
              "请列出本文中支持当前结论的原文证据(最多 5 条),并说明每条证据的局限。",
            ),
        },
        {
          // 只有 PDF 才有页码概念;Markdown/Word 问"相邻页"会让模型去猜，
          // 既答不出又拖长生成。非 PDF 时改为对比相邻章节。
          id: "compare",
          label: isPdf ? "对比相邻页" : "对比相邻章节",
          icon: Columns2,
          onSelect: () =>
            void handleSuggestedClick(
              isPdf
                ? "请对比本文相邻两页的核心差异,逐项列出新增概念、公式或数值示例,并给出引用。"
                : "请对比本文相邻章节的核心差异,逐项列出新增概念、关键结论或示例,并给出简要回答(不超过 300 字),并给出引用。",
            ),
        },
      ],
    },
    {
      label: "学习",
      items: [
        {
          id: "quiz",
          label: "出题",
          hint: "quiz.v2",
          icon: ListChecks,
          onSelect: () => requestTool("quiz"),
        },
        {
          id: "summary",
          label: "总结成文档",
          hint: summaryTemplateLabel(summaryTemplate),
          icon: FileText,
          onSelect: () => requestTool("summary"),
        },
      ],
    },
    {
      label: "语言",
      items: [
        {
          id: "translation_page",
          label: "生成当前页/本节译文产物",
          hint: isPdf ? "当前页 · translation.v1" : "本节 · translation.v1",
          icon: Languages,
          onSelect: () => requestTool("translation_page"),
        },
      ],
    },
  ];

  const conversationCount = conversationItems.length;
  const currentConversation =
    conversationItems.find((conversation) => conversation.id === conversationId) ?? null;

  const renderConversationPanel = (variant: "sidebar" | "drawer") => (
    <div className="agent-conv-panel">
      <div className="agent-conv-panel-head">
        <span className="agent-conv-panel-title">
          <History className="w-3.5 h-3.5" aria-hidden="true" />
          <span>当前文档会话</span>
          <span className="agent-conv-count">{conversationCount}</span>
        </span>
        {variant === "sidebar" ? (
          <button
            type="button"
            className="icon-btn small"
            aria-label="收起会话栏"
            title="收起会话栏"
            onClick={() => setConvSidebarCollapsed(true)}
          >
            <PanelLeftClose className="w-3.5 h-3.5" aria-hidden="true" />
          </button>
        ) : (
          <button
            type="button"
            className="icon-btn small"
            aria-label="关闭会话列表"
            title="关闭"
            autoFocus
            onClick={closeConversationDrawer}
          >
            <X className="w-3.5 h-3.5" aria-hidden="true" />
          </button>
        )}
      </div>

      <button
        type="button"
        className="agent-conv-new"
        onClick={() => void createConversation()}
        disabled={conversationBusy}
      >
        <Plus className="w-3.5 h-3.5" aria-hidden="true" />
        <span>{conversationBusy ? "处理中…" : "新建会话"}</span>
      </button>

      {conversationItems.length === 0 ? (
        <p className="agent-conv-empty">还没有会话;直接提问会自动创建。</p>
      ) : (
        <ul className="agent-conv-list" aria-label="当前文档会话列表">
          {conversationItems.map((conversation) => {
            const active = conversation.id === conversationId;
            if (confirmDeleteId === conversation.id) {
              return (
                <li key={conversation.id} className="agent-conv-row">
                  <div
                    className="qa-conversation-confirm"
                    role="alertdialog"
                    aria-label="删除会话"
                  >
                    <span>删除这个会话及全部消息?</span>
                    <span className="qa-conversation-confirm-actions">
                      <button
                        type="button"
                        className="btn small danger"
                        disabled={conversationBusy}
                        onClick={() => void deleteConversation(conversation)}
                      >
                        删除
                      </button>
                      <button
                        type="button"
                        className="btn small ghost"
                        onClick={() => setConfirmDeleteId(null)}
                      >
                        取消
                      </button>
                    </span>
                  </div>
                </li>
              );
            }
            if (renamingId === conversation.id) {
              return (
                <li key={conversation.id} className="agent-conv-row">
                  <form
                    className="qa-conversation-rename"
                    onSubmit={(event) => {
                      event.preventDefault();
                      void commitRename(conversation);
                    }}
                  >
                    <input
                      autoFocus
                      value={renameDraft}
                      aria-label="会话名称"
                      maxLength={80}
                      onChange={(event) => setRenameDraft(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Escape") {
                          setRenameDraft(conversation.title);
                          setRenamingId(null);
                        }
                      }}
                      onBlur={() => void commitRename(conversation)}
                    />
                    <button type="submit" className="icon-btn small" aria-label="保存名称">
                      <Check className="w-3.5 h-3.5" aria-hidden="true" />
                    </button>
                  </form>
                </li>
              );
            }
            return (
              <li key={conversation.id} className="agent-conv-row">
                <div className="agent-conv-item-wrap">
                  <button
                    type="button"
                    className={`agent-conv-item${active ? " current" : ""}`}
                    aria-current={active ? "true" : undefined}
                    title={conversation.title || "未命名会话"}
                    onClick={() => {
                      setConversation(conversation.id);
                      if (convDrawerOpen) closeConversationDrawer();
                    }}
                  >
                    <span className="agent-conv-item-title">
                      {active ? (
                        <span className="agent-conv-current-dot" aria-hidden="true" />
                      ) : null}
                      <span className="truncate">{conversation.title || "新会话"}</span>
                    </span>
                    <span className="agent-conv-item-meta">
                      {relativeTimeLabel(conversation.updatedAt) ? (
                        <span>{relativeTimeLabel(conversation.updatedAt)}</span>
                      ) : null}
                      <span>{conversation.messageCount} 条消息</span>
                    </span>
                  </button>
                  <span className="agent-conv-item-actions">
                    <button
                      type="button"
                      className="icon-btn small"
                      aria-label="重命名会话"
                      title="重命名"
                      onClick={() => startRename(conversation)}
                    >
                      <Pencil className="w-3 h-3" aria-hidden="true" />
                    </button>
                    <button
                      type="button"
                      className="icon-btn small danger-ghost"
                      aria-label="删除会话"
                      title="删除会话"
                      onClick={() => setConfirmDeleteId(conversation.id)}
                    >
                      <Trash2 className="w-3 h-3" aria-hidden="true" />
                    </button>
                  </span>
                </div>
              </li>
            );
          })}
        </ul>
      )}

      <div className="agent-conv-foot">
        <button
          type="button"
          className="agent-conv-foot-btn"
          onClick={() => setAllConvsOpen(true)}
        >
          <FolderTree className="w-3 h-3" aria-hidden="true" />
          <span>全部会话</span>
        </button>
        <button
          type="button"
          className="agent-conv-foot-btn"
          disabled={conversationId === null}
          onClick={() => setExportConversationId(conversationId)}
        >
          <Download className="w-3 h-3" aria-hidden="true" />
          <span>导出</span>
        </button>
      </div>
    </div>
  );

  return (
    <section className="agent-region" aria-label="Agent 助手" ref={agentRootRef}>
      {documentId === null ? (
        <p className="agent-scope">先打开一个文档,再向它提问。</p>
      ) : (
        <div className="agent-shell">
          {!narrowAgent ? (
            convSidebarCollapsed ? (
              <div className="agent-conv-rail" aria-label="会话栏(已收起)">
                <button
                  type="button"
                  className="icon-btn small"
                  aria-label="展开会话栏"
                  title="展开会话栏"
                  onClick={() => setConvSidebarCollapsed(false)}
                >
                  <PanelLeftOpen className="w-4 h-4" aria-hidden="true" />
                </button>
                <button
                  type="button"
                  className="icon-btn small"
                  aria-label="新建会话"
                  title="新建会话"
                  onClick={() => void createConversation()}
                  disabled={conversationBusy}
                >
                  <Plus className="w-4 h-4" aria-hidden="true" />
                </button>
                <button
                  type="button"
                  className="icon-btn small"
                  aria-label="全部会话"
                  title="全部会话"
                  onClick={() => setAllConvsOpen(true)}
                >
                  <FolderTree className="w-4 h-4" aria-hidden="true" />
                </button>
              </div>
            ) : (
              <aside className="agent-conv-sidebar" aria-label="当前文档会话">
                {renderConversationPanel("sidebar")}
              </aside>
            )
          ) : null}

          <div className="agent-chat-main">
            {narrowAgent ? (
              <div className="agent-chat-head">
                <button
                  ref={convTriggerRef}
                  type="button"
                  className="agent-chat-conv-btn"
                  aria-label="打开当前文档会话列表"
                  aria-expanded={convDrawerOpen}
                  onClick={() => setConvDrawerOpen(true)}
                >
                  <MessageSquare className="w-3.5 h-3.5" aria-hidden="true" />
                  <span>会话</span>
                  <span className="agent-chat-conv-count">{conversationCount}</span>
                </button>
                <span className="agent-chat-current-title">
                  {currentConversation?.title || (conversationId ? "当前会话" : "新会话")}
                </span>
              </div>
            ) : null}

          <div
            className="qa-messages"
            aria-label="消息区"
            aria-live="polite"
            ref={messagesRef}
            onScroll={handleMessagesScroll}
          >
            {messages.isError ? (
              <div className="agent-empty" role="alert">
                <p className="agent-empty-title">消息加载失败</p>
                <p>请检查连接后重试;已保存的会话不会丢失。</p>
                <button
                  type="button"
                  className="agent-empty-action"
                  onClick={() => void messages.refetch()}
                >
                  重试加载
                </button>
              </div>
            ) : messages.data ? (
              <>
              {messagesItems.length > 0 ? (
                messagesItems.map((message) => {
                  const liveStream =
                    message.id === stream.messageId ? stream : undefined;
                  return (
                  <article
                    key={message.id}
                    id={`message-${message.id}`}
                    data-message-id={message.id}
                    data-answer-delivery={
                      nonStreamingMessages.has(message.id) ? "single" : undefined
                    }
                    className={`qa-message ${message.role === "user" ? "user" : "assistant"}${
                      message.answerStatus === "pending" ? " pending" : ""
                    }${highlightedMessageId === message.id ? " reveal-highlight" : ""}`}
                  >
                    <MessageView
                      content={message.content as MessageContent}
                      lifecycle={message.answerStatus}
                      errorCode={message.errorCode}
                      onCitationClick={onCitationClick}
                      attachmentImages={attachmentImages}
                      stream={liveStream}
                      onCancel={
                        message.role === "assistant" &&
                        message.answerStatus === "pending"
                          ? () => void cancelActive()
                          : undefined
                      }
                      reasoning={message.reasoning}
                      showReasoning={showRawReasoning}
                      onNoticeAction={(actionId) => handleNoticeAction(actionId, message)}
                    />
                    {message.role === "assistant" && message.contextNote ? (
                      <p className="qa-context-note" role="note">
                        {message.contextNote}
                      </p>
                    ) : null}
                    {message.role === "assistant" ? (
                      <div className="qa-message-actions">
                        {message.content.schemaVersion === "answer.v1" ? (
                          <CopyAnswerButton text={answerPlainText(message.content.content)} />
                        ) : message.content.schemaVersion === "agent.v1" ? (
                          <CopyAnswerButton
                            text={agentAnswerPlainText(message.content.content)}
                          />
                        ) : null}
                        {message.content.schemaVersion === "answer.v1" ||
                        message.content.schemaVersion === "agent.v1" ? (
                          <EvidenceLensPanel
                            messageId={message.id}
                            onCitationClick={onCitationClick}
                          />
                        ) : null}
                        {message.taskId !== undefined &&
                        canRetryMessage(
                          message.taskStatus,
                          (tasks.data?.items ?? []).find(
                            (task) => task.id === message.taskId,
                          )?.taskType,
                        ) ? (
                          <button
                            type="button"
                            className="qa-retry-btn"
                            onClick={() => void handleRetry(message)}
                            disabled={retryingId === message.id}
                          >
                            <RefreshCw className="w-3.5 h-3.5" aria-hidden="true" />
                            <span>
                              {retryingId === message.id ? "重试中…" : "重试回答"}
                            </span>
                          </button>
                        ) : null}
                      </div>
                    ) : null}
                  </article>
                  );
                })
              ) : (
                renderWelcomeCard()
              )}
              </>
            ) : conversationId === null ? (
              renderWelcomeCard()
            ) : (
              <p className="agent-hint">消息加载中…</p>
            )}
          </div>

          {activeAgentTaskId !== null ? (
            <div className="agent-running sr-only" role="status" aria-live="polite">
              <div className="agent-running-head">
                <span>深度研究执行中…</span>
                <button
                  type="button"
                  className="btn small"
                  onClick={() => void cancelAgent()}
                  disabled={agentCancelBusy}
                >
                  {agentCancelBusy ? "取消中…" : "取消"}
                </button>
              </div>
              {agentSteps.data && agentSteps.data.steps.length > 0 ? (
                <ol className="agent-running-steps">
                  {agentSteps.data.steps.map((step) => (
                    <li
                      key={`${step.stepNo}-${step.kind}`}
                      className={`status-${step.status}`}
                    >
                      <span className="agent-running-step-name">
                        {step.kind === "tool" ? step.toolName ?? "工具" : "模型"}
                      </span>
                      <span>{AGENT_STEP_STATUS_TEXT[step.status] ?? step.status}</span>
                      {step.resultSummary ? <span>{step.resultSummary}</span> : null}
                      {step.errorCode ? (
                        <span className="agent-running-step-error">{step.errorCode}</span>
                      ) : null}
                    </li>
                  ))}
                </ol>
              ) : (
                <p className="agent-hint">正在准备第一步…</p>
              )}
            </div>
          ) : null}



          {pendingConfirmation !== null && activeAgentTaskId !== null ? (
            <div className="agent-confirm-backdrop">
              <div
                className="agent-confirm"
                role="dialog"
                aria-modal="true"
                aria-labelledby="agent-confirm-title"
              >
                <h3 id="agent-confirm-title">需要你的确认</h3>
                <p className="agent-confirm-tool">
                  {CONFIRMATION_TOOL_LABELS[pendingConfirmation.confirmation?.tool ?? ""] ??
                    pendingConfirmation.confirmation?.tool ??
                    pendingConfirmation.toolName ??
                    "写操作"}
                </p>
                <p className="agent-confirm-summary">
                  {pendingConfirmation.confirmation?.summary ?? "该操作需要你确认后才会执行。"}
                </p>
                {pendingConfirmation.confirmation?.requiresPath ? (
                  <div className="agent-confirm-path">
                    <button
                      type="button"
                      className="btn small"
                      onClick={() => void chooseConfirmationPath()}
                    >
                      选择保存位置
                    </button>
                    <span>
                      {confirmationPath !== null
                        ? fileBasename(confirmationPath)
                        : "尚未选择"}
                    </span>
                  </div>
                ) : null}
                {confirmationError !== null ? (
                  <p className="agent-error" role="alert">
                    {confirmationError}
                  </p>
                ) : null}
                <p className="agent-hint">
                  未确认前不会写入任何产物、文件或导出记录;超时 60 秒视为拒绝。
                </p>
                <div className="agent-confirm-actions">
                  <button
                    type="button"
                    className="btn small"
                    onClick={() => void rejectConfirmation()}
                    disabled={confirmationBusy}
                  >
                    拒绝
                  </button>
                  <button
                    type="button"
                    className="btn primary"
                    onClick={() => void approveConfirmation()}
                    disabled={
                      confirmationBusy ||
                      (pendingConfirmation.confirmation?.requiresPath === true &&
                        confirmationPath === null)
                    }
                  >
                    {confirmationBusy ? "处理中…" : "批准"}
                  </button>
                  <button
                    type="button"
                    className="btn small"
                    onClick={() => void cancelAgent()}
                    disabled={confirmationBusy || agentCancelBusy}
                  >
                    取消研究
                  </button>
                </div>
              </div>
            </div>
          ) : null}

          {!atBottom ? (
            <button
              type="button"
              className="qa-jump-latest"
              onClick={() => {
                const el = messagesRef.current;
                if (el) el.scrollTop = el.scrollHeight;
                stickToBottomRef.current = true;
                setAtBottom(true);
              }}
            >
              回到最新 ↓
            </button>
          ) : null}
          <div className="agent-composer">
            {toolConfirm ? (
              <div className="agent-tool-confirm" role="alertdialog" aria-label="确认生成长任务">
                <p>
                  {toolConfirm.label} · 范围 {toolConfirm.range}
                  {toolConfirm.estimate ? ` · ${toolConfirm.estimate}` : ""}
                </p>
                {toolConfirm.kind === "quiz" ? (
                  <QuizCountControls
                    counts={quizCounts}
                    onChange={setQuizCounts}
                    disabled={toolBusy !== null}
                  />
                ) : null}
                {toolConfirm.kind === "summary" ? (
                  <label className="agent-tool-template">
                    模板
                    <select
                      value={summaryTemplate}
                      disabled={toolBusy !== null}
                      onChange={(event) => setSummaryTemplate(event.target.value)}
                      aria-label="总结模板"
                    >
                      {Object.entries(SUMMARY_TEMPLATE_LABELS).map(([value, label]) => (
                        <option key={value} value={value}>
                          {label}
                        </option>
                      ))}
                    </select>
                  </label>
                ) : null}
                <div className="agent-tool-confirm-actions">
                  <button type="button" className="btn small" onClick={() => setToolConfirm(null)}>
                    取消
                  </button>
                  <button
                    type="button"
                    className="btn primary small"
                    onClick={() => void runTool(toolConfirm.kind)}
                    disabled={
                      toolConfirm.kind === "quiz" && quizConfigIssue(quizCounts) !== null
                    }
                    title={
                      toolConfirm.kind === "quiz"
                        ? (quizConfigIssue(quizCounts) ?? undefined)
                        : undefined
                    }
                  >
                    确认生成
                  </button>
                </div>
              </div>
            ) : null}
            {toolBusy ? (
              <p className="agent-tool-status" role="status">
                <span className="agent-tool-status-dot" aria-hidden="true" />
                {toolBusy === "quiz"
                  ? "正在生成练习题…"
                  : toolBusy === "translation_page" || toolBusy === "translation_full"
                    ? "正在翻译…(失败时保留原文)"
                    : "正在生成文档产物…"}
                {generationTask ? (
                  <>
                    <span>{generationStatus.data?.task.cancelRequested ? "正在取消…" : generationStatus.data?.task.phase === "validating" ? "正在校验" : generationStatus.data?.task.phase === "committing" ? "正在保存" : "后台执行，可继续阅读"}</span>
                    <button type="button" className="btn tiny ghost" aria-label="取消生成任务"
                      disabled={generationCancelBusy || generationStatus.data?.task.cancelRequested}
                      onClick={() => void cancelGeneration()}>取消生成</button>
                  </>
                ) : null}
              </p>
            ) : null}
            {generationTask && generationStatus.isError ? (
              <p className="agent-tool-status error" role="alert">
                暂时无法读取任务状态，正在重试查询；可在任务中心查看或取消，尚不能判断生成失败。
              </p>
            ) : null}
            {toolNotice ? (
              <p className="agent-tool-status" role="status">
                {toolNotice}
              </p>
            ) : null}
            {toolError ? (
              <p className="agent-tool-status error" role="alert">
                <span>{toolError}</span>
                {lastToolKind !== null ? (
                  <button
                    type="button"
                    className="agent-tool-status-retry"
                    onClick={() => {
                      setToolError(null);
                      requestTool(lastToolKind);
                    }}
                  >
                    重试
                  </button>
                ) : null}
              </p>
            ) : null}
            {researchExpanded && effectivePage !== undefined ? (
              <div className="agent-scope-summary" role="status">
                <span className="agent-scope-diff">
                  深度研究将扩展到 p.{Math.max(1, effectivePage - effectiveScopeWindow)}–
                  {viewport?.pageCount !== undefined
                    ? Math.min(viewport.pageCount, effectivePage + effectiveScopeWindow)
                    : effectivePage + effectiveScopeWindow}
                  <button
                    type="button"
                    className="btn small"
                    onClick={() => setResearchKeepBase(true)}
                  >
                    改回原范围
                  </button>
                </span>
              </div>
            ) : null}
            {customRange !== null ? (
              <div className="agent-scope-custom" role="group" aria-label="自定义页码范围">
                <label>
                  起始页
                  <input
                    type="number"
                    min={1}
                    max={viewport?.pageCount}
                    value={customStart}
                    onChange={(event) => setCustomStart(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") applyCustomRange();
                    }}
                  />
                </label>
                <label>
                  结束页
                  <input
                    type="number"
                    min={1}
                    max={viewport?.pageCount}
                    value={customEnd}
                    onChange={(event) => setCustomEnd(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") applyCustomRange();
                    }}
                  />
                </label>
                <button type="button" className="btn small" onClick={applyCustomRange}>
                  应用
                </button>
                <button
                  type="button"
                  className="btn small"
                  onClick={() => {
                    setCustomRange(null);
                    setScopeError(null);
                  }}
                >
                  取消
                </button>
                {scopeError ? (
                  <span className="agent-scope-error" role="alert">
                    {scopeError}
                  </span>
                ) : null}
              </div>
            ) : null}
            {attachments.length > 0 ? (
              <div className="agent-attachments" aria-label="图片附件">
                {attachments.map((attachment) => (
                  <div key={attachment.id} className="agent-attachment">
                    <img src={attachment.dataUrl} alt="图片附件" />
                    <button
                      type="button"
                      className="agent-attachment-remove"
                      aria-label="删除图片"
                      title="删除图片"
                      onClick={() => removeAttachment(attachment.id)}
                    >
                      <X className="w-3 h-3" aria-hidden="true" />
                    </button>
                  </div>
                ))}
                <span className="agent-attachment-count">
                  {attachments.length}/{MAX_SCREENSHOTS}
                </span>
              </div>
            ) : null}

            {recentImageIds.length > 0 && !sending ? (
              <div className="agent-image-memory" aria-label="最近图片">
                <span className="agent-image-memory-label">最近图片(可选重发)</span>
                {recentImageIds.map((id) => (
                  <button
                    key={id}
                    type="button"
                    className="agent-image-memory-item"
                    aria-label="重发这张历史图片"
                    title="重发这张历史图片"
                    disabled={attaching || attachments.length >= MAX_SCREENSHOTS}
                    onClick={() => void reuseImage(id)}
                  >
                    {attachmentImages[id] ? (
                      <img src={attachmentImages[id]} alt="" />
                    ) : (
                      <span aria-hidden="true">…</span>
                    )}
                  </button>
                ))}
              </div>
            ) : null}

            {citationPickerOpen ? (
              <CitationPicker
                documentId={documentId}
                cited={citedArtifacts}
                onClose={() => setCitationPickerOpen(false)}
                onToggle={(item) => {
                  setCitedArtifacts((prev) => {
                    const exists = prev.some((entry) => entry.versionId === item.versionId);
                    return exists
                      ? prev.filter((entry) => entry.versionId !== item.versionId)
                      : [...prev, item];
                  });
                }}
              />
            ) : null}
            <CitedArtifactChips
              cited={citedArtifacts}
              onRemove={(versionId) =>
                setCitedArtifacts((prev) => prev.filter((item) => item.versionId !== versionId))
              }
            />
            {citedArtifacts.length > 0 ? (
              <p className="agent-cited-note" role="status">
                本轮仅以所选 {citedArtifacts.length} 份资料为来源;当前页/范围选择不参与检索。
              </p>
            ) : null}

            <div className="agent-input-container">
              <label className="agent-input-label" htmlFor="agent-input">
                <span className="agent-input-title">向文档提问</span>
                <textarea
                  id="agent-input"
                  ref={setTextareaRef}
                  value={draft}
                  rows={3}
                  placeholder="把问题写清楚一点。AI 会结合当前范围与最近几轮对话回答…"
                  onChange={(event) => setDraft(event.target.value)}
                  onKeyDown={handleInputKeyDown}
                />
              </label>
              <div className="agent-input-footer">
                <div className="agent-input-tools">
                  <div className="agent-scope-inline" role="group" aria-label="提问范围">
                    {viewport?.pageNumber !== undefined ? (
                      <>
                        <select
                          className="agent-scope-select"
                          aria-label="页码范围"
                          value={
                            fullScope
                              ? "full"
                              : customRange !== null
                                ? "custom"
                                : sectionScope
                                  ? "section"
                                  : String(pageWindow)
                          }
                          onChange={(event) => {
                            const value = event.target.value;
                            setScopeError(null);
                            if (value === "full") {
                              setSectionScope(false);
                              setCustomRange(null);
                              setFullScope(true);
                              return;
                            }
                            setFullScope(false);
                            if (value === "custom") {
                              setSectionScope(false);
                              setCustomRange({ start: effectivePage ?? 1, end: effectivePage ?? 1 });
                              setCustomStart(String(effectivePage ?? 1));
                              setCustomEnd(String(effectivePage ?? 1));
                              return;
                            }
                            if (value === "section") {
                              setCustomRange(null);
                              setSectionScope(true);
                              return;
                            }
                            setCustomRange(null);
                            setSectionScope(false);
                            setPageWindow(Number(value));
                          }}
                        >
                          <option value="0">当前页 p.{effectivePage}</option>
                          <option value="1">附近 p.{effectivePage} ±1</option>
                          <option value="2">附近 p.{effectivePage} ±2</option>
                          <option value="custom">自定义页码…</option>
                          <option value="section">本节</option>
                          <option value="full">全文</option>
                        </select>
                        <button
                          type="button"
                          className={`btn small${pageLock !== null ? " primary" : ""}`}
                          onClick={() =>
                            setPageLock(pageLock !== null ? null : viewport.pageNumber ?? null)
                          }
                          title={pageLock !== null ? "解锁:下次提问跟随当前页" : "锁定提问页码"}
                        >
                          {pageLock !== null ? `已锁定 p.${pageLock}` : "锁定页码"}
                        </button>
                      </>
                    ) : null}
                    {viewport?.pageNumber === undefined ? (
                      <button
                        type="button"
                        className={`btn small${fullScope ? " primary" : ""}`}
                        aria-pressed={fullScope}
                        title="全文范围:整篇文档参与检索(不虚构页码)"
                        onClick={() => {
                          setSectionScope(false);
                          setCustomRange(null);
                          setFullScope((value) => !value);
                        }}
                      >
                        全文
                      </button>
                    ) : null}
                    <button
                      type="button"
                      className={`btn small${sectionScope ? " primary" : ""}`}
                      disabled={headingPath.length === 0}
                      title={
                        headingPath.length === 0
                          ? "当前视口尚未识别到标题路径"
                          : `本节:${headingPath.join(" / ")}`
                      }
                      onClick={() => {
                        setFullScope(false);
                        setCustomRange(null);
                        setSectionScope((prev) => !prev);
                      }}
                    >
                      本节
                    </button>
                  </div>
                  <AgentToolMenu
                    groups={toolGroups}
                    disabled={sending || agentRunning || toolBusy !== null}
                  />
                  <button
                    type="button"
                    className="agent-shot-btn agent-attach-btn"
                    aria-label="选择本地图片"
                    title="选择本地图片(PNG / JPEG / WebP)"
                    onClick={() => void handlePickImages()}
                    disabled={
                      attaching || capturing || sending || attachments.length >= MAX_SCREENSHOTS
                    }
                  >
                    <ImagePlus className="w-4 h-4" aria-hidden="true" />
                    <span>{attaching ? "处理中…" : "图片"}</span>
                  </button>
                  {clipSupported ? (
                    <button
                      type="button"
                      className="agent-shot-btn agent-attach-btn"
                      aria-label="截图"
                      title="框选屏幕区域截图"
                      onClick={() => void handleScreenshot()}
                      disabled={
                        capturing ||
                        attaching ||
                        sending ||
                        attachments.length >= MAX_SCREENSHOTS
                      }
                    >
                      <Camera className="w-4 h-4" aria-hidden="true" />
                      <span>{capturing ? "框选中…" : "截图"}</span>
                    </button>
                  ) : null}
                  <button
                    type="button"
                    className="agent-shot-btn agent-attach-btn"
                    aria-label="引用资料"
                    aria-expanded={citationPickerOpen}
                    title="引用已有资料作为本轮来源"
                    onClick={() => setCitationPickerOpen((open) => !open)}
                    disabled={sending || agentRunning}
                  >
                    <BookMarked className="w-4 h-4" aria-hidden="true" />
                    <span>
                      引用资料{citedArtifacts.length > 0 ? `(${citedArtifacts.length})` : ""}
                    </span>
                  </button>
                  <label className="agent-deep-research sr-only" title="质量模式 / 深度研究">
                    <input
                      type="checkbox"
                      checked={deepResearch}
                      onChange={(e) => { setResearchKeepBase(false); setDeepResearch(e.target.checked); }}
                    />
                    <span>深度研究</span>
                  </label>
                  <div className="agent-mode-toggle inline-flex items-center rounded-full p-0.5 bg-slate-100 dark:bg-slate-800 border border-slate-200 dark:border-slate-700 text-xs">
                    <button
                      type="button"
                      className={`px-2.5 py-0.5 rounded-full transition-all flex items-center gap-1 text-[11px] ${!deepResearch ? "bg-purple-600 text-white font-medium shadow-xs" : "text-slate-500 hover:text-slate-800 dark:text-slate-400"}`}
                      aria-label="快速模式"
                      aria-pressed={!deepResearch}
                      onClick={() => setDeepResearch(false)}
                      title="快速模式: 秒速响应, 流畅直接"
                    >
                      <span aria-hidden="true">⚡</span>
                      <span>快速</span>
                    </button>
                    <button
                      type="button"
                      className={`px-2.5 py-0.5 rounded-full transition-all flex items-center gap-1 text-[11px] ${deepResearch ? "bg-purple-600 text-white font-medium shadow-xs" : "text-slate-500 hover:text-slate-800 dark:text-slate-400"}`}
                      aria-label="质量模式"
                      aria-pressed={deepResearch}
                      onClick={() => { setResearchKeepBase(false); setDeepResearch(true); }}
                      title="质量模式: 深度研究 Agent, 展示完整思考推理过程"
                    >
                      <span aria-hidden="true">🧠</span>
                      <span>质量</span>
                    </button>
                  </div>
                  <span className="agent-input-hint">
                    Enter 发送 · Shift+Enter 换行
                  </span>
                </div>
                <div className="agent-input-submit">
                  {draft.trim().length > 0 ? (
                    <span className="agent-input-count" aria-hidden="true">
                      {draft.trim().length} 字
                    </span>
                  ) : null}
                  <button
                    type="button"
                    className="agent-send-btn btn primary"
                    onClick={() => void handleAsk()}
                    disabled={
                      sending ||
                      agentRunning ||
                      stream.status === "running" ||
                      documentId === null ||
                      draft.trim().length === 0 ||
                      imageSendBlocked
                    }
                    title={imageSendBlocked ? "图片发送未授权" : undefined}
                  >
                    <Send className="w-3.5 h-3.5" aria-hidden="true" />
                    <span>{sending ? "发送中…" : "发送"}</span>
                  </button>
                </div>
              </div>
            </div>

            {deepResearch &&
            !qualityToolStepSeen &&
            qualityCapability.data !== undefined &&
            !qualityCapability.data.toolCalling ? (
              <p className="agent-mode-note" role="status">
                当前质量模型未声明工具调用能力:质量模式仅单步回答,不显示多步工具轨迹。
              </p>
            ) : null}

            {visionNotice ? (
              <div className="agent-vision-notice" role="alert">
                <span>{visionNotice}</span>
                {canAllowRemoteVision && !allowingVision ? (
                  <button
                    type="button"
                    className="btn small"
                    onClick={() => void allowRemoteVision()}
                  >
                    允许把图片发送给模型
                  </button>
                ) : null}
                {allowingVision ? <span className="agent-hint">授权中…</span> : null}
              </div>
            ) : null}
            {captureError ? (
              <p className="agent-error" role="alert">
                {captureError}
              </p>
            ) : null}
            {sendError ? (
              <p className="agent-error" role="alert">
                {sendError}
              </p>
            ) : null}
            {attachments.length > 0 && vision.data?.ready ? (
              <p className="agent-hint">
                图片会随问题发送给当前模型;请确认所选模型支持图片输入,最多 3 张。
              </p>
            ) : null}
            {attachments.length === 0 && draft.trim().length > 0 ? (
              <p className="agent-hint">
                可截图或选择本地图片一起提问;图片会先压缩,正文只保存引用。
              </p>
            ) : null}
          </div>
          </div>
        </div>
      )}

      {narrowAgent && convDrawerOpen && documentId !== null ? (
        <div className="agent-conv-drawer-backdrop" onClick={closeConversationDrawer}>
          <aside
            className="agent-conv-drawer"
            role="dialog"
            aria-modal="true"
            aria-label="当前文档会话"
            tabIndex={-1}
            onClick={(event) => event.stopPropagation()}
            onKeyDown={(event) => handleTrapKey(event.currentTarget, event)}
          >
            {renderConversationPanel("drawer")}
          </aside>
        </div>
      ) : null}

      {gotoToast !== null ? (
        <div className="agent-toast" role="status">
          {gotoToast}
        </div>
      ) : null}

      {exportConversationId !== null ? (
        <ConversationExportDialog
          conversationId={exportConversationId}
          onClose={() => setExportConversationId(null)}
        />
      ) : null}

      <AllConversationsDialog
        open={allConvsOpen}
        onClose={() => setAllConvsOpen(false)}
      />
    </section>
  );
}
