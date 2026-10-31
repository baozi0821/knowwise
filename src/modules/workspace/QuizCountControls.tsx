// 分题型出题数量控制(单选/多选/判断/填空/问答)。
//
// 设计约定(《M11》quiz.v2 + 本轮 P0「分题型数量」):
// - 总数 = 各题型之和;全为 0 时按「混合/自动」由后端在默认题型内分配;
// - 任一项改动都会实时更新总数,超出上限(20)时禁用确认并说明原因;
// - 只提交数量,不提交题型枚举列表:后端按数量推导 types,避免两处不一致。
import type { ReactNode } from "react";
import type { QuizPerTypeCounts } from "../../shared/generated/QuizPerTypeCounts";

export const MAX_QUIZ_TOTAL = 20;

export const QUIZ_TYPE_FIELDS: Array<{
  key: keyof QuizPerTypeCounts;
  label: string;
  hint: string;
}> = [
  { key: "singleChoice", label: "单选", hint: "四选一,唯一正确答案" },
  { key: "multipleChoice", label: "多选", hint: "多个正确选项" },
  { key: "trueFalse", label: "判断", hint: "正确 / 错误" },
  { key: "fillBlank", label: "填空", hint: "按可接受答案判分" },
  { key: "shortAnswer", label: "问答", hint: "参考答案 + 评分要点,自评" },
];

export const EMPTY_PER_TYPE_COUNTS: QuizPerTypeCounts = {
  singleChoice: 0,
  multipleChoice: 0,
  trueFalse: 0,
  fillBlank: 0,
  shortAnswer: 0,
};

/** 总题数 = 各题型之和。 */
export function quizTotal(counts: QuizPerTypeCounts): number {
  return QUIZ_TYPE_FIELDS.reduce((sum, field) => sum + (counts[field.key] || 0), 0);
}

/** 是否显式指定了题型(任一 > 0);否则为「混合/自动」。 */
export function isPerTypeExplicit(counts: QuizPerTypeCounts): boolean {
  return quizTotal(counts) > 0;
}

/**
 * 表单可提交性:全 0 = 混合自动(合法);只有超过上限才阻止提交。
 * 不把“未指定题型”当错误:默认就该能直接生成混合题。
 */
export function quizConfigIssue(counts: QuizPerTypeCounts): string | null {
  const total = quizTotal(counts);
  if (total > MAX_QUIZ_TOTAL) return `总题数 ${total} 超过上限 ${MAX_QUIZ_TOTAL}。`;
  return null;
}

export function QuizCountControls({
  counts,
  onChange,
  disabled = false,
}: {
  counts: QuizPerTypeCounts;
  onChange: (next: QuizPerTypeCounts) => void;
  disabled?: boolean;
}): ReactNode {
  const total = quizTotal(counts);
  const explicit = isPerTypeExplicit(counts);
  const issue = quizConfigIssue(counts);

  function setCount(key: keyof QuizPerTypeCounts, value: string): void {
    const parsed = Number.parseInt(value, 10);
    const next = Number.isFinite(parsed) ? Math.max(0, Math.min(MAX_QUIZ_TOTAL, parsed)) : 0;
    onChange({ ...counts, [key]: next });
  }

  return (
    <div className="quiz-counts" role="group" aria-label="题型与数量">
      <div className="quiz-counts-head">
        <span className="quiz-counts-title">题型数量</span>
        <span className="quiz-counts-total" aria-live="polite">
          共 {total} 题{explicit ? "" : "(混合自动)"}
        </span>
        <button
          type="button"
          className="btn tiny ghost"
          disabled={disabled || !explicit}
          onClick={() => onChange({ ...EMPTY_PER_TYPE_COUNTS })}
        >
          改为混合自动
        </button>
      </div>
      <div className="quiz-counts-grid">
        {QUIZ_TYPE_FIELDS.map((field) => (
          <label key={field.key} title={field.hint}>
            <span>{field.label}</span>
            <input
              type="number"
              min={0}
              max={MAX_QUIZ_TOTAL}
              step={1}
              inputMode="numeric"
              value={counts[field.key] === 0 ? "" : String(counts[field.key])}
              placeholder="0"
              aria-label={`${field.label}题数量`}
              disabled={disabled}
              onChange={(event) => setCount(field.key, event.target.value)}
            />
          </label>
        ))}
      </div>
      {issue ? (
        <p className="quiz-counts-issue" role="status">
          {issue}
        </p>
      ) : null}
    </div>
  );
}
