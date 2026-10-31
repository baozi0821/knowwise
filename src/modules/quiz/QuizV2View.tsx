// 2B quiz.v2 渲染与作答:单选/多选/判断/填空确定性判分;问答 rubric 自评。
// 解析与引用只在作答后显示;引用回跳经 evidence_resolve_block,失败明确提示。
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { CommandClient } from "../../app/api/client";
import type { QuestionTypeV2 } from "../../shared/generated/QuestionTypeV2";
import type { QuizContentV2 } from "../../shared/generated/QuizContentV2";
import type { QuizQuestionV2 } from "../../shared/generated/QuizQuestionV2";
import type { QuizV2GradeResult } from "../../shared/generated/QuizV2GradeResult";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import { jumpToLocator } from "../notes/jumpToLocator";
import "./quizV2.css";

const TYPE_LABELS: Record<QuestionTypeV2, string> = {
  single_choice: "单选",
  multiple_choice: "多选",
  true_false: "判断",
  fill_blank: "填空",
  short_answer: "问答",
};

const DIFFICULTY_LABELS: Record<string, string> = {
  easy: "简单",
  medium: "中等",
  hard: "困难",
};

function QuestionView({
  client,
  artifactVersionId,
  documentId,
  question,
  index,
  onSubmitted,
}: {
  client: CommandClient;
  artifactVersionId: string;
  /** 引用定位所属文档;为空时阅读器按当前打开文档处理。 */
  documentId: string;
  question: QuizQuestionV2;
  index: number;
  onSubmitted: (element: HTMLLIElement) => void;
}) {
  // 每个题目实例独立命名；同题单选共享 name，重复卡片互不串组。
  const inputId = useId();
  const questionRef = useRef<HTMLLIElement>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [blanks, setBlanks] = useState<string[]>(() => question.blanks.map(() => ""));
  const [answerText, setAnswerText] = useState("");
  const [result, setResult] = useState<QuizV2GradeResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [gotoError, setGotoError] = useState<string | null>(null);

  useLayoutEffect(() => {
    if (result !== null && questionRef.current) onSubmitted(questionRef.current);
  }, [result, onSubmitted]);

  // 定位失败只做短暂提示,不把红字长期留在页面里。
  useEffect(() => {
    if (gotoError === null) return;
    const timer = window.setTimeout(() => setGotoError(null), 2800);
    return () => window.clearTimeout(timer);
  }, [gotoError]);

  function toggleOption(id: string) {
    if (result !== null || submitting) return;
    if (question.type === "multiple_choice") {
      setSelected((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
    } else {
      setSelected([id]);
    }
  }

  const canSubmit =
    question.type === "fill_blank"
      ? blanks.some((value) => value.trim().length > 0)
      : question.type === "short_answer"
        ? answerText.trim().length > 0
        : selected.length > 0;

  async function submit() {
    if (!canSubmit || submitting || result !== null) return;
    setSubmitting(true);
    setError(null);
    try {
      const grade = await client.call<QuizV2GradeResult>("quiz_submit_answer_v2", {
        artifactVersionId,
        questionId: question.id,
        selectedOptionIds: selected,
        blankAnswers: blanks,
        answerText: question.type === "short_answer" ? answerText : null,
        idempotencyKey: client.newIdempotencyKey(),
      });
      setResult(grade);
    } catch {
      setError("提交失败,请重试;答案未保存。");
    } finally {
      setSubmitting(false);
    }
  }

  async function gotoBlock(blockId: string) {
    setGotoError(null);
    try {
      const locator = await client.call<SourceLocator | null>("evidence_resolve_block", {
        blockId,
      });
      if (!locator) {
        setGotoError("无法定位该引用:原文块不存在。");
        return;
      }
      // 按 locator 类型分派:PDF 用页码,Markdown 用文本偏移,DOCX 用块序。
      // 只发 PDF 事件会让 Markdown/Word 文档的引用点击毫无反应。
      jumpToLocator(documentId, locator);
    } catch {
      setGotoError("无法定位该引用,请重试。");
    }
  }

  if (question.validity === "invalid") {
    return (
      <li className="quizv2-question invalid">
        <div className="quizv2-head">
          <span className="quizv2-index">第 {index} 题</span>
          <span className="quizv2-type">{TYPE_LABELS[question.type]}</span>
          <span className="quizv2-type">无效 · 不计分</span>
        </div>
        <p className="quizv2-prompt">{question.prompt}</p>
        <p className="quizv2-note">
          该题未通过服务端校验({question.invalidReason ?? "unknown"}),不参与作答。
        </p>
      </li>
    );
  }

  return (
    <li className="quizv2-question" ref={questionRef}>
      <div className="quizv2-head">
        <span className="quizv2-index">第 {index} 题</span>
        <span className="quizv2-type">{TYPE_LABELS[question.type]}</span>
        <span className="quizv2-difficulty">{DIFFICULTY_LABELS[question.difficulty] ?? question.difficulty}</span>
      </div>
      <p className="quizv2-prompt" id={`${inputId}-prompt`}>{question.prompt}</p>

      {question.type === "fill_blank" ? (
        <div className="quizv2-blanks">
          {question.blanks.map((blank, blankIndex) => (
            <label key={blank.id}>
              空 {blankIndex + 1}
              <input
                type="text"
                name={`${inputId}-blank-${blankIndex}`}
                aria-label={`第 ${index} 题，第 ${blankIndex + 1} 空`}
                aria-describedby={`${inputId}-prompt`}
                value={blanks[blankIndex] ?? ""}
                disabled={result !== null || submitting}
                onChange={(event) =>
                  setBlanks((prev) =>
                    prev.map((value, i) => (i === blankIndex ? event.target.value : value)),
                  )
                }
              />
            </label>
          ))}
        </div>
      ) : question.type === "short_answer" ? (
        <label className="quizv2-answer">
          你的回答
          <textarea
            className="quizv2-textarea"
            name={`${inputId}-answer`}
            aria-label={`第 ${index} 题的问答答案`}
            aria-describedby={`${inputId}-prompt`}
            rows={3}
            value={answerText}
            disabled={result !== null || submitting}
            placeholder="写下你的回答；提交后显示参考答案与评分要点，由你自评。"
            onChange={(event) => setAnswerText(event.target.value)}
          />
        </label>
      ) : (
        <fieldset className="quizv2-options" disabled={result !== null || submitting} aria-describedby={`${inputId}-prompt`}>
          <legend>第 {index} 题选项（{question.type === "multiple_choice" ? "可多选" : "单选"}）</legend>
          {question.options.map((option) => (
            <label key={option.id} className="quizv2-option">
              <input
                type={question.type === "multiple_choice" ? "checkbox" : "radio"}
                name={`${inputId}-options`}
                value={option.id}
                checked={selected.includes(option.id)}
                onChange={() => toggleOption(option.id)}
              />
              <span>{option.text}</span>
            </label>
          ))}
        </fieldset>
      )}

      {result === null ? (
        <div className="quizv2-question-actions">
          <button
            type="button"
            className="btn small"
            aria-label={submitting ? `正在提交第 ${index} 题答案` : `提交答案：第 ${index} 题`}
            onClick={() => void submit()}
            disabled={!canSubmit || submitting}
          >
            {submitting ? "提交中…" : "提交答案"}
          </button>
        </div>
      ) : (
        <div className="quizv2-result" role="status">
          {result.grading === "self_assessed" ? (
            <p>
              <strong>自评题</strong>:参考下方评分要点自行判断;系统不做自动判分。
            </p>
          ) : (
            <p>
              <strong>{result.isCorrect ? "回答正确" : "回答错误"}</strong>
              {result.score !== undefined && result.score !== null
                ? ` · 得分 ${result.score.toFixed(2)}/${result.maxScore.toFixed(2)}`
                : ""}
            </p>
          )}
          {result.correctOptionIds.length > 0 ? (
            <p>
              正确答案:
              {result.correctOptionIds
                .map((id) => question.options.find((option) => option.id === id)?.text ?? id)
                .join(" / ")}
            </p>
          ) : null}
          {result.referenceAnswer ? <p>参考答案:{result.referenceAnswer}</p> : null}
          {result.rubric.length > 0 ? (
            <div>
              <p>评分要点:</p>
              <ul>
                {result.rubric.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </div>
          ) : null}
          {question.explanationSegments.length > 0 ? (
            <div>
              <p>解析:</p>
              {question.explanationSegments.map((segment) => (
                <p key={segment.id} className="quizv2-explanation">
                  {segment.kind !== "document_fact" ? <span className="quizv2-badge">推断</span> : null}
                  {segment.text}
                </p>
              ))}
            </div>
          ) : null}
          {question.citations.length > 0 ? (
            <div className="quizv2-citations">
              <span>引用:</span>
              {question.citations.map((citation) => (
                <button
                  key={citation.id}
                  type="button"
                  className="quizv2-citation"
                  title={`定位原文:${citation.quotedText}`}
                  aria-label={`定位原文引用:${citation.quotedText}`}
                  onClick={() => void gotoBlock(citation.blockId)}
                >
                  「{citation.quotedText}」
                </button>
              ))}
            </div>
          ) : null}
        </div>
      )}
      {error ? (
        <p className="quizv2-error" role="alert">
          {error}
        </p>
      ) : null}
      {gotoError ? (
        <p className="quizv2-goto-hint" role="status">
          {gotoError}
        </p>
      ) : null}
    </li>
  );
}

export function QuizV2Stats({ content }: { content: QuizContentV2 }) {
  const invalid = content.questions.filter((q) => q.validity === "invalid").length;
  return (
    <div className="quizv2-stats" role="group" aria-label="题目统计">
      <p className="quizv2-stats-summary">
        <strong>共 {content.questions.length} 题</strong>
        <span>无效 {invalid} 题{invalid > 0 ? "（不计分）" : ""}</span>
      </p>
      <dl className="quizv2-distribution" aria-label="五种题型分布（含无效题）">
        {(Object.keys(TYPE_LABELS) as QuestionTypeV2[]).map((type) => (
          <div key={type}>
            <dt>{TYPE_LABELS[type]}</dt>
            <dd>{content.questions.filter((question) => question.type === type).length} 题</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

export function QuizV2View({
  client,
  artifactVersionId,
  documentId = "",
  content,
  scrollable = false,
  showStats = true,
  scrollLabel = "练习题列表，可滚动",
}: {
  client: CommandClient;
  artifactVersionId: string;
  /** 引用定位所属文档;为空时阅读器按当前打开文档处理。 */
  documentId?: string;
  content: QuizContentV2;
  scrollable?: boolean;
  showStats?: boolean;
  scrollLabel?: string;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const locateSubmittedQuestion = useCallback((question: HTMLLIElement) => {
    const container = scrollRef.current;
    if (!scrollable || !container) return;
    // 不抢走已移至其他题目/对话输入框的焦点，也不滚动外层页面。
    const active = document.activeElement;
    if (active && active !== document.body && active !== container && !question.contains(active)) return;
    const offset = question.getBoundingClientRect().top
      - container.getBoundingClientRect().top - container.clientTop;
    container.scrollTop += offset - 6;
  }, [scrollable]);

  return (
    <div className="quizv2">
      {showStats ? <QuizV2Stats content={content} /> : null}
      <div
        className={scrollable ? "quizv2-body quizv2-body-scrollable" : "quizv2-body"}
        ref={scrollRef}
        role={scrollable ? "region" : undefined}
        tabIndex={scrollable ? 0 : undefined}
        aria-label={scrollable ? scrollLabel : undefined}
      >
        {content.questions.length === 0 ? <p className="quizv2-note" role="status">该版本暂无题目。</p> : null}
        <ol className="quizv2-list">
          {content.questions.map((question, index) => (
            <QuestionView
              key={`${artifactVersionId}:${question.id}`}
              client={client}
              artifactVersionId={artifactVersionId}
              documentId={documentId}
              question={question}
              index={index + 1}
              onSubmitted={locateSubmittedQuestion}
            />
          ))}
        </ol>
      </div>
    </div>
  );
}
