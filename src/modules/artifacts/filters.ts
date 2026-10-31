// M10 产物中心筛选(《M10》§9):按文档、类型、时间和标题筛选;
// 与后端 artifact_list 的服务端分页互补,此处在已加载页上做即时过滤。
import type { ArtifactListItem } from "../../shared/generated/ArtifactListItem";

export type ArtifactFilter = {
  kind?: string;
  /** 标题子串(大小写不敏感)。 */
  query?: string;
  /** 截止时间(ms,含);不填为全部。 */
  updatedUntil?: number;
};

export function filterArtifacts(
  items: readonly ArtifactListItem[],
  filter: ArtifactFilter,
): ArtifactListItem[] {
  const q = filter.query?.trim().toLowerCase();
  return items.filter((it) => {
    if (filter.kind !== undefined && it.kind !== filter.kind) return false;
    if (q !== undefined && q !== "" && !it.title.toLowerCase().includes(q)) {
      return false;
    }
    if (filter.updatedUntil !== undefined && it.updatedAt > filter.updatedUntil) {
      return false;
    }
    return true;
  });
}

/** 产物类型显示名。 */
export const KIND_LABELS: Record<string, string> = {
  summary: "摘要",
  quiz: "测验",
  graph: "论证图",
};

export function kindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind;
}

/** 覆盖提示:partial 不宣称完整(§5);unknown 显示"未知"。 */
export function coverageHint(complete: boolean | undefined): string {
  if (complete === undefined) return "覆盖未知";
  return complete ? "覆盖完整" : "部分覆盖(有缺失)";
}
