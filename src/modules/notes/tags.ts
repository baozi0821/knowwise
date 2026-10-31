// 笔记标签编辑(纯函数):输入解析、规范化、去重、删除。
// 规范化语义与 filters.ts / bodyPolicy.ts 完全一致(normalizeTag/normalizeTagList),
// 面板里的 chips 编辑与后端 note_save 校验不会出现两套规则。
import {
  MAX_NOTE_TAGS,
  normalizeTag,
  normalizeTagList,
} from "./bodyPolicy";

export type AddTagsFailure = "empty" | "invalid" | "duplicate" | "limit";

export type AddTagsResult =
  | { ok: true; tags: string[] }
  | { ok: false; reason: AddTagsFailure };

export const TAG_ERROR_LABELS: Record<AddTagsFailure, string> = {
  empty: "请输入标签",
  invalid: "标签为空、过长或包含非法内容",
  duplicate: "标签已存在",
  limit: `最多 ${MAX_NOTE_TAGS} 个标签`,
};

/** 解析一次输入:支持英文/中文逗号、分号分隔多个标签。 */
export function parseTagInput(raw: string): string[] {
  return raw
    .split(/[,，;；]/)
    .map((part) => part.trim().replace(/\s+/g, " "))
    .filter((part) => part !== "");
}

/** 添加标签(可一次多个):规范化后去重,超过上限拒绝。 */
export function addTags(tags: string[], raw: string): AddTagsResult {
  const parsed = parseTagInput(raw);
  if (parsed.length === 0) return { ok: false, reason: "empty" };
  const next = [...tags];
  for (const candidate of parsed) {
    const norm = normalizeTag(candidate);
    if (norm === null) return { ok: false, reason: "invalid" };
    if (next.some((tag) => normalizeTag(tag) === norm)) {
      return { ok: false, reason: "duplicate" };
    }
    next.push(candidate);
  }
  if (next.length > MAX_NOTE_TAGS) return { ok: false, reason: "limit" };
  const validated = normalizeTagList(next);
  if (!validated.ok) {
    return { ok: false, reason: validated.reason === "tag_limit" ? "limit" : "invalid" };
  }
  return { ok: true, tags: validated.tags };
}

/** 删除标签(按规范化名称匹配,保留显示名大小写)。 */
export function removeTag(tags: string[], raw: string): string[] {
  const norm = normalizeTag(raw);
  if (norm === null) return tags;
  return tags.filter((tag) => normalizeTag(tag) !== norm);
}
