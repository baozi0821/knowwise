// M09 拥有:消息内容渲染(user / 占位 / answer.v1)。answer.v1 按
// AnswerStatus 给出结论横幅,分段按 SegmentKind 标注(推断/示例/用户
// 观点必须可见,不冒充文档事实);引用可点击(由容器解析并回跳)。
// 用户消息用统一 Markdown 渲染(新消息为 attachment:<id> 引用,历史消息可能是 data-URI);
// 回答提供「复制回答」,复制失败如实提示,不静默。
import { memo, useEffect, useMemo, useState } from "react";
import { Check, ChevronDown, Copy, Quote } from "lucide-react";
import type { AgentAnswerV1 } from "../../shared/generated/AgentAnswerV1";
import type { AgentStepView } from "../../shared/generated/AgentStepView";
import type { AnswerContent } from "../../shared/generated/AnswerContent";
import type { AnswerLifecycle } from "../../shared/generated/AnswerLifecycle";
import type { MessageContent } from "../../shared/generated/MessageContent";
import type { SegmentKind } from "../../shared/generated/SegmentKind";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import type { CommandClient } from "../../app/api/client";
import { clipboardWriteText, type WriteClipboardTextFn } from "../../app/ipc/transport";
import { Markdown } from "../reader/Markdown";
import { jumpToLocator } from "../notes/jumpToLocator";
import { stageLabel, type StreamState } from "./stream";
import { useSmoothText } from "./smoothText";
import type { ReactNode } from "react";
import "./qa.css";

const STATUS_BANNERS: Record<AnswerContent["status"], string> = {
  answered: "已回答",
  insufficient_evidence: "当前范围证据不足:所选范围内没有足够依据,以下内容仅为可核对的线索。",
  conflict: "文档内存在相互冲突的陈述,不做裁决:",
  out_of_scope: "该问题超出当前快照范围。",
};

const SEGMENT_BADGES: Partial<Record<SegmentKind, string>> = {
  inference: "推断",
  example: "示例",
  user_view: "用户观点",
  general_note: "说明",
};

/** 只有 document_fact 且核对未通过时才逐段标注;其余段类型不出现“未校验”。 */
function needsCitationWarning(kind: SegmentKind, verificationStatus: string): boolean {
  return kind === "document_fact" && verificationStatus === "unverified";
}

/** 后端说明可能以“未校验:”开头;在“引用说明”内去掉该前缀,避免被读成整条回答状态。 */
function formatCitationNote(note: string): string {
  return note.replace(/^\s*未校验\s*[:：]\s*/, "").trim() || note;
}

/** 答案纯文本(复制回答用):结论 + 分段 + 冲突/引用/引用说明。 */
export function answerPlainText(answer: AnswerContent): string {
  const lines: string[] = [STATUS_BANNERS[answer.status]];
  for (const segment of answer.segments) {
    const badge = SEGMENT_BADGES[segment.kind];
    lines.push(badge ? `【${badge}】${segment.text}` : segment.text);
  }
  for (const conflict of answer.conflicts) {
    lines.push(`冲突:${conflict.description}`);
  }
  if (answer.citations.length > 0) {
    lines.push("引用:");
    for (const citation of answer.citations) {
      lines.push(`- 「${citation.quotedText}」`);
    }
  }
  if (answer.unverifiedNotes.length > 0) {
    lines.push(`引用说明:${answer.unverifiedNotes.map(formatCitationNote).join(";")}`);
  }
  return lines.join("\n");
}

/** 复制回答:成功/失败都反馈,失败不假装成功(《M13》§8)。 */
export function CopyAnswerButton({
  text,
  writeClipboard = clipboardWriteText,
}: {
  text: string;
  writeClipboard?: WriteClipboardTextFn;
}): ReactNode {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  async function copy(): Promise<void> {
    try {
      await writeClipboard(text);
      setState("copied");
    } catch {
      setState("failed");
    }
  }
  return (
    <span className="qa-answer-actions">
      <button
        type="button"
        className="qa-copy-btn"
        title="复制回答到剪贴板"
        onClick={() => void copy()}
      >
        {state === "copied" ? (
          <Check className="w-3.5 h-3.5" aria-hidden="true" />
        ) : (
          <Copy className="w-3.5 h-3.5" aria-hidden="true" />
        )}
        <span>{state === "copied" ? "已复制" : "复制回答"}</span>
      </button>
      {state === "failed" ? (
        <span className="qa-copy-feedback" role="alert">
          复制失败:剪贴板不可用
        </span>
      ) : null}
    </span>
  );
}

/**
 * 引用点击:解析稳定引用 → 按格式派发定位事件 → 阅读器滚动。
 *
 * 关键点(曾经只发 `knowwise-goto`,只有 PDF 视图监听,Markdown/Word
 * 点引用完全没反应):
 * - PDF 用 `knowwise-goto`(页码);
 * - Markdown 用 `knowwise-goto-text`(文本偏移);
 * - DOCX 引用来自解析块序,用 `knowwise-goto-docx` 单独派发;
 *   笔记/书签仍走字符偏移,两套定位不混用;
 * - 解析失败或位置失效返回 false,由调用方给可见提示。
 */
export async function resolveCitationAndGoto(
  client: CommandClient,
  documentId: string,
  citationId: string,
): Promise<boolean> {
  try {
    const resolution = await client.call<{ locator?: SourceLocator }>(
      "evidence_resolve_citation",
      { citationId },
    );
    const locator = resolution?.locator;
    if (locator === undefined || locator === null) return false;
    if (locator.kind === "docx") {
      window.dispatchEvent(
        new CustomEvent("knowwise-goto-docx", {
          detail: {
            documentId,
            start: locator.bodyBlockStart,
            end: locator.bodyBlockEnd,
          },
        }),
      );
      return true;
    }
    jumpToLocator(documentId, locator);
    return true;
  } catch {
    return false;
  }
}

function parseSegmentText(text: string): { thinking: string | null; content: string } {
  const thinkMatch = /<think>([\s\S]*?)<\/think>/i.exec(text);
  if (thinkMatch && thinkMatch[1] !== undefined) {
    return {
      thinking: thinkMatch[1].trim(),
      content: text.replace(/<think>[\s\S]*?<\/think>/i, "").trim(),
    };
  }
  return { thinking: null, content: text };
}

function PendingStreamView({
  lifecycle,
  errorCode,
  stream,
  onCancel,
  showReasoning = true,
}: {
  lifecycle: AnswerLifecycle;
  errorCode?: string;
  stream?: StreamState;
  onCancel?: () => void;
  showReasoning?: boolean;
}): ReactNode {
  // 只用于刷新真实已用时间;文案全部来自真实事件,不按秒轮播。
  const [, setTick] = useState(0);
  // 平滑呈现:逐字放出已到达的真实文本(不伪造内容),终态后快速补完。
  // 暂停只停止呈现,不取消生成;恢复后继续补完已到达内容。
  const [paused, setPaused] = useState(false);
  const smoothAnswer = useSmoothText(
    stream?.answer ?? "",
    stream?.status !== "running",
    paused,
  );

  useEffect(() => {
    if (lifecycle !== "pending") return;
    const interval = setInterval(() => setTick((value) => value + 1), 1000);
    return () => clearInterval(interval);
  }, [lifecycle]);

  if (lifecycle !== "pending") {
    return (
      <p className="qa-stream-idle">未产出答案({errorCode ?? lifecycle})</p>
    );
  }

  const running = stream?.status === "running" || stream?.status === "idle";
  const stage = stageLabel(stream?.stage);
  const elapsedSeconds =
    stream?.startedAt && stream.startedAt > 0
      ? Math.max(0, Math.round((Date.now() - stream.startedAt) / 1000))
      : null;

  return (
    <div className="qa-stream-card">
      <div className="qa-stream-head">
        <span className="qa-stream-controls">
          {elapsedSeconds !== null ? (
            <span className="qa-stream-elapsed">已用 {elapsedSeconds}s</span>
          ) : null}
          {running ? (
            <button
              type="button"
              className="qa-stream-pause"
              aria-pressed={paused}
              onClick={() => setPaused((value) => !value)}
            >
              {paused ? "继续输出" : "暂停输出"}
            </button>
          ) : null}
          {onCancel ? (
            <button type="button" className="qa-stream-cancel" onClick={onCancel}>
              取消
            </button>
          ) : null}
        </span>
      </div>

      {stream && stream.tools.length > 0 ? (
        <ul className="qa-stream-tools" aria-label="工具轨迹">
          {stream.tools.map((item) => (
            <li key={item.id}>
              <span className="qa-stream-tool-name">{item.tool}</span>
              <span>{item.status === "running" ? "执行中" : item.status}</span>
              {item.summary ? <span className="qa-stream-tool-summary">{item.summary}</span> : null}
            </li>
          ))}
        </ul>
      ) : null}

      {stream?.notice ? (
        <p className="qa-stream-notice" role="status">
          {stream.notice}
        </p>
      ) : null}

      {paused ? (
        <p className="qa-stream-paused" role="status">
          已暂停显示;生成仍在继续。点「继续输出」查看已到达内容。
        </p>
      ) : null}

      {stream?.stages !== undefined && stream.stages.length > 0 ? (
        <details className="qa-stream-stages">
          <summary>阶段耗时(来自真实事件)</summary>
          <ul>
            {stream.stages.map((entry, index) => {
              const end = entry.endedAt ?? (running ? Date.now() : entry.startedAt);
              const ms = Math.max(0, end - entry.startedAt);
              return (
                <li key={`${entry.stage}-${index}`}>
                  <span>{stageLabel(entry.stage) ?? entry.stage}</span>
                  <span>{ms < 1000 ? `${ms} ms` : `${Math.round(ms / 1000)} s`}</span>
                </li>
              );
            })}
          </ul>
        </details>
      ) : null}

      {showReasoning && stream && stream.reasoning.length > 0 ? (
        <details className="qa-stream-reasoning" open>
          <summary>模型原始推理</summary>
          <pre>{stream.reasoning}</pre>
        </details>
      ) : null}

      {smoothAnswer.length > 0 ? (
        // 流式阶段就用与终态相同的 Markdown 树渲染:若这里先出纯文本,
        // 消息落定时整段替换成 Markdown 会闪一下并重排(长答案尤甚)。
        // 提取器只输出 segments[].text,原始 JSON 不会到这里。
        // 纯段落长文按句断段,避免输出过程中挤成一整块"密不透风"的话。
        <div className="qa-stream-answer qa-segment-markdown">
          <StreamingMarkdown text={parseSegmentText(smoothAnswer).content} />
          {stream?.status === "running" ? (
            <span className="qa-stream-caret" aria-hidden="true" />
          ) : null}
        </div>
      ) : null}

      {stream?.status === "failed" ? (
        <p className="qa-stream-failed" role="alert">
          {stream.visibleMessage ?? "回答未完成,可重试。"}
        </p>
      ) : null}

      {running && !stage && !stream?.reasoning && !stream?.answer && stream?.status === "idle" ? (
        <p className="qa-stream-idle">已提交;等待模型返回首个真实事件。</p>
      ) : null}
    </div>
  );
}

/**
 * 纯段落文本的可读性补强:
 * - 已有块级 Markdown(标题/列表/代码围栏/表格/引用)一律不动;
 * - 单段落且明显过长时,按句末标点切分为多段,段间留空行。
 * 只影响渲染,不改数据库内容。
 */
function breakDenseParagraph(text: string): string {
  const trimmed = text.trim();
  if (trimmed.length < 60) return text;
  // 已有块级 Markdown 结构(标题/列表/代码/表格/引用)一律不动。
  if (/^(#{1,6}\s|[-*+]\s|\d+\.\s|>|```|\|)/m.test(trimmed)) return text;
  if (/\n\s*\n/.test(trimmed)) return text;
  // 中文回答常把多个要点塞进一个长段,只靠句号切分仍是一坨。
  // 先按句末标点切,再按"；/;/:"这类分句点兜底。
  const sentences = trimmed
    .split(/(?<=[。！？])/)
    .flatMap((part) => (part.length > 80 ? part.split(/(?<=[;；])/) : [part]))
    .filter((part) => part.trim().length > 0);
  if (sentences.length < 2) return text;
  const paragraphs: string[] = [];
  let current = "";
  for (const sentence of sentences) {
    current += sentence;
    if (current.length >= 60) {
      paragraphs.push(current.trim());
      current = "";
    }
  }
  if (current.trim()) paragraphs.push(current.trim());
  return paragraphs.length >= 2 ? paragraphs.join("\n\n") : text;
}

/** 按空行切块,但不拆开代码围栏/表格;流式渲染时已完成块保持稳定。 */
export function splitMarkdownBlocks(text: string): string[] {
  const lines = text.split(/\r?\n/);
  const blocks: string[] = [];
  let current: string[] = [];
  let fence = false;
  for (const line of lines) {
    if (/^\s*```/.test(line)) fence = !fence;
    if (!fence && line.trim() === "") {
      if (current.length > 0) {
        blocks.push(current.join("\n").replace(/\n+$/, ""));
        current = [];
      }
      continue;
    }
    current.push(line);
  }
  if (current.length > 0) blocks.push(current.join("\n").replace(/\n+$/, ""));
  return blocks.length > 0 ? blocks : [""];
}

const StreamingBlock = memo(function StreamingBlock({ text }: { text: string }) {
  return (
    <div className="qa-stream-block">
      <Markdown>{breakDenseParagraph(text)}</Markdown>
    </div>
  );
});

/**
 * 块级增量渲染:已完结段落的文本不变,memo 阻止整篇 Markdown 重排;
 * 只有最后一块随着新字增长重渲染。
 */
function StreamingMarkdown({ text }: { text: string }): ReactNode {
  const blocks = useMemo(() => splitMarkdownBlocks(text), [text]);
  return (
    <>
      {blocks.map((block, index) => (
        <StreamingBlock key={index} text={block} />
      ))}
    </>
  );
}

/**
 * 合并碎片段:模型常把一句话拆成好几个 segment(尤其在流式与结构化
 * 混用下),逐段渲染会变成"一句一行"的碎片感。
 *
 * 规则保守:只合并「相邻、同 kind、同校验状态、无引用、且都很短」的段;
 * 带引用的 document_fact 一律不合并(会破坏引用与段的对应关系)。
 */
function mergeFragments(segments: AnswerContent["segments"]): AnswerContent["segments"] {
  const out: AnswerContent["segments"] = [];
  for (const segment of segments) {
    const previous = out[out.length - 1];
    const mergeable =
      previous !== undefined &&
      previous.kind === segment.kind &&
      previous.verificationStatus === segment.verificationStatus &&
      previous.citationIds.length === 0 &&
      segment.citationIds.length === 0 &&
      !previous.text.includes("\n\n") &&
      previous.text.length < 48 &&
      segment.text.length < 120;
    if (mergeable) {
      const joiner = /[。！？.!?]$/.test(previous.text.trim()) ? "" : "";
      out[out.length - 1] = {
        ...previous,
        text: `${previous.text.trimEnd()}${joiner} ${segment.text.trimStart()}`,
      };
    } else {
      out.push(segment);
    }
  }
  return out;
}

function AnswerBody({
  answer,
  onCitationClick,
  savedReasoning,
  showReasoning = true,
}: {
  answer: AnswerContent;
  onCitationClick: (citationId: string) => void;
  savedReasoning?: string | null;
  showReasoning?: boolean;
}): ReactNode {
  const [citationsOpen, setCitationsOpen] = useState(answer.citations.length <= 2);
  // 「未校验/冲突过滤」:只显示带引用或未校验的段落(冲突提示始终保留)。
  const [onlyCited, setOnlyCited] = useState(false);
  const mergedSegments = mergeFragments(answer.segments);
  const canFilter = answer.citations.length > 0 || answer.conflicts.length > 0;
  // 只按“是否真的带引用”筛段落。校验阶段会把所有无引用的 document_fact
  // 降为 unverified;旧实现把 unverified 也保留,于是筛选永远不减少段落。
  // 冲突条目是回答级数据,始终显示在段落之外,不受本筛选影响。
  const visibleSegments = onlyCited
    ? mergedSegments.filter((segment) => segment.citationIds.length > 0)
    : mergedSegments;
  return (
    <div className="qa-answer-container">
      {showReasoning && savedReasoning ? (
        <details className="qa-history-reasoning">
          <summary>已保存的思考记录</summary>
          <pre>{savedReasoning}</pre>
        </details>
      ) : null}
      {answer.status !== "answered" ? (
        <div className={`qa-answer-status status-${answer.status}`} role="status">
          <span className="qa-answer-status-title">提示</span>
          <span>{STATUS_BANNERS[answer.status]}</span>
        </div>
      ) : null}

      {canFilter ? (
        <label className="qa-filter-row">
          <input
            type="checkbox"
            checked={onlyCited}
            onChange={(event) => setOnlyCited(event.target.checked)}
          />
          <span title="只隐藏没有原文引用的段落;冲突提示仍会单独显示">
            仅看带原文引用的段落
          </span>
        </label>
      ) : null}

      <div className="qa-markdown-wrapper">
        {visibleSegments.length === 0 ? (
          <p className="qa-filter-empty" role="status">
            没有带原文引用的段落;取消筛选查看完整回答。
          </p>
        ) : null}
        {visibleSegments.map((segment) => {
          const { thinking, content: rawText } = parseSegmentText(segment.text);
          // 段间空一行的可读性:模型有时把多个要点挤在一个 segment 的
          // 单行文本里(没有换行),直接渲染会变成"一大坨"。这里只在
          // "没有自带块级结构"的纯段落文本上,把中文分句按句号断成
          // 段落,保留列表/标题/代码等已有结构不动。
          const cleanText = breakDenseParagraph(rawText);
          const kindBadge = SEGMENT_BADGES[segment.kind];
          // 段级引用告警:仅 document_fact 且该段引用未通过核对。
          const citationWarning = needsCitationWarning(segment.kind, segment.verificationStatus);
          return (
            <div key={segment.id} className="qa-segment-item">
              {thinking ? (
                <details className="qa-history-reasoning">
                  <summary>模型原始推理(历史消息)</summary>
                  <pre>{thinking}</pre>
                </details>
              ) : null}
              {kindBadge && segment.kind !== "general_note" ? (
                <span className="qa-badge">{kindBadge}</span>
              ) : null}
              {citationWarning ? (
                <span
                  className="qa-badge qa-badge-citation-warning"
                  role="note"
                  aria-label="引用未校验:本段引用未能核实,内容可能不准确"
                  title="本段引用未能核实,内容可能不准确"
                >
                  引用未校验
                </span>
              ) : null}
              {cleanText ? (
                <div className="qa-segment-markdown">
                  <Markdown>{cleanText}</Markdown>
                </div>
              ) : null}
            </div>
          );
        })}
      </div>

      {answer.conflicts.map((conflict, i) => (
        <div key={i} className="qa-conflict">
          <span className="qa-conflict-title">冲突提示</span>
          {conflict.description}
        </div>
      ))}

      {answer.citations.length > 0 ? (
        <div className="qa-citations-section">
          <button
            type="button"
            className="qa-citations-label"
            aria-expanded={citationsOpen}
            aria-controls={`citations-${answer.schemaVersion}-${answer.citations[0]?.id ?? "none"}`}
            onClick={() => setCitationsOpen((value) => !value)}
          >
            <ChevronDown
              className={`w-3 h-3 qa-citations-chevron${citationsOpen ? " is-open" : ""}`}
              aria-hidden="true"
            />
            <span>参考引用 {answer.citations.length} 条</span>
            <span className="qa-citations-hint">
              {citationsOpen ? "点击收起" : "点击展开并定位原文"}
            </span>
          </button>
          {citationsOpen ? (
            <div
              id={`citations-${answer.schemaVersion}-${answer.citations[0]?.id ?? "none"}`}
              className="qa-citations"
              aria-label="参考引用"
            >
              {answer.citations.map((citation) => (
                <button
                  key={citation.id}
                  type="button"
                  className="qa-citation"
                  title={`定位原文:${citation.quotedText}`}
                  aria-label={`定位原文引用:${citation.quotedText}`}
                  onClick={() => onCitationClick(citation.id)}
                >
                  <Quote className="w-3 h-3" aria-hidden="true" />
                  <span className="qa-citation-quote">「{citation.quotedText}」</span>
                  <span className="qa-citation-preview" role="tooltip">
                    {citation.quotedText}
                  </span>
                </button>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}

      {/* 引用说明:仅提示“部分引用/降级说明”,不是整条回答的状态。 */}
      {answer.unverifiedNotes.length > 0 ? (
        <div className="qa-citation-note" role="note" aria-label="引用说明">
          <span className="qa-citation-note-title">引用说明</span>
          <ul className="qa-citation-note-list">
            {answer.unverifiedNotes.map((note, i) => (
              <li key={i}>{formatCitationNote(note)}</li>
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  );
}

function AgentThinkingProcess({ steps }: { steps: AgentStepView[] }): ReactNode {
  const [open, setOpen] = useState(false);
  const toolNames = steps
    .filter((step) => step.kind === "tool" && step.toolName)
    .map((step) => step.toolName as string);

  return (
    <div className="qa-agent-trace">
      <button
        type="button"
        className="qa-agent-trace-toggle"
        onClick={() => setOpen((prev) => !prev)}
        aria-expanded={open}
      >
        <span>
          研究步骤 · {steps.length}
          {toolNames.length > 0 ? ` · ${toolNames.join(" / ")}` : ""}
        </span>
        {/* 工具名保持可检索(屏幕阅读器与测试),不伪造额外叙述。 */}
        <span className="sr-only">{toolNames.join(" ")}</span>
        <span className="qa-agent-trace-caret">{open ? "⌵" : "›"}</span>
      </button>
      {open ? (
        <ol className="qa-agent-trace-list">
          {steps.map((step) => (
            <li key={`${step.stepId || step.stepNo}-${step.kind}`}>
              <span className="qa-agent-trace-name">
                {step.kind === "tool" ? step.toolName ?? "工具" : "模型"}
              </span>
              <span> · {step.status}</span>
              {step.resultSummary ? <span> · {step.resultSummary}</span> : null}
              {step.errorCode ? <span className="qa-agent-trace-error"> · {step.errorCode}</span> : null}
            </li>
          ))}
        </ol>
      ) : null}
    </div>
  );
}

function AgentAnswerBody({
  content,
  onCitationClick,
  savedReasoning,
  showReasoning = true,
}: {
  content: AgentAnswerV1;
  onCitationClick: (citationId: string) => void;
  savedReasoning?: string | null;
  showReasoning?: boolean;
}): ReactNode {
  return (
    <>
      {content.stoppedReason === "max_steps" ? (
        <p className="qa-agent-converged" role="note">
          已达到最大研究步数,已强制收敛为最终答复。
        </p>
      ) : null}
      {!content.verified ? (
        <p className="qa-agent-freeform-note" role="note">
          无引用自由文本:本条答复未走引用核对,内容请自行判断。
        </p>
      ) : null}
      {content.steps.length > 0 ? (
        <AgentThinkingProcess steps={content.steps} />
      ) : null}
      <AnswerBody
        answer={content.answer}
        onCitationClick={onCitationClick}
        savedReasoning={savedReasoning}
        showReasoning={showReasoning}
      />
    </>
  );
}

/** Agent 回答的复制文本:未通过引用核对时在开头说明为无引用自由文本。 */
export function agentAnswerPlainText(content: AgentAnswerV1): string {
  return `${content.verified ? "" : "无引用自由文本\n"}${answerPlainText(content.answer)}`;
}

export function MessageView({
  content,
  lifecycle,
  errorCode,
  onCitationClick,
  attachmentImages,
  stream,
  onCancel,
  reasoning,
  showReasoning = true,
  onNoticeAction,
}: {
  content: MessageContent;
  lifecycle: AnswerLifecycle;
  errorCode?: string;
  onCitationClick: (citationId: string) => void;
  /** attachment:<id> → 展示用 data URL(只来自 IPC,不写数据库)。 */
  attachmentImages?: Record<string, string>;
  /** 当前运行的临时流(仅占位消息使用;非事实来源)。 */
  stream?: StreamState;
  onCancel?: () => void;
  /** 用户开启“保存思考记录”后的历史 reasoning(默认不落库)。 */
  reasoning?: string | null;
  /** 设置中的“显示模型原始推理”;关闭后仍显示真实工具/阶段轨迹。 */
  showReasoning?: boolean;
  /** notice 动作(设置/授权/重试);由容器决定是否可执行。 */
  onNoticeAction?: (actionId: string) => void;
}): ReactNode {
  switch (content.schemaVersion) {
    case "user_message.v1":
      // 用户消息可能是多行纯文本或 Markdown(新消息为 attachment:<id> 引用;
      // 历史消息可能仍是 data-URI,两者都必须能回看)。
      return (
        <div className="qa-user-message">
          <Markdown
            imageResolver={(src) => {
              if (!src.startsWith("attachment:")) return undefined;
              return attachmentImages?.[src.slice("attachment:".length)] ?? null;
            }}
          >
            {content.text}
          </Markdown>
        </div>
      );
    case "assistant_placeholder.v1":
      return (
        <PendingStreamView
          lifecycle={lifecycle}
          errorCode={errorCode}
          stream={stream}
          onCancel={onCancel}
          showReasoning={showReasoning}
        />
      );
    case "answer.v1":
      return (
        <AnswerBody
          answer={content.content}
          onCitationClick={onCitationClick}
          savedReasoning={reasoning}
          showReasoning={showReasoning}
        />
      );
    case "agent.v1":
      return (
        <AgentAnswerBody
          content={content.content}
          onCitationClick={onCitationClick}
          savedReasoning={reasoning}
          showReasoning={showReasoning}
        />
      );
    case "assistant_notice.v1":
      return (
        <div className="qa-notice" role="alert">
          <p className="qa-notice-text">{content.notice.message}</p>
          {content.notice.actions.length > 0 ? (
            <div className="qa-notice-actions">
              {content.notice.actions.map((action) => (
                <button
                  key={action.id}
                  type="button"
                  className="btn small"
                  onClick={() => onNoticeAction?.(action.id)}
                >
                  {action.label}
                </button>
              ))}
            </div>
          ) : null}
          <p className="qa-notice-reason">兜底通知 · {content.notice.reasonCode}</p>
        </div>
      );
  }
}
