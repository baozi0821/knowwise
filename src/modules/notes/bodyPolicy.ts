// M06 笔记正文与标签策略(《M06》§3/§5)。
// 与 Rust 服务端(note_save 校验)保持一致:长度上限拒绝而非截断,
// 安全 Markdown 子集禁用脚本与远程执行协议;标签规范化唯一。
export const MAX_NOTE_BODY_CHARS = 100_000;
export const MAX_NOTE_TAGS = 20;
export const MAX_TAG_CHARS = 64;

export type BodyPolicyResult =
  | { ok: true }
  | { ok: false; reason: "too_long" | "unsafe"; maxChars?: number };

/** 安全校验:服务端同款禁用项(渲染端另有 DOMPurify 兜底)。 */
const BANNED_SUBSTRINGS = ["<script", "javascript:", "data:text/html"];

export function validateNoteBody(body: string): BodyPolicyResult {
  if (body.length > MAX_NOTE_BODY_CHARS) {
    return { ok: false, reason: "too_long", maxChars: MAX_NOTE_BODY_CHARS };
  }
  const lower = body.toLowerCase();
  for (const banned of BANNED_SUBSTRINGS) {
    if (lower.includes(banned)) {
      return { ok: false, reason: "unsafe" };
    }
  }
  return { ok: true };
}

/** UI 剩余字数(不静默截断,超限时显示负数提示)。 */
export function remainingChars(body: string): number {
  return MAX_NOTE_BODY_CHARS - body.length;
}

/** 标签规范化:折叠空白 + 小写;显示名保留大小写由调用方处理。 */
export function normalizeTag(name: string): string | null {
  const collapsed = name.split(/\s+/).filter((s) => s.length > 0).join(" ");
  if (collapsed.length === 0 || collapsed.length > MAX_TAG_CHARS) {
    return null;
  }
  return collapsed.toLowerCase();
}

/** 标签集合校验:数量、规范化唯一;返回规范化后的显示名列表。 */
export function normalizeTagList(
  tags: string[],
): { ok: true; tags: string[] } | { ok: false; reason: "tag_invalid" | "tag_limit" } {
  if (tags.length > MAX_NOTE_TAGS) {
    return { ok: false, reason: "tag_limit" };
  }
  const seen = new Set<string>();
  const out: string[] = [];
  for (const t of tags) {
    const norm = normalizeTag(t);
    if (norm === null) return { ok: false, reason: "tag_invalid" };
    if (!seen.has(norm)) {
      seen.add(norm);
      out.push(t.trim().replace(/\s+/g, " "));
    }
  }
  return { ok: true, tags: out };
}

/** 保存按钮可用性:策略通过且有变化才允许提交。 */
export function canSubmit(body: string, tags: string[], dirty: boolean): boolean {
  return dirty && validateNoteBody(body).ok && normalizeTagList(tags).ok;
}
