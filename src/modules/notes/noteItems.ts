// 笔记面板条目模型(纯函数,便于单测):
// - 把「标注」与「普通笔记」归一为同一种可渲染条目;
// - 衍生标题:正文首行去掉 Markdown 标记,空则回退到类型名(高亮/便签/涂鸦…);
// - 摘要、标签、出处位置、类型筛选与排序都在客户端完成(服务端命令保持不变)。
import type { AnnotationView } from "../../shared/generated/AnnotationView";
import type { NoteView } from "../../shared/generated/NoteView";
import type { NoteOrigin } from "../../shared/generated/NoteOrigin";
import type { NoteStatus } from "../../shared/generated/NoteStatus";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import {
  INK_PREFIX,
  STYLE_UNDERLINE_PREFIX,
  decodeStyleNote,
  decodeTextBox,
} from "../reader/annotations";
import { normalizeTag } from "./bodyPolicy";

export type NoteKind = "note" | "highlight" | "underline" | "ink" | "sticky" | "plain";

export const NOTE_KIND_LABELS: Record<NoteKind, string> = {
  note: "笔记",
  highlight: "高亮",
  underline: "下划线",
  ink: "涂鸦",
  sticky: "便签",
  plain: "随笔",
};

/** 顶部筛选 chips:全部 / 笔记(含便签、随笔)/ 高亮(含下划线)/ 涂鸦。 */
export type NoteKindFilter = "all" | "note" | "highlight" | "ink";

export const NOTE_KIND_FILTERS: ReadonlyArray<{ id: NoteKindFilter; label: string }> = [
  { id: "all", label: "全部" },
  { id: "note", label: "笔记" },
  { id: "highlight", label: "高亮" },
  { id: "ink", label: "涂鸦" },
];

export type NoteSortMode = "updated" | "created";

export const NOTE_SORT_LABELS: Record<NoteSortMode, string> = {
  updated: "最近更新",
  created: "最近创建",
};

export type NoteItem = {
  /** 列表 key:标注用 id,普通笔记用 `note:<id>`(与划词聚焦保持一致)。 */
  key: string;
  kind: NoteKind;
  /** 衍生标题(去 Markdown 的正文首行;空则类型名)。 */
  title: string;
  /** 列表摘要(正文其余内容,其次引文;可为空串)。 */
  summary: string;
  /** 笔记标签(标注没有标签,恒为空数组)。 */
  tags: string[];
  /** 搜索用小写文本。 */
  searchText: string;
  createdAt: number;
  updatedAt: number;
  /** 出处位置的可读标签(第 3 页 / 章节名),无则 null。 */
  location: string | null;
  locator: SourceLocator | null;
  /** 引用原文;便签占位不算引文。 */
  quote: string | null;
  annotation: AnnotationView | null;
  note: NoteView | null;
};

export type NoteFilters = {
  kind: NoteKindFilter;
  keyword: string;
  /** 标签筛选(显示名;比较时按 normalizeTag 归一化)。 */
  tag: string | null;
  /** 组合筛选:状态/来源只作用于普通笔记(标注没有这两列)。 */
  status?: NoteStatus | null;
  origin?: NoteOrigin | null;
  /** 更新时间下限(ms);null = 不限。 */
  sinceMs?: number | null;
  sort: NoteSortMode;
};

/** 内嵌长数据块(kwink 手绘围栏 / 裸笔迹串 / data-URI 图片)折叠为可读占位符。 */
export function maskEmbeddedBlocks(source: string): string {
  return source
    .replace(/```kwink\n[\s\S]*?\n```/g, "〔手绘〕")
    .replace(/ink:v1:\S+/g, "〔手绘〕")
    .replace(/!\[[^\]]*\]\(data:[^)]+\)/g, "〔图片〕")
    .trim();
}

/** 去掉行首 Markdown 块级标记(标题/列表/任务清单/引用/有序列表)。 */
function stripBlockPrefix(line: string): string {
  let text = line.trim();
  text = text.replace(/^>+\s*/, "");
  text = text.replace(/^#{1,6}\s+/, "");
  text = text.replace(/^[-*+]\s+(?:\[[ xX]\]\s*)?/, "");
  text = text.replace(/^\d+[.)]\s+/, "");
  return text;
}

/** 去掉行内 Markdown 标记(加粗/斜体/行内代码/链接/图片/删除线)。 */
export function stripInlineMarkdown(line: string): string {
  return line
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, (_all, alt: string) => (alt.trim() === "" ? "图片" : alt))
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/(\*\*|__)(.*?)\1/g, "$2")
    .replace(/(\*|_)(.*?)\1/g, "$2")
    .replace(/~~(.*?)~~/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

function clip(text: string, maxLength: number): string {
  if (maxLength <= 0) return "";
  if (text.length <= maxLength) return text;
  return `${text.slice(0, Math.max(1, maxLength - 1))}…`;
}

/** 衍生标题:正文首行去掉 Markdown 标记;没有可用正文时回退为类型名。 */
export function deriveNoteTitle(body: string, kind: NoteKind, maxLength = 36): string {
  const masked = maskEmbeddedBlocks(body);
  let hasImage = false;
  for (const line of masked.split(/\r?\n/)) {
    const stripped = stripInlineMarkdown(stripBlockPrefix(line));
    if (stripped === "" || /^[-—_*]{3,}$/.test(stripped)) continue;
    // 纯手绘数据块没有可读文字,继续找下一行;全篇只有它时回退类型名。
    if (stripped === "〔手绘〕") continue;
    // 截图/图片行不是好标题:优先用图片之后的第一行文字。
    if (stripped === "〔图片〕") {
      hasImage = true;
      continue;
    }
    return clip(stripped, maxLength);
  }
  if (hasImage) return "截图笔记";
  return NOTE_KIND_LABELS[kind];
}

/** 列表摘要:正文标题行之后的内容,其次引用原文;没有则空串(不重复标题)。 */
export function deriveNoteSummary(
  body: string,
  _kind: NoteKind,
  quote: string | null = null,
  maxLength = 48,
): string {
  const masked = maskEmbeddedBlocks(body);
  const lines = masked
    .split(/\r?\n/)
    .map((line) => stripInlineMarkdown(stripBlockPrefix(line)))
    .filter((line) => line !== "" && !/^[-—_*]{3,}$/.test(line));
  const rest = lines.slice(1).join(" ");
  if (rest !== "") return clip(rest, maxLength);
  if (quote !== null && quote !== "" && quote !== "便签") {
    return clip(stripInlineMarkdown(quote), maxLength);
  }
  return "";
}

export function locatorLabel(locator: SourceLocator | null): string | null {
  if (locator === null) return null;
  switch (locator.kind) {
    case "pdf":
      return `第 ${locator.pageNumber} 页`;
    case "markdown":
    case "docx": {
      const heading = locator.headingPath[locator.headingPath.length - 1];
      return heading != null && heading !== "" ? heading : "正文";
    }
    case "composite": {
      for (const part of locator.parts) {
        const label = locatorLabel(part);
        if (label !== null) return label;
      }
      return null;
    }
    default:
      return null;
  }
}

/**
 * 标注类型判定:涂鸦/文本框看编码载荷;下划线看样式前缀;
 * 其余一律是「高亮(+可选备注)」——备注不应把高亮改分类成普通笔记。
 */
export function annotationKindOf(annotation: AnnotationView): NoteKind {
  if (annotation.quoteText === "便签") return "sticky";
  if (annotation.noteText.startsWith(INK_PREFIX)) return "ink";
  if (decodeTextBox(annotation.noteText) !== null) return "sticky";
  return annotation.noteText.startsWith(STYLE_UNDERLINE_PREFIX) ? "underline" : "highlight";
}

/**
 * 标注的可读正文:优先独立备注(remark);没有备注时回退历史内容
 * (高亮/下划线的旧备注、文本框文字)。涂鸦载荷永远不当正文。
 */
export function annotationBody(annotation: AnnotationView): string {
  const remark = annotation.remark?.trim() ?? "";
  if (remark !== "") return remark;
  // 笔迹/图形正文是编码载荷,不是可读文本;不要把 JSON 当备注渲染。
  if (annotation.noteText.startsWith(INK_PREFIX)) return "";
  const box = decodeTextBox(annotation.noteText);
  if (box !== null) return box.text;
  return decodeStyleNote(annotation.noteText).note;
}

/** 由标注列表与普通笔记列表构建时间倒序的条目数组。 */
export function buildNoteItems(
  annotations: AnnotationView[],
  notes: NoteView[],
): NoteItem[] {
  const fromAnnotations: NoteItem[] = annotations.map((annotation) => {
    const kind = annotationKindOf(annotation);
    const body = annotationBody(annotation);
    const title = deriveNoteTitle(body, kind);
    const summary = deriveNoteSummary(body, kind, annotation.quoteText);
    const location = locatorLabel(annotation.locator);
    const quote =
      annotation.quoteText !== "" && annotation.quoteText !== "便签"
        ? annotation.quoteText
        : null;
    return {
      key: annotation.id,
      kind,
      title,
      summary,
      tags: [],
      searchText: [title, body, annotation.remark ?? "", annotation.quoteText, location ?? ""]
        .join(" ")
        .toLowerCase(),
      createdAt: annotation.createdAt,
      updatedAt: annotation.updatedAt,
      location,
      locator: annotation.locator,
      quote,
      annotation,
      note: null,
    };
  });
  const fromNotes: NoteItem[] = notes.map((note) => {
    const kind: NoteKind = "plain";
    const title = deriveNoteTitle(note.body, kind);
    const summary = deriveNoteSummary(note.body, kind, null);
    const locator = note.locator ?? null;
    const location = locatorLabel(locator);
    return {
      key: `note:${note.id}`,
      kind,
      title,
      summary,
      tags: [...note.tags],
      searchText: [title, note.body, note.tags.join(" "), location ?? ""]
        .join(" ")
        .toLowerCase(),
      createdAt: note.createdAt,
      updatedAt: note.updatedAt,
      location,
      locator,
      quote: null,
      annotation: null,
      note,
    };
  });
  return sortNoteItems([...fromAnnotations, ...fromNotes], "updated");
}

/** 类型筛选:笔记=笔记/便签/随笔;高亮=高亮/下划线;涂鸦单独一类。 */
export function noteMatchesKind(kind: NoteKind, filter: NoteKindFilter): boolean {
  if (filter === "all") return true;
  if (filter === "note") return kind === "note" || kind === "sticky" || kind === "plain";
  if (filter === "highlight") return kind === "highlight" || kind === "underline";
  return kind === "ink";
}

/** 排序:updated=最近更新;created=最近创建;时间相同时用 key 稳定排序。 */
export function sortNoteItems(items: NoteItem[], mode: NoteSortMode): NoteItem[] {
  const primary = mode === "created" ? "createdAt" : "updatedAt";
  const secondary = primary === "updatedAt" ? "createdAt" : "updatedAt";
  return [...items].sort((a, b) => {
    if (b[primary] !== a[primary]) return b[primary] - a[primary];
    if (b[secondary] !== a[secondary]) return b[secondary] - a[secondary];
    return a.key.localeCompare(b.key);
  });
}

/** 按类型/关键词/标签过滤并排序(纯函数,测试锚点见 noteItems.test.ts)。 */
export function filterNoteItems(items: NoteItem[], filters: NoteFilters): NoteItem[] {
  const keyword = filters.keyword.trim().toLowerCase();
  const tagNorm = filters.tag === null ? null : normalizeTag(filters.tag);
  const filtered = items.filter((item) => {
    if (!noteMatchesKind(item.kind, filters.kind)) return false;
    if (keyword !== "" && !item.searchText.includes(keyword)) return false;
    if (tagNorm !== null) {
      if (!item.tags.some((tag) => normalizeTag(tag) === tagNorm)) return false;
    }
    if (filters.status != null) {
      if (item.note === null || item.note.status !== filters.status) return false;
    }
    if (filters.origin != null) {
      if (item.note === null || item.note.origin !== filters.origin) return false;
    }
    if (filters.sinceMs != null && item.updatedAt < filters.sinceMs) return false;
    return true;
  });
  return sortNoteItems(filtered, filters.sort);
}

/** 全部标签(显示名去重,中文排序),用于标签筛选下拉。 */
export function collectTags(items: NoteItem[]): string[] {
  const byNorm = new Map<string, string>();
  for (const item of items) {
    for (const tag of item.tags) {
      const norm = normalizeTag(tag);
      if (norm !== null && !byNorm.has(norm)) byNorm.set(norm, tag);
    }
  }
  return [...byNorm.values()].sort((a, b) => a.localeCompare(b, "zh-CN"));
}
