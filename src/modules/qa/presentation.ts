// M09 问答展示逻辑(《M09》§2/§6/§9、《02》§6)。
// 任务视图是唯一真相:流式 ai.delta 只作进度提示,不渲染为答案;
// 答案状态、来源分层与未核验标识按 AnswerContent 派生,不自行猜测。
import type { AnswerContent } from "../../shared/generated/AnswerContent";
import type { AnswerSegment } from "../../shared/generated/AnswerSegment";
import type { AppError } from "../../shared/generated/AppError";
import type { TaskView } from "../../shared/generated/TaskView";

/** 段类型标签:非 document_fact 一律显式标识,不得冒充原文事实(§6)。 */
export const SEGMENT_KIND_LABELS: Record<AnswerSegment["kind"], string> = {
  document_fact: "原文事实",
  inference: "推断",
  example: "示例",
  user_view: "用户观点",
  general_note: "一般说明",
};

/** 运行阶段文案(准备中/检索中/生成中/校验中,AI-06 状态可见)。 */
export function phaseLabel(view: TaskView): string {
  if (view.status === "queued") return "排队中";
  if (view.cancelRequested) return "正在取消";
  switch (view.phase) {
    case "waiting":
      return "准备中";
    case "retrieving":
      return "检索中";
    case "generating":
      return "生成中";
    case "validating":
      return "校验引用中";
    case "committing":
      return "保存中";
    default:
      return "处理中";
  }
}

/** 终态结果文案:回答状态由 Task.outcome 表达,不另建状态机(《01》§4.5)。 */
export function outcomeLabel(view: TaskView): string {
  switch (view.outcome) {
    case "answered":
      return "已回答";
    case "insufficient_evidence":
      return "证据不足";
    case "conflict":
      return "证据冲突";
    case "out_of_scope":
      return "超出所选范围";
    default:
      return "未完成";
  }
}

/** 顶部状态:运行中显示阶段,终态显示结果(取消态不冒充完成)。 */
export function qaStatusText(view: TaskView): string {
  switch (view.status) {
    case "running":
    case "queued":
      return phaseLabel(view);
    case "succeeded":
      return outcomeLabel(view);
    case "canceled":
      return "已取消";
    case "interrupted":
      return "已中断";
    default:
      return "失败";
  }
}

/**
 * 只有终态成功任务才允许渲染 AnswerContent;流式片段(ai.delta)
 * 与 running 中的任何内容都不是可引用的答案正文(《02》§4)。
 */
export function isRenderableAnswer(view: TaskView): boolean {
  return view.status === "succeeded";
}

/** 仅失败/取消/中断可显式重试;运行中与已完成都不显示重试(AI-06)。 */
export function canRetry(view: TaskView): boolean {
  return view.status === "failed" || view.status === "canceled" || view.status === "interrupted";
}

/** 仅 running/queued 可取消。 */
export function canCancel(view: TaskView): boolean {
  return view.status === "running" || view.status === "queued";
}

/** 失败后的可执行下一步:按稳定 userAction 派生,不解析自由文本(A8)。 */
export function nextStepHint(error: AppError | undefined): string {
  switch (error?.userAction) {
    case "wait_for_index":
      return "语义索引未完成:等待索引完成,或缩小提问范围后重试。";
    case "choose_scope":
      return "所选范围超出上下文预算:缩小范围后重新提问。";
    case "grant_permission":
      return "本次提问需要模型授权:授权远程处理,或改用本地模型。";
    case "open_settings":
      return "模型不可用或额度不足:打开设置检查模型配置。";
    case "retry":
      return "可以重试本次提问;问题与所选范围已保留。";
    default:
      return error ? "本次提问未完成,可重试。" : "";
  }
}

export type SegmentPresentation = {
  kindLabel: string;
  /** 原文事实但缺结构有效引用:显示未核验,不显示为已证实(§7)。 */
  unverified: boolean;
  /** 引用编号(AnswerContent.citations 中的 id)。 */
  citationIds: string[];
};

export function segmentPresentation(seg: AnswerSegment): SegmentPresentation {
  const hasValidCitation =
    seg.verificationStatus === "structurally_valid" && seg.citationIds.length > 0;
  return {
    kindLabel: SEGMENT_KIND_LABELS[seg.kind],
    unverified: seg.kind === "document_fact" ? !hasValidCitation : false,
    citationIds: seg.citationIds,
  };
}

/** 非事实段的说明:推断/示例/用户观点必须可见且与原文事实区分。 */
export function isNonFactSegment(seg: AnswerSegment): boolean {
  return seg.kind !== "document_fact";
}

/**
 * 引用覆盖率提示:"范围为全文,本次检索使用若干原文块" ——
 * 不暗示模型逐字读完整全文(《00》§3.3)。
 */
export function coverageDisclosure(
  view: TaskView,
  answer: AnswerContent | undefined,
  includedBlocks: number | undefined,
): string {
  if (!isRenderableAnswer(view) || !answer) return "";
  const cited = new Set<string>();
  for (const c of answer.citations) {
    for (const id of c.claimSegmentIds) cited.add(id);
  }
  const citationCount = answer.citations.length;
  const basis =
    includedBlocks !== undefined
      ? `本次回答基于检索到的 ${includedBlocks} 个原文块`
      : "本次回答基于检索到的原文块";
  const scopeNote =
    view.outcome === "insufficient_evidence"
      ? "在所选范围内未找到支持证据,不代表全文不存在该信息。"
      : `回答引用了 ${citationCount} 处原文。`;
  return `${basis};${scopeNote}`;
}
