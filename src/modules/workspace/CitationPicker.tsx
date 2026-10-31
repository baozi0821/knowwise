// 聊天框「引用资料」选择器:从产物中心/资料库挑已有资料作为本轮问答来源。
//
// 设计要点(《M07》§4 + 本轮 P0「聊天引用资料」):
// - 选中的资料以 artifact_version 来源进入冻结快照,正文真正进入 Prompt;
// - 只提交 versionId,不提交正文(服务端按版本快照文本,防篡改);
// - 显示已引用资料 chip,可逐个移除;
// - 删除/升版后的失效由后端 source_available 与服务端解析共同表达。
import { useMemo, useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { BookMarked, Check, X } from "lucide-react";
import type { ArtifactListItem } from "../../shared/generated/ArtifactListItem";
import type { ArtifactView } from "../../shared/generated/ArtifactView";
import type { ScopeDraft } from "../../shared/generated/ScopeDraft";
import { useServices } from "../../app/services";
import { kindLabel } from "../artifacts/filters";
import "./citationPicker.css";

/** 一条已引用资料(展示用投影;正文由后端按版本读取)。 */
export type CitedArtifact = {
  /** 产物 id;笔记来源为空字符串。 */
  artifactId: string;
  /** 资料来源 id:产物为 versionId,笔记为 noteId。 */
  versionId: string;
  title: string;
  /** 产物 kind 或 "note"(笔记作为来源)。 */
  kind: string;
  versionNo?: number;
  /** 来源类型:默认 artifact_version;笔记用 note。 */
  sourceType?: "artifact_version" | "note";
};

/** 把已引用资料并入范围草案:有资料时用 sources,否则沿用原范围。 */
export function mergeCitedArtifacts(
  base: ScopeDraft,
  cited: readonly CitedArtifact[],
): ScopeDraft {
  if (cited.length === 0) return base;
  const cloned = structuredCloneScope(base);
  // 已有 sources 范围:追加资料来源(去重由服务端完成)。
  if (cloned.kind === "sources") {
    return {
      kind: "sources",
      sources: [
        ...cloned.sources,
        ...cited.map((item) => ({
          sourceType: (item.sourceType ?? "artifact_version") as
            | "artifact_version"
            | "note",
          sourceId: item.versionId,
        })),
      ],
    };
  }
  // 其他范围(页/节/全文)不能与 sources 混用:资料优先,
  // 由 UI 明确提示"本轮只用所选资料"。
  return {
    kind: "sources",
    sources: cited.map((item) => ({
      sourceType: (item.sourceType ?? "artifact_version") as
        | "artifact_version"
        | "note",
      sourceId: item.versionId,
    })),
  };
}

/** 深拷贝范围草案(避免修改调用方状态)。 */
function structuredCloneScope(scope: ScopeDraft): ScopeDraft {
  return JSON.parse(JSON.stringify(scope)) as ScopeDraft;
}

export function CitedArtifactChips({
  cited,
  onRemove,
}: {
  cited: readonly CitedArtifact[];
  onRemove: (versionId: string) => void;
}): ReactNode {
  if (cited.length === 0) return null;
  return (
    <div className="cited-chips" aria-label="已引用资料">
      {cited.map((item) => (
        <span key={item.versionId} className="cited-chip">
          <BookMarked className="w-3 h-3" aria-hidden="true" />
          <span className="cited-chip-label">
            {kindLabel(item.kind)}·{item.title}
            {item.versionNo !== undefined ? ` · v${item.versionNo}` : ""}
          </span>
          <button
            type="button"
            className="cited-chip-remove"
            aria-label={`移除引用:${item.title}`}
            title="移除引用"
            onClick={() => onRemove(item.versionId)}
          >
            <X className="w-3 h-3" aria-hidden="true" />
          </button>
        </span>
      ))}
    </div>
  );
}

export function CitationPicker({
  documentId,
  cited,
  onToggle,
  onClose,
}: {
  documentId: string | null;
  cited: readonly CitedArtifact[];
  onToggle: (item: CitedArtifact) => void;
  onClose: () => void;
}): ReactNode {
  const { client } = useServices();
  const [scope, setScope] = useState<"current" | "all">(documentId ? "current" : "all");
  const [query, setQuery] = useState("");
  const effectiveDocId = scope === "current" ? (documentId ?? "") : "";
  const citedVersions = useMemo(
    () => new Set(cited.map((item) => item.versionId)),
    [cited],
  );

  const list = useQuery({
    queryKey: ["artifacts", "citation-picker", effectiveDocId || "all-library"],
    queryFn: () =>
      client.callFlat<{ items: ArtifactListItem[] }>("artifact_list", {
        documentId: effectiveDocId,
      }),
  });

  const items = (list.data?.items ?? []).filter((item) => {
    const q = query.trim().toLowerCase();
    return q === "" || item.title.toLowerCase().includes(q);
  });

  async function pick(item: ArtifactListItem): Promise<void> {
    // 列表只有 currentVersionId;点击时取详情拿到完整版本信息(标题/版本号)。
    if (item.currentVersionId === undefined) return;
    try {
      const view = await client.callFlat<ArtifactView>("artifact_get", {
        artifactId: item.id,
      });
      const versionId = view.currentVersionId ?? item.currentVersionId;
      onToggle({
        artifactId: view.id,
        versionId,
        title: view.title,
        kind: view.kind,
        versionNo:
          view.currentVersion?.versionNo ??
          (item.currentVersionNo !== undefined ? Number(item.currentVersionNo) : undefined),
      });
    } catch {
      // 详情失败时不静默:用列表信息降级选择(正文仍由后端按版本读取)。
      onToggle({
        artifactId: item.id,
        versionId: item.currentVersionId,
        title: item.title,
        kind: item.kind,
        versionNo:
          item.currentVersionNo !== undefined ? Number(item.currentVersionNo) : undefined,
      });
    }
  }

  return (
    <div className="citation-picker" role="dialog" aria-label="选择要引用的资料">
      <header className="citation-picker-head">
        <strong>引用资料</strong>
        <div className="citation-picker-scope" role="group" aria-label="资料范围">
          <button
            type="button"
            className={`btn tiny${scope === "current" ? " primary" : " ghost"}`}
            onClick={() => setScope("current")}
            disabled={documentId === null}
          >
            当前文档
          </button>
          <button
            type="button"
            className={`btn tiny${scope === "all" ? " primary" : " ghost"}`}
            onClick={() => setScope("all")}
          >
            全部资料
          </button>
        </div>
        <button
          type="button"
          className="btn tiny ghost"
          onClick={onClose}
          aria-label="关闭资料选择"
        >
          <X className="w-3 h-3" aria-hidden="true" />
          <span>关闭</span>
        </button>
      </header>
      <label className="citation-picker-search">
        <input
          type="search"
          value={query}
          placeholder="搜索资料标题"
          aria-label="搜索资料标题"
          onChange={(event) => setQuery(event.target.value)}
        />
      </label>
      {list.isLoading ? <p role="status">资料加载中…</p> : null}
      {list.isError ? (
        <p className="art-error" role="alert">
          资料加载失败。
        </p>
      ) : null}
      {list.data && list.data.items.length === 0 ? (
        <p className="citation-picker-empty">还没有可引用的资料;先生成摘要或题目。</p>
      ) : null}
      {list.data && list.data.items.length > 0 && items.length === 0 ? (
        <p className="citation-picker-empty">没有匹配的资料。</p>
      ) : null}
      <ul className="citation-picker-list">
        {items.map((item) => {
          const selected =
            item.currentVersionId !== undefined && citedVersions.has(item.currentVersionId);
          return (
            <li key={item.id}>
              <button
                type="button"
                className={`citation-picker-item${selected ? " selected" : ""}`}
                aria-pressed={selected}
                onClick={() => void pick(item)}
              >
                <span className="citation-picker-kind">{kindLabel(item.kind)}</span>
                <span className="citation-picker-title">{item.title}</span>
                {selected ? <Check className="w-3.5 h-3.5" aria-hidden="true" /> : null}
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
