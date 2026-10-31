// 产物「阅览」视图:像普通文档一样只读阅读产物内容。
//
// 设计要点(《M10》§6/§9,本轮 P0「产物中心-阅览」):
// - 统一用 Markdown 渲染(标题/列表/表格/引用/代码),不把原始 JSON 当正文;
// - summary/quiz/translation/graph 都给出可读投影,未知 schema 回退到
//   UnknownArtifact(不丢数据、不伪装成可读);
// - 顶部提供返回、标题、来源/覆盖信息与导出入口;
// - 只读:不提供编辑/作答,避免与产物中心的正式交互重复。
import { useMemo, useState, type ReactNode } from "react";
import { ArrowLeft, Check, FileText, Pencil, Quote, Trash2 } from "lucide-react";
import type { CommandClient } from "../../app/api/client";
import { toAppError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import type { ArtifactVersionView } from "../../shared/generated/ArtifactVersionView";
import type { ArtifactView } from "../../shared/generated/ArtifactView";
import type { CoverageReport } from "../../shared/generated/CoverageReport";
import type { GraphContent } from "../../shared/generated/GraphContent";
import type { QuizContentV1 } from "../../shared/generated/QuizContentV1";
import type { QuizContentV2 } from "../../shared/generated/QuizContentV2";
import type { SummaryContentV1 } from "../../shared/generated/SummaryContentV1";
import type { TranslationV1 } from "../../shared/generated/TranslationV1";
import { Markdown } from "../reader/Markdown";
import { UnknownArtifact } from "../workspace/artifactRenderers";
import { clipboardWriteText } from "../../app/ipc/transport";
import "./artifactReader.css";

const QUIZ_TYPE_LABELS: Record<string, string> = {
  single_choice: "单选",
  multiple_choice: "多选",
  true_false: "判断",
  fill_blank: "填空",
  short_answer: "问答",
};

const SEGMENT_LABELS: Record<string, string> = {
  document_fact: "原文事实",
  ai_inference: "AI 推断",
  user_view: "用户观点",
  needs_verification: "待核实",
};

/** 覆盖真相:不把“部分覆盖”说成全文。 */
export function readerCoverageText(coverage?: CoverageReport): string {
  if (!coverage) return "覆盖未知";
  const regions = coverage.regions ?? [];
  const labels = regions.map((region) => region.label).filter(Boolean);
  if (labels.length === 0) {
    return coverage.complete ? "全文(完整)" : "部分内容,有缺失";
  }
  return `${labels.join("、")}${coverage.complete ? "(完整)" : "(部分,有缺失)"}`;
}

function summaryMarkdown(content: SummaryContentV1): string {
  const lines: string[] = [];
  for (const section of content.sections) {
    lines.push(`## ${section.title}`);
    lines.push("");
    for (const segment of section.segments) {
      const label = SEGMENT_LABELS[segment.kind];
      if (label && segment.kind !== "document_fact") lines.push(`**(${label})**`, "");
      lines.push(segment.text, "");
    }
  }
  if (content.openQuestions.length > 0) {
    lines.push("## 待澄清");
    lines.push("");
    for (const question of content.openQuestions) {
      lines.push(`- ${question}`);
    }
  }
  return lines.join("\n");
}

function quizMarkdown(content: QuizContentV1 | QuizContentV2): string {
  const lines: string[] = [];
  content.questions.forEach((question, index) => {
    const isV2 = "type" in question;
    const type = isV2 ? (QUIZ_TYPE_LABELS[question.type] ?? question.type) : "单选";
    lines.push(`## 第 ${index + 1} 题 · ${type}`);
    lines.push("");
    lines.push(question.prompt);
    lines.push("");
    if (question.options.length > 0) {
      for (const [optionIndex, option] of question.options.entries()) {
        lines.push(`- ${String.fromCharCode(65 + optionIndex)}. ${option.text}`);
      }
      lines.push("");
    }
    if (isV2 && question.blanks.length > 0) {
      question.blanks.forEach((blank, blankIndex) => {
        lines.push(`- 空 ${blankIndex + 1}:____ (可接受:${blank.acceptedAnswers.join(" / ")})`);
      });
      lines.push("");
    }
    // v1 用 correctOptionId,v2 用 answer.correctOptionIds + referenceAnswer。
    const optionText = (id: string): string =>
      question.options.find((option) => option.id === id)?.text ?? id;
    const correctIds = isV2
      ? question.answer.correctOptionIds
      : question.correctOptionId
        ? [question.correctOptionId]
        : [];
    if (correctIds.length > 0) {
      lines.push(`**答案**:${correctIds.map(optionText).join(" / ")}`);
      lines.push("");
    }
    const referenceAnswer = isV2 ? question.answer.referenceAnswer : "";
    if (referenceAnswer.trim()) {
      lines.push(`**参考答案**:${referenceAnswer}`);
      lines.push("");
    }
    if (isV2 && question.answer.rubric.length > 0) {
      lines.push("**评分要点**:");
      lines.push("");
      for (const item of question.answer.rubric) {
        lines.push(`- ${item}`);
      }
      lines.push("");
    }
    if (question.explanationSegments.length > 0) {
      lines.push("**解析**:");
      lines.push("");
      for (const segment of question.explanationSegments) {
        lines.push(`- ${segment.text}`);
      }
      lines.push("");
    }
    if (question.citations.length > 0) {
      lines.push("**引用**:");
      lines.push("");
      for (const citation of question.citations) {
        lines.push(`- 「${citation.quotedText}」`);
      }
      lines.push("");
    }
  });
  return lines.join("\n");
}

function translationMarkdown(content: TranslationV1): string {
  const lines: string[] = [];
  for (const segment of content.segments) {
    lines.push(`## ${segment.pageNumber !== null && segment.pageNumber !== undefined ? `p.${segment.pageNumber}` : "段落"}`);
    lines.push("");
    if (segment.sourceText) {
      lines.push(`**原文**:${segment.sourceText}`);
      lines.push("");
    }
    lines.push(`**译文**:${segment.translatedText.trim() || "未完成(翻译失败;原文已保留)"}`);
    lines.push("");
  }
  if (content.glossary.length > 0) {
    lines.push("## 术语表");
    lines.push("");
    for (const entry of content.glossary) {
      lines.push(`- ${entry.source} → ${entry.target}`);
    }
  }
  return lines.join("\n");
}

function graphMarkdown(content: GraphContent): string {
  const lines: string[] = [`## 节点`, ""];
  for (const node of content.nodes) {
    lines.push(`- **${node.label}**${node.description ? `:${node.description}` : ""}(证据:${node.evidenceStatus})`);
  }
  lines.push("");
  lines.push("## 关系");
  lines.push("");
  for (const edge of content.edges) {
    lines.push(`- ${edge.label ? `${edge.label}:` : ""}${edge.source} → ${edge.target}(确定性:${edge.certainty})`);
  }
  return lines.join("\n");
}

/** 把产物内容投影为可读 Markdown;未知 schema 返回 null(由调用方兜底)。 */
export function artifactToMarkdown(
  version: Pick<ArtifactVersionView, "schemaVersion" | "content">,
): string | null {
  const content = version.content as Record<string, unknown> | null;
  if (content === null || typeof content !== "object") return null;
  switch (version.schemaVersion) {
    case "summary.v1":
      return summaryMarkdown(content as unknown as SummaryContentV1);
    case "quiz.v1":
    case "quiz.v2":
      return quizMarkdown(content as unknown as QuizContentV1 | QuizContentV2);
    case "translation.v1":
      return translationMarkdown(content as unknown as TranslationV1);
    case "graph.v1":
      return graphMarkdown(content as unknown as GraphContent);
    default:
      return null;
  }
}

type SegmentReviewStatus = "pending" | "accepted" | "rewritten";

export function ArtifactReader({
  artifact,
  version,
  onBack,
  exportControls,
  client,
  onSaved,
}: {
  artifact: ArtifactView;
  version: ArtifactVersionView;
  onBack: () => void;
  /** 导出入口由产物中心注入(复用同一套预览/保存流程,避免两套导出)。 */
  exportControls?: ReactNode;
  client?: CommandClient;
  onSaved?: () => void;
}): ReactNode {
  const [showSources, setShowSources] = useState(false);
  const markdown = useMemo(() => artifactToMarkdown(version), [version]);
  const [review, setReview] = useState<SummaryContentV1 | null>(null);
  const [reviewStatus, setReviewStatus] = useState<Record<string, SegmentReviewStatus>>({});
  const [reviewBusy, setReviewBusy] = useState(false);
  const [reviewError, setReviewError] = useState<string | null>(null);
  const [reviewNotice, setReviewNotice] = useState<string | null>(null);
  const [reviewHint, setReviewHint] = useState<string | null>(null);

  const summaryContent = useMemo(() => {
    if (version.schemaVersion !== "summary.v1") return null;
    const content = version.content as Partial<SummaryContentV1> | null;
    if (
      content === null ||
      typeof content !== "object" ||
      !Array.isArray(content.sections) ||
      content.sections.length === 0
    ) {
      return null;
    }
    return content as SummaryContentV1;
  }, [version]);

  function startReview() {
    if (summaryContent === null) {
      setReviewHint("该产物没有分段结构(不是摘要分段格式),无法逐段审阅;仍可整体复制或以新版本重新生成。");
      return;
    }
    setReviewHint(null);
    setReview(structuredClone(summaryContent));
    setReviewStatus({});
    setReviewError(null);
    setReviewNotice(null);
  }

  function updateSegmentText(sectionIndex: number, segmentIndex: number, text: string) {
    setReview((prev) => {
      if (prev === null) return prev;
      const next = structuredClone(prev);
      const segment = next.sections[sectionIndex]?.segments[segmentIndex];
      if (segment === undefined) return prev;
      segment.text = text;
      return next;
    });
    setReviewStatus((prev) => {
      const segment = review?.sections[sectionIndex]?.segments[segmentIndex];
      if (segment === undefined) return prev;
      return { ...prev, [segment.id]: "rewritten" };
    });
  }

  function removeSegment(sectionIndex: number, segmentIndex: number) {
    setReview((prev) => {
      if (prev === null) return prev;
      const next = structuredClone(prev);
      next.sections[sectionIndex]?.segments.splice(segmentIndex, 1);
      return next;
    });
  }

  async function saveReview() {
    if (client === undefined || review === null) return;
    setReviewBusy(true);
    setReviewError(null);
    setReviewNotice(null);
    try {
      const edited = await client.call<ArtifactView>("artifact_begin_edit", {
        artifactId: artifact.id,
        baseVersionId: version.id,
        idempotencyKey: client.newIdempotencyKey(),
      });
      const candidates = [
        ...(edited.versions ?? []),
        ...(edited.currentVersion !== undefined ? [edited.currentVersion] : []),
      ];
      const editable =
        candidates.find(
          (candidate) =>
            candidate.state === "mutable" &&
            candidate.origin === "user_edit" &&
            candidate.id !== version.id,
        ) ??
        candidates.find((candidate) => candidate.state === "mutable");
      if (editable === undefined) {
        throw new Error("没有可写的用户版本");
      }
      const saved = await client.call<ArtifactView>("artifact_save", {
        artifactId: artifact.id,
        versionId: editable.id,
        content: review,
        expectedRevision: editable.revision,
        idempotencyKey: client.newIdempotencyKey(),
      });
      const savedVersion =
        saved.currentVersion ??
        saved.versions.find((candidate) => candidate.id === editable.id);
      setReviewNotice(
        `已保存人工版本 v${savedVersion?.versionNo ?? "?"};旧版本保留,未被覆盖。`,
      );
      setReview(null);
      setReviewStatus({});
      onSaved?.();
    } catch (error) {
      const appError = toAppError(error);
      setReviewError(describeAppError(appError).title);
    } finally {
      setReviewBusy(false);
    }
  }

  return (
    <section className="artifact-reader" aria-label={`阅览:${artifact.title}`}>
      <header className="artifact-reader-head">
        <button
          type="button"
          className="btn tiny ghost artifact-reader-back"
          onClick={onBack}
          aria-label="返回产物列表"
        >
          <ArrowLeft className="w-3.5 h-3.5" aria-hidden="true" />
          <span>返回</span>
        </button>
        <div className="artifact-reader-title">
          <FileText className="w-4 h-4" aria-hidden="true" />
          <h3>{artifact.title}</h3>
        </div>
        {exportControls}
        <button
          type="button"
          className="btn tiny ghost"
          onClick={startReview}
          aria-label="逐段审阅"
        >
          <Pencil className="w-3 h-3" aria-hidden="true" />
          <span>逐段审阅</span>
        </button>
      </header>
      {reviewHint !== null ? (
        <p className="artifact-reader-warning" role="status">
          {reviewHint}
        </p>
      ) : null}
      {reviewError !== null ? (
        <p className="artifact-reader-warning" role="alert">
          保存人工版本失败:{reviewError}
        </p>
      ) : null}
      {reviewNotice !== null ? (
        <p className="artifact-reader-notice" role="status">
          {reviewNotice}
        </p>
      ) : null}

      <p className="artifact-reader-meta">
        第 {version.versionNo} 版 · 覆盖:{readerCoverageText(version.coverage)} · 引用{" "}
        {artifact.citationCount}
      </p>
      {version.coverage.complete === false ? (
        <p className="artifact-reader-warning" role="status">
          本产物只覆盖所选范围,未完整覆盖全文;不要据此推断全文结论。
        </p>
      ) : null}

      <article className="artifact-reader-body">
        {review !== null ? (
          <div className="artifact-review" aria-label="逐段审阅">
            <div className="artifact-review-head">
              <span>
                逐段审阅:{version.origin} / v{version.versionNo}
              </span>
              <span className="spacer" />
              <button
                type="button"
                className="btn tiny ghost"
                onClick={() => {
                  setReview(null);
                  setReviewStatus({});
                  setReviewError(null);
                }}
                disabled={reviewBusy}
              >
                取消
              </button>
              <button
                type="button"
                className="btn tiny"
                onClick={() => void saveReview()}
                disabled={reviewBusy}
              >
                {reviewBusy ? "保存中…" : "保存人工版本"}
              </button>
            </div>
            <p className="artifact-review-hint">
              「接受」保留原文;「改写」直接编辑文本;「删除」从人工版本中移除该段。保存会创建新的 user_edit 版本,旧版不覆盖。
            </p>
            {review.sections.map((section, sectionIndex) => (
              <section key={section.id} className="artifact-review-section">
                <h4>{section.title}</h4>
                {section.segments.map((segment, segmentIndex) => {
                  const status = reviewStatus[segment.id] ?? "pending";
                  return (
                    <div
                      key={segment.id}
                      className={`artifact-review-segment status-${status}`}
                    >
                      <div className="artifact-review-segment-head">
                        <span className="artifact-review-kind">
                          {SEGMENT_LABELS[segment.kind] ?? segment.kind}
                        </span>
                        <button
                          type="button"
                          className={`btn tiny ghost${status === "accepted" ? " active" : ""}`}
                          onClick={() =>
                            setReviewStatus((prev) => ({
                              ...prev,
                              [segment.id]: "accepted",
                            }))
                          }
                        >
                          <Check className="w-3 h-3" aria-hidden="true" />
                          接受
                        </button>
                        <button
                          type="button"
                          className="btn tiny ghost"
                          onClick={() => {
                            setReviewStatus((prev) => ({
                              ...prev,
                              [segment.id]: "rewritten",
                            }));
                            document
                              .getElementById(`review-${segment.id}`)
                              ?.focus();
                          }}
                        >
                          <Pencil className="w-3 h-3" aria-hidden="true" />
                          改写
                        </button>
                        <button
                          type="button"
                          className="btn tiny danger-ghost"
                          aria-label="删除该段"
                          onClick={() => removeSegment(sectionIndex, segmentIndex)}
                        >
                          <Trash2 className="w-3 h-3" aria-hidden="true" />
                        </button>
                      </div>
                      <textarea
                        id={`review-${segment.id}`}
                        value={segment.text}
                        aria-label={`第 ${sectionIndex + 1} 节第 ${segmentIndex + 1} 段`}
                        onChange={(event) =>
                          updateSegmentText(sectionIndex, segmentIndex, event.target.value)
                        }
                      />
                      {segment.citationIds.length > 0 ? (
                        <span className="artifact-review-citations">
                          引用 {segment.citationIds.length} 条
                        </span>
                      ) : null}
                    </div>
                  );
                })}
              </section>
            ))}
          </div>
        ) : markdown !== null ? (
          <Markdown>{markdown}</Markdown>
        ) : (
          <UnknownArtifact
            kind={artifact.kind}
            contentJson={JSON.stringify(version.content, null, 2)}
            writeClipboard={clipboardWriteText}
          />
        )}
      </article>

      <footer className="artifact-reader-foot">
        <button
          type="button"
          className="btn tiny ghost"
          aria-expanded={showSources}
          onClick={() => setShowSources((value) => !value)}
        >
          <Quote className="w-3 h-3" aria-hidden="true" />
          <span>{showSources ? "收起来源信息" : "来源与覆盖"}</span>
        </button>
        {showSources ? (
          <dl className="artifact-reader-sources">
            <dt>覆盖</dt>
            <dd>{readerCoverageText(version.coverage)}</dd>
            <dt>版本</dt>
            <dd>
              v{version.versionNo} · {version.origin} · {version.state}
            </dd>
            <dt>引用数</dt>
            <dd>{artifact.citationCount}</dd>
          </dl>
        ) : null}
      </footer>
    </section>
  );
}
