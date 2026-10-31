// 默认提问/生成范围:明确选区/位置/章节优先;只有全文兜底时才要求确认。
import type { ContextSuggestion } from "../../shared/generated/ContextSuggestion";
import type { ScopeDraft } from "../../shared/generated/ScopeDraft";

export function pickDefaultScope(
  candidates: ContextSuggestion["candidates"],
): ScopeDraft {
  const ordered = [...candidates].sort((a, b) => a.rank - b.rank);
  const nonFull = ordered.find((c) => c.scope.kind !== "full_document");
  if (nonFull) return nonFull.scope;
  const full = ordered.find((c) => c.scope.kind === "full_document");
  if (full) {
    return full.scope.kind === "full_document"
      ? { ...full.scope, confirmed: true }
      : full.scope;
  }
  return { kind: "full_document", confirmed: true };
}
