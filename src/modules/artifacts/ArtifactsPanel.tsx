// M10–M12 拥有:产物中心面板,只负责查看/阅览/版本/导出/删除。
// 列表(artifact_list,平铺参数)→ 详情(artifact_get)→ 按 content.schemaVersion
// 分发渲染:summary.v1 摘要(可导出 Markdown/docx)、quiz.v1 校验题(可作答,
// 经 quiz_submit_answer)、graph.v1 论证图(节点/边清单)、translation.v1 译文;
// 未知 kind 走既有兜底不丢数据。生成与重新生成入口统一在 AI 工具栏。
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import {
  BookOpen,
  ChevronDown,
  Download,
  Search,
  Trash2,
} from "lucide-react";
import type { ArtifactExportPreview } from "../../shared/generated/ArtifactExportPreview";
import type { ArtifactListItem } from "../../shared/generated/ArtifactListItem";
import type { ArtifactVersionView } from "../../shared/generated/ArtifactVersionView";
import type { ArtifactView } from "../../shared/generated/ArtifactView";
import type { ArtifactDeletePreview } from "../../shared/generated/ArtifactDeletePreview";
import type { ComprehensionRecordView } from "../../shared/generated/ComprehensionRecordView";
import type { ExportResult } from "../../shared/generated/ExportResult";
import type { GraphContent } from "../../shared/generated/GraphContent";
import type { QuizContentV1 } from "../../shared/generated/QuizContentV1";
import type { QuizContentV2 } from "../../shared/generated/QuizContentV2";
import { QuizV2View } from "../quiz/QuizV2View";
import { TranslationView } from "../translation/TranslationView";
import type { TranslationV1 } from "../../shared/generated/TranslationV1";
import type { SummaryContentV1 } from "../../shared/generated/SummaryContentV1";
import type { CommandClient } from "../../app/api/client";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import { useServices } from "../../app/services";
import { useWorkspaceUi } from "../workspace/uiStore";
import { clipboardWriteText, dialogSavePath } from "../../app/ipc/transport";
import { UnknownArtifact } from "../workspace/artifactRenderers";
import { ArtifactReader } from "./ArtifactReader";
import { SEGMENT_KIND_LABELS } from "../qa/presentation";
import { filterArtifacts, kindLabel } from "./filters";
import type { SummarySegmentKind } from "../../shared/generated/SummarySegmentKind";
import "./artifacts.css";

const ORIGIN_LABELS: Record<string, string> = {
  ai_draft: "AI 草稿",
  user_edit: "用户编辑",
  copy: "副本",
  restore: "恢复版本",
};

const STATE_LABELS: Record<string, string> = {
  mutable: "可编辑",
  sealed: "已封存",
};

/** summary.v1 专属段类型标注(与 answer.v1 的 SegmentKind 不同源)。 */
const SUMMARY_SEGMENT_LABELS: Record<SummarySegmentKind, string> = {
  document_fact: "原文事实",
  ai_inference: "AI 推断",
  user_view: "用户观点",
  needs_verification: "待核验",
};

function SummaryView({ content }: { content: SummaryContentV1 }) {
  return (
    <div className="art-summary">
      {content.sections.map((section) => (
        <section key={section.id} className="art-section">
          <h4>{section.title}</h4>
          {section.segments.map((segment) => (
            <p key={segment.id} className="art-segment">
              {segment.kind !== "document_fact" ? (
                <span className="art-badge">{SUMMARY_SEGMENT_LABELS[segment.kind]}</span>
              ) : null}
              {segment.text}
            </p>
          ))}
        </section>
      ))}
      {content.openQuestions.length > 0 ? (
        <section className="art-section">
          <h4>待澄清问题</h4>
          <ul>
            {content.openQuestions.map((q, i) => (
              <li key={i}>{q}</li>
            ))}
          </ul>
        </section>
      ) : null}
    </div>
  );
}

type ExportFormat = "markdown" | "docx";

const EXPORT_FORMATS: Record<
  ExportFormat,
  { label: string; extension: string; dialogTitle: string }
> = {
  markdown: {
    label: "Markdown",
    extension: "md",
    dialogTitle: "导出摘要(Markdown)",
  },
  docx: {
    label: "docx",
    extension: "docx",
    dialogTitle: "导出摘要(docx)",
  },
};

/** 默认文件名只取标题,清掉 Windows 非法字符;不携带任何本机路径。 */
export function exportDefaultFileName(title: string, format: ExportFormat): string {
  const noControls = Array.from(title, (char) =>
    char.charCodeAt(0) < 32 ? "-" : char,
  ).join("");
  const cleaned = noControls
    .replace(/[<>:"/\\|?*]/g, "-")
    .replace(/\s+/g, " ")
    .replace(/[. ]+$/g, "")
    .trim()
    .slice(0, 80);
  return `${cleaned || "摘要"}.${EXPORT_FORMATS[format].extension}`;
}

function presentExportError(caught: unknown, fallback: string): string {
  if (caught instanceof ApiError) {
    const presentation = describeAppError(caught.appError);
    return (
      presentation.title + (presentation.actionLabel ? `(${presentation.actionLabel})` : "")
    );
  }
  return fallback;
}

function SummaryExportControls({
  artifactId,
  versionId,
  title,
  coverageComplete,
  label = "摘要",
  showVariant = false,
}: {
  artifactId: string;
  versionId: string;
  title: string;
  coverageComplete: boolean;
  label?: string;
  showVariant?: boolean;
}) {
  const { client } = useServices();
  const [menuOpen, setMenuOpen] = useState(false);
  const [pendingFormat, setPendingFormat] = useState<ExportFormat | null>(null);
  const [preview, setPreview] = useState<ArtifactExportPreview | null>(null);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [exportVariant, setExportVariant] = useState<"bilingual" | "translation_only">(
    "bilingual",
  );

  function resetPreview() {
    setPreview(null);
    setPendingFormat(null);
  }

  async function startPreview(format: ExportFormat) {
    setMenuOpen(false);
    resetPreview();
    setStatus(null);
    setError(null);
    setBusy(true);
    try {
      // 现有预览命令固定返回 Markdown 等价文本;docx 复用同一内容检查。
      const next = await client.callFlat<ArtifactExportPreview>("artifact_export_preview", {
        artifactId,
        versionId,
        variant: showVariant ? exportVariant : undefined,
      });
      setPreview(next);
      setPendingFormat(format);
    } catch (caught) {
      setError(presentExportError(caught, "导出预览失败,请重试"));
    } finally {
      setBusy(false);
    }
  }

  async function confirmExport() {
    if (pendingFormat === null || preview === null || busy) return;
    const meta = EXPORT_FORMATS[pendingFormat];
    setBusy(true);
    setError(null);
    setStatus(null);
    try {
      const path = await dialogSavePath({
        title: meta.dialogTitle,
        defaultPath: exportDefaultFileName(title, pendingFormat),
        filters: [{ name: meta.label, extensions: [meta.extension] }],
      });
      if (path === null) {
        setStatus("已取消导出");
        resetPreview();
        return;
      }
      const result = await client.call<ExportResult>("artifact_export", {
        artifactId,
        versionId,
        format: pendingFormat,
        path,
        variant: showVariant ? exportVariant : undefined,
        idempotencyKey: client.newIdempotencyKey(),
      });
      setStatus(`已导出 ${meta.label}(${result.bytesWritten} 字节)`);
      resetPreview();
    } catch (caught) {
      setError(presentExportError(caught, "导出失败,请重试"));
    } finally {
      setBusy(false);
    }
  }

  const previewText =
    preview === null
      ? ""
      : preview.markdown.length > 1200
        ? `${preview.markdown.slice(0, 1200)}\n\n…(仅显示前 1200 字,实际导出内容完整)`
        : preview.markdown;

  return (
    <div className="art-export" role="region" aria-label={`${label}导出`} aria-busy={busy}>
      <div className="art-export-head">
        <span className="art-export-title">导出{label}</span>
        {showVariant ? (
          <label className="art-export-variant">
            版本
            <select
              value={exportVariant}
              onChange={(event) =>
                setExportVariant(
                  event.target.value === "translation_only" ? "translation_only" : "bilingual",
                )
              }
              aria-label="导出翻译版本"
            >
              <option value="bilingual">双语</option>
              <option value="translation_only">仅译文</option>
            </select>
          </label>
        ) : null}
        <div className="art-export-menu-wrap">
          <button
            type="button"
            className="btn small"
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            aria-controls="art-export-menu"
            disabled={busy}
            onClick={() => {
              setMenuOpen((open) => !open);
              setError(null);
            }}
          >
            <Download className="w-3.5 h-3.5" aria-hidden />
            <span>导出</span>
            <ChevronDown className="w-3.5 h-3.5" aria-hidden />
          </button>
          {menuOpen ? (
            <div
              id="art-export-menu"
              className="art-export-menu"
              role="menu"
              aria-label="导出格式"
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.stopPropagation();
                  setMenuOpen(false);
                }
              }}
            >
              {(Object.keys(EXPORT_FORMATS) as ExportFormat[]).map((format) => (
                <button
                  key={format}
                  type="button"
                  role="menuitem"
                  className="art-export-menu-item"
                  onClick={() => void startPreview(format)}
                >
                  导出 {EXPORT_FORMATS[format].label}(.{EXPORT_FORMATS[format].extension})
                </button>
              ))}
            </div>
          ) : null}
        </div>
      </div>

      {preview !== null && pendingFormat !== null ? (
        <section className="art-export-preview" aria-label="导出预览">
          <p className="art-export-hint">
            即将导出为 {EXPORT_FORMATS[pendingFormat].label};以下为内容等价预览。
            {pendingFormat === "docx" ? "docx 保留来源标签与引用附录。" : ""}
          </p>
          {!coverageComplete ? (
            <p className="art-export-warning" role="status">
              覆盖不完整:导出文档会标明“部分覆盖,有缺失”,不会声称全文。
            </p>
          ) : null}
          <pre
            className="art-export-preview-text"
            tabIndex={0}
            aria-label={`${EXPORT_FORMATS[pendingFormat].label} 导出内容预览`}
          >
            {previewText}
          </pre>
          <div className="art-export-actions">
            <button
              type="button"
              className="btn small primary"
              disabled={busy}
              onClick={() => void confirmExport()}
            >
              {busy ? "导出中…" : "选择保存位置并导出"}
            </button>
            <button
              type="button"
              className="btn small ghost"
              disabled={busy}
              onClick={() => {
                resetPreview();
                setStatus("已取消导出");
              }}
            >
              取消
            </button>
          </div>
        </section>
      ) : null}

      {status !== null ? (
        <p className="art-export-status" role="status">
          {status}
        </p>
      ) : null}
      {error !== null ? (
        <p className="art-error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}

function QuizQuestionView({
  client,
  artifactVersionId,
  question,
}: {
  client: CommandClient;
  artifactVersionId: string;
  question: QuizContentV1["questions"][number];
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const [result, setResult] = useState<ComprehensionRecordView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  async function submit() {
    if (selected === null) return;
    setSubmitting(true);
    setError(null);
    try {
      const record = await client.call<ComprehensionRecordView>("quiz_submit_answer", {
        artifactVersionId,
        questionId: question.id,
        selectedOptionId: selected,
        idempotencyKey: client.newIdempotencyKey(),
      });
      setResult(record);
    } catch {
      setError("提交失败,请重试");
    } finally {
      setSubmitting(false);
    }
  }

  if (question.validity === "invalid") {
    return (
      <li className="art-quiz-question invalid">
        <p className="art-quiz-prompt">{question.prompt}</p>
        <p className="art-badge-line">该题被判为无效({question.invalidReason ?? "未知原因"}),不作答。</p>
      </li>
    );
  }

  return (
    <li className="art-quiz-question">
      <p className="art-quiz-prompt">{question.prompt}</p>
      <fieldset>
        <legend className="visually-hidden">选项</legend>
        {question.options.map((option) => (
          <label key={option.id} className="art-quiz-option">
            <input
              type="radio"
              name={`q-${question.id}`}
              value={option.id}
              checked={selected === option.id}
              disabled={result !== null}
              onChange={() => setSelected(option.id)}
            />{" "}
            {option.text}
          </label>
        ))}
      </fieldset>
      {result === null ? (
        <button type="button" onClick={() => void submit()} disabled={selected === null || submitting}>
          {submitting ? "提交中…" : "提交答案"}
        </button>
      ) : (
        <div role="status" className="art-quiz-result">
          <p>{result.isCorrect ? "回答正确" : "回答错误"}</p>
          <h5>解析</h5>
          {question.explanationSegments.map((segment) => (
            <p key={segment.id} className="art-segment">
              {segment.kind !== "document_fact" ? (
                <span className="art-badge">{SEGMENT_KIND_LABELS[segment.kind] ?? segment.kind}</span>
              ) : null}
              {segment.text}
            </p>
          ))}
        </div>
      )}
      {error ? (
        <p role="alert" className="art-error">
          {error}
        </p>
      ) : null}
    </li>
  );
}

function QuizView({
  client,
  artifactVersionId,
  content,
}: {
  client: CommandClient;
  artifactVersionId: string;
  content: QuizContentV1;
}) {
  return (
    <ol className="art-quiz">
      {content.questions.map((question) => (
        <QuizQuestionView
          key={question.id}
          client={client}
          artifactVersionId={artifactVersionId}
          question={question}
        />
      ))}
    </ol>
  );
}

function VisualGraphCanvas({
  nodes,
  edges,
}: {
  nodes: GraphContent["nodes"];
  edges: GraphContent["edges"];
}) {
  if (nodes.length === 0) return null;
  const cols = Math.min(3, Math.max(1, nodes.length));
  const nodeWidth = 130;
  const nodeHeight = 50;
  const gapX = 70;
  const gapY = 60;
  const padding = 16;

  const nodePositions = new Map<string, { x: number; y: number }>();
  nodes.forEach((n, idx) => {
    const col = idx % cols;
    const row = Math.floor(idx / cols);
    const x = padding + col * (nodeWidth + gapX);
    const y = padding + row * (nodeHeight + gapY);
    nodePositions.set(n.id, { x, y });
  });

  const totalWidth = padding * 2 + cols * nodeWidth + (cols - 1) * gapX;
  const totalHeight = padding * 2 + Math.ceil(nodes.length / cols) * (nodeHeight + gapY);

  return (
    <div className="overflow-x-auto my-3 bg-slate-50 p-3 rounded-lg border border-slate-200">
      <svg width={totalWidth} height={totalHeight} className="mx-auto block">
        <defs>
          <marker
            id="graph-arrow"
            viewBox="0 0 10 10"
            refX="6"
            refY="5"
            markerWidth="6"
            markerHeight="6"
            orient="auto-start-reverse"
          >
            <path d="M 0 1 L 8 5 L 0 9 z" fill="#6366f1" />
          </marker>
        </defs>
        {edges.map((e) => {
          const from = nodePositions.get(e.source);
          const to = nodePositions.get(e.target);
          if (!from || !to) return null;
          const x1 = from.x + nodeWidth / 2;
          const y1 = from.y + nodeHeight / 2;
          const x2 = to.x + nodeWidth / 2;
          const y2 = to.y + nodeHeight / 2;
          const midX = (x1 + x2) / 2;
          const midY = (y1 + y2) / 2;
          return (
            <g key={e.id}>
              <line
                x1={x1}
                y1={y1}
                x2={x2}
                y2={y2}
                stroke="#818cf8"
                strokeWidth="2"
                strokeDasharray={e.certainty === "uncertain" ? "4 4" : undefined}
                markerEnd="url(#graph-arrow)"
              />
              {e.label && (
                <text
                  x={midX}
                  y={midY - 4}
                  fill="#475569"
                  fontSize="10"
                  textAnchor="middle"
                  className="select-none font-medium"
                >
                  {e.label}
                </text>
              )}
            </g>
          );
        })}
        {nodes.map((n) => {
          const pos = nodePositions.get(n.id);
          if (!pos) return null;
          const isSupported = n.evidenceStatus === "supported";
          const isConflict = n.evidenceStatus === "conflict";
          const borderColor = isSupported ? "#22c55e" : isConflict ? "#ef4444" : "#cbd5e1";
          const bgColor = isSupported ? "#f0fdf4" : isConflict ? "#fef2f2" : "#ffffff";
          return (
            <g key={n.id} transform={`translate(${pos.x}, ${pos.y})`}>
              <rect
                width={nodeWidth}
                height={nodeHeight}
                rx="8"
                fill={bgColor}
                stroke={borderColor}
                strokeWidth="1.5"
                className="drop-shadow-sm"
              />
              <text
                x={nodeWidth / 2}
                y={nodeHeight / 2 - (n.description ? 4 : 0)}
                fill="#0f172a"
                fontSize="11"
                fontWeight="600"
                textAnchor="middle"
                dominantBaseline="middle"
              >
                {n.label.length > 9 ? `${n.label.slice(0, 8)}…` : n.label}
              </text>
              {n.description && (
                <text
                  x={nodeWidth / 2}
                  y={nodeHeight / 2 + 10}
                  fill="#64748b"
                  fontSize="9"
                  textAnchor="middle"
                  dominantBaseline="middle"
                >
                  {n.description.length > 11 ? `${n.description.slice(0, 10)}…` : n.description}
                </text>
              )}
            </g>
          );
        })}
      </svg>
    </div>
  );
}

function GraphView({ content }: { content: GraphContent }) {
  return (
    <div className="art-graph">
      <p className="art-badge-line">
        {content.title} · {content.nodes.length} 节点 / {content.edges.length} 边
      </p>
      <VisualGraphCanvas nodes={content.nodes} edges={content.edges} />
      <h4>节点</h4>
      <ul>
        {content.nodes.map((node) => (
          <li key={node.id}>
            <strong>{node.label}</strong>
            {node.description ? `:${node.description}` : ""}
            <span className="art-badge-line">
              {" "}
              · 证据:{node.evidenceStatus}
              {node.citations.length > 0 ? ` ×${node.citations.length}` : ""}
            </span>
          </li>
        ))}
      </ul>
      <h4>边</h4>
      <ul>
        {content.edges.map((edge) => (
          <li key={edge.id}>
            {edge.label ? `${edge.label}:` : ""} {edge.source} → {edge.target}
            <span className="art-badge-line">
              {" "}
              · 确定性:{edge.certainty} · 证据:{edge.evidenceStatus}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function ArtifactDetail({
  artifact,
  version,
  onDeleted,
}: {
  artifact: ArtifactView;
  version: ArtifactVersionView;
  onDeleted: () => void;
}) {
  const { client } = useServices();
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [previewing, setPreviewing] = useState(false);
  const [deletePreview, setDeletePreview] = useState<ArtifactDeletePreview | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  /** 删除前先取真实影响面(版本/引用/导出记录),不靠猜测。 */
  async function requestDelete(): Promise<void> {
    setPreviewing(true);
    setDeleteError(null);
    try {
      const preview = await client.callFlat<ArtifactDeletePreview>("artifact_delete_preview", {
        artifactId: artifact.id,
      });
      setDeletePreview(preview);
      setConfirmDelete(true);
    } catch (caught) {
      setDeleteError(
        caught instanceof ApiError
          ? describeAppError(caught.appError).title
          : "无法获取删除影响面,未执行删除",
      );
    } finally {
      setPreviewing(false);
    }
  }

  async function handleDeleteArtifact() {
    setDeleting(true);
    setDeleteError(null);
    try {
      await client.call("artifact_delete", {
        artifactId: artifact.id,
        expectedRevision: artifact.revision,
      });
      onDeleted();
    } catch (caught) {
      if (caught instanceof ApiError) {
        const pres = describeAppError(caught.appError);
        setDeleteError(pres.title);
      } else {
        setDeleteError("删除产物失败，请重试");
      }
    } finally {
      setDeleting(false);
      setConfirmDelete(false);
    }
  }
  const content = version.content as { schemaVersion?: string };
  const body = (() => {
    switch (content?.schemaVersion) {
      case "summary.v1":
        return (
          <div className="art-summary-detail">
            <SummaryExportControls
              artifactId={artifact.id}
              versionId={version.id}
              title={artifact.title}
              coverageComplete={version.coverage.complete !== false}
            />
            <SummaryView content={content as SummaryContentV1} />
          </div>
        );
      case "quiz.v1":
        return (
          <QuizView
            client={client}
            artifactVersionId={version.id}
            content={content as QuizContentV1}
          />
        );
      case "quiz.v2":
        return (
          <div className="art-quiz-detail">
            <SummaryExportControls
              artifactId={artifact.id}
              versionId={version.id}
              title={artifact.title}
              coverageComplete={version.coverage.complete !== false}
              label="题目"
            />
            <QuizV2View
              client={client}
              artifactVersionId={version.id}
              content={content as QuizContentV2}
            />
          </div>
        );
      case "graph.v1":
        return <GraphView content={content as GraphContent} />;
      case "translation.v1":
        return (
          <div className="art-translation-detail">
            <SummaryExportControls
              artifactId={artifact.id}
              versionId={version.id}
              title={artifact.title}
              coverageComplete={version.coverage.complete !== false}
              label="译文"
              showVariant
            />
            <TranslationView client={client} content={content as TranslationV1} />
          </div>
        );
      default:
        return (
          <UnknownArtifact
            kind={artifact.kind}
            contentJson={JSON.stringify(version.content, null, 2)}
            writeClipboard={clipboardWriteText}
          />
        );
    }
  })();
  return (
    <>
      <div className="art-detail-toolbar flex items-center justify-end mb-2">
        <div className="art-actions-bar">
          {confirmDelete ? (
            <div className="art-confirm-box" role="alertdialog" aria-label="确认删除产物">
              <span className="text-xs text-red-600 dark:text-red-400">
                {deletePreview === null
                  ? "正在获取删除影响面…"
                  : `删除“${artifact.title}”:${deletePreview.versionCount} 个版本、${deletePreview.citationCount} 处引用、${deletePreview.exportRecordCount} 条导出记录${
                      deletePreview.clarificationRefCount > 0
                        ? `、${deletePreview.clarificationRefCount} 个待澄清项引用`
                        : ""
                    }?`}
              </span>
              <button
                type="button"
                className="btn tiny danger"
                disabled={deleting || deletePreview === null}
                onClick={() => void handleDeleteArtifact()}
              >
                {deleting ? "删除中…" : "确认删除"}
              </button>
              <button
                type="button"
                className="btn tiny ghost"
                disabled={deleting}
                onClick={() => {
                  setConfirmDelete(false);
                  setDeletePreview(null);
                }}
              >
                取消
              </button>
            </div>
          ) : (
            <button
              type="button"
              className="btn small danger-ghost"
              onClick={() => void requestDelete()}
              disabled={previewing}
              title="删除此产物"
              aria-label="删除产物"
            >
              <Trash2 className="w-3.5 h-3.5 mr-1" />
              <span>删除产物</span>
            </button>
          )}
        </div>
      </div>
      {deleteError && (
        <p className="art-error mb-2" role="alert">{deleteError}</p>
      )}
      {body}
    </>
  );
}

export function ArtifactsPanel({
  documentId,
  isDialog = false,
  active = true,
}: {
  documentId: string | null;
  /** 兼容既有调用方;生成已移至 AI 工具栏,面板内不再使用。 */
  versionId: string | null;
  isDialog?: boolean;
  /** 面板此刻是否显示(右栏标签=资料且右栏展开)。隐藏时不发起列表查询。 */
  active?: boolean;
}) {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const openArtifactReading = useWorkspaceUi((s) => s.openArtifactReading);
  const [open, setOpen] = useState(isDialog);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selectedVersionId, setSelectedVersionId] = useState<string | null>(null);

  const [viewScope, setViewScope] = useState<"current" | "all">(documentId ? "current" : "all");
  const effectiveDocId = viewScope === "current" ? (documentId ?? "") : "";
  // 阅览态:选中产物的只读阅读视图(不改变列表选中语义)。
  const [reading, setReading] = useState(false);
  const [query, setQuery] = useState("");
  const [kindFilter, setKindFilter] = useState<string>("all");
  // 列表内删除的二次确认(与详情删除共用同一命令)。
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);
  const [pendingDeletePreview, setPendingDeletePreview] = useState<ArtifactDeletePreview | null>(null);
  const [previewingDeleteId, setPreviewingDeleteId] = useState<string | null>(null);
  const [listDeleting, setListDeleting] = useState(false);
  const [listDeleteError, setListDeleteError] = useState<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const [bulkConfirm, setBulkConfirm] = useState(false);
  const [bulkDeleting, setBulkDeleting] = useState(false);

  // 「查看结果 / 打开资料」由 WorkspaceView 转存到 uiStore,面板挂载后消费,
  // 事件不再因为面板尚未挂载而丢失;支持按 artifactId 或 artifactVersionId 定位。
  const pendingArtifactOpen = useWorkspaceUi((s) => s.pendingArtifactOpen);
  const consumePendingArtifactOpen = useWorkspaceUi((s) => s.consumePendingArtifactOpen);

  useEffect(() => {
    setSelectedId(null);
    setSelectedVersionId(null);
    setSelectedIds(new Set());
    setReading(false);
    setPendingDeleteId(null);
    setPendingDeletePreview(null);
    setBulkConfirm(false);
  }, [documentId]);

  const artifacts = useQuery({
    queryKey: ["artifacts", effectiveDocId || "all-library"],
    queryFn: () =>
      client.callFlat<{ items: ArtifactListItem[] }>("artifact_list", {
        documentId: effectiveDocId,
      }),
    enabled: (open || isDialog) && active,
  });

  const items = artifacts.data?.items ?? [];

  useEffect(() => {
    if (!open || pendingArtifactOpen === null || items.length === 0) return;
    const match =
      (pendingArtifactOpen.artifactId !== null
        ? items.find((item) => item.id === pendingArtifactOpen.artifactId)
        : undefined) ??
      (pendingArtifactOpen.versionId !== null
        ? items.find((item) => item.currentVersionId === pendingArtifactOpen.versionId)
        : undefined);
    consumePendingArtifactOpen();
    if (match === undefined) return;
    setSelectedId(match.id);
    setSelectedVersionId(pendingArtifactOpen.versionId ?? null);
    setReading(pendingArtifactOpen.read);
    openArtifactReading({
      artifactId: match.id,
      versionId: pendingArtifactOpen.versionId ?? null,
      documentId: documentId ?? null,
    });
    void queryClient.invalidateQueries({ queryKey: ["artifacts"] });
  }, [open, pendingArtifactOpen, items, consumePendingArtifactOpen, openArtifactReading, documentId, queryClient]);

  const kindOptions = Array.from(new Set(items.map((item) => item.kind))).sort();
  const visibleItems = filterArtifacts(items, {
    kind: kindFilter === "all" ? undefined : kindFilter,
    query,
  });

  async function requestListDelete(item: ArtifactListItem): Promise<void> {
    setListDeleteError(null);
    setPreviewingDeleteId(item.id);
    try {
      const preview = await client.callFlat<ArtifactDeletePreview>("artifact_delete_preview", {
        artifactId: item.id,
      });
      setPendingDeletePreview(preview);
      setPendingDeleteId(item.id);
    } catch (caught) {
      setListDeleteError(
        caught instanceof ApiError
          ? describeAppError(caught.appError).title
          : "无法获取删除影响面,未执行删除",
      );
    } finally {
      setPreviewingDeleteId(null);
    }
  }

  async function deleteFromList(item: ArtifactListItem): Promise<void> {
    setListDeleting(true);
    setListDeleteError(null);
    try {
      // 删除需要乐观并发校验:先取最新 revision,避免删掉别人刚改过的产物。
      const fresh = await client.callFlat<ArtifactView>("artifact_get", {
        artifactId: item.id,
      });
      await client.call("artifact_delete", {
        artifactId: item.id,
        expectedRevision: fresh.revision,
      });
      if (selectedId === item.id) {
        setSelectedId(null);
        setSelectedVersionId(null);
        setReading(false);
      }
      setPendingDeleteId(null);
      setPendingDeletePreview(null);
      await queryClient.invalidateQueries({ queryKey: ["artifacts", effectiveDocId || "all-library"] });
      await queryClient.invalidateQueries({ queryKey: ["tasks"] });
    } catch (caught) {
      if (caught instanceof ApiError) {
        setListDeleteError(describeAppError(caught.appError).title);
      } else {
        setListDeleteError("删除产物失败,请重试");
      }
    } finally {
      setListDeleting(false);
    }
  }
  function toggleSelected(id: string): void {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
    setBulkConfirm(false);
  }

  async function bulkDelete(): Promise<void> {
    if (selectedIds.size === 0) return;
    setBulkDeleting(true);
    setListDeleteError(null);
    const failures: string[] = [];
    let lastError = "";
    for (const id of selectedIds) {
      try {
        const fresh = await client.callFlat<ArtifactView>("artifact_get", { artifactId: id });
        await client.call("artifact_delete", {
          artifactId: id,
          expectedRevision: fresh.revision,
        });
        if (selectedId === id) {
          setSelectedId(null);
          setSelectedVersionId(null);
          setReading(false);
        }
      } catch (caught) {
        failures.push(id);
        lastError =
          caught instanceof ApiError
            ? describeAppError(caught.appError).title
            : "删除产物失败,请重试";
      }
    }
    setSelectedIds(new Set(failures));
    setBulkConfirm(false);
    setBulkDeleting(false);
    await queryClient.invalidateQueries({
      queryKey: ["artifacts", effectiveDocId || "all-library"],
    });
    await queryClient.invalidateQueries({ queryKey: ["tasks"] });
    if (failures.length > 0) {
      setListDeleteError(
        `${failures.length} 个产物删除失败${lastError ? `(${lastError})` : ""},已保留选择可重试`,
      );
    }
  }

  const detail = useQuery({
    queryKey: ["artifact", selectedId ?? "none"],
    queryFn: () => client.callFlat<ArtifactView>("artifact_get", { artifactId: selectedId! }),
    enabled: selectedId !== null,
  });

  const artifact = detail.data;
  const versions: ArtifactVersionView[] =
    artifact === undefined
      ? []
      : artifact.versions.length > 0
        ? artifact.versions
        : artifact.currentVersion !== undefined
          ? [artifact.currentVersion]
          : [];
  const activeVersion =
    versions.find((version) => version.id === selectedVersionId) ??
    artifact?.currentVersion ??
    versions[versions.length - 1];

  function selectArtifact(id: string): void {
    setSelectedId(id);
    setSelectedVersionId(null);
  }

  if (documentId === null) return null;

  return (
    <section className={`art-panel${isDialog ? " is-dialog" : ""}`} aria-label="产物中心">
      {!isDialog && (
        <button
          type="button"
          className="art-toggle"
          aria-expanded={open}
          aria-controls="art-panel-body"
          onClick={() => setOpen(!open)}
        >
          产物中心{open ? "(收起)" : `(展开${(artifacts.data?.items.length ?? 0) > 0 ? ` · ${artifacts.data?.items.length}` : ""})`}
        </button>
      )}
      {open || isDialog ? (
        <div id="art-panel-body" className="art-body">
          <div className="art-scope-switch" role="tablist" aria-label="资料范围">
            <button
              type="button"
              role="tab"
              aria-selected={viewScope === "current"}
              className={`art-scope-tab${viewScope === "current" ? " is-active" : ""}`}
              onClick={() => setViewScope("current")}
              disabled={!documentId}
            >
              当前文档资料
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={viewScope === "all"}
              className={`art-scope-tab${viewScope === "all" ? " is-active" : ""}`}
              onClick={() => setViewScope("all")}
            >
              总体资料库
            </button>
            <span className="art-scope-count">
              共 {artifacts.data?.items?.length ?? 0} 个资料
            </span>
          </div>
          {artifacts.isLoading ? <p role="status">产物加载中…</p> : null}
          {artifacts.isError ? (
            <div className="art-state" role="alert">
              <p>产物列表加载失败。</p>
              <button type="button" className="btn tiny" onClick={() => void artifacts.refetch()}>
                重试
              </button>
            </div>
          ) : null}

          {/* 搜索 + 类型筛选:在当前已加载列表上即时过滤。 */}
          {items.length > 0 ? (
            <div className="art-filters" role="search">
              <label className="art-search">
                <Search className="w-3.5 h-3.5" aria-hidden="true" />
                <input
                  type="search"
                  value={query}
                  placeholder="搜索标题"
                  aria-label="按标题搜索产物"
                  onChange={(event) => setQuery(event.target.value)}
                />
              </label>
              <label className="art-kind-filter">
                <span className="visually-hidden">按类型筛选</span>
                <select
                  value={kindFilter}
                  aria-label="按类型筛选产物"
                  onChange={(event) => setKindFilter(event.target.value)}
                >
                  <option value="all">全部类型</option>
                  {kindOptions.map((kind) => (
                    <option key={kind} value={kind}>
                      {kindLabel(kind)}
                    </option>
                  ))}
                </select>
              </label>
            </div>
          ) : null}

          {visibleItems.length > 0 ? (
            <div className="art-bulk-bar" role="group" aria-label="批量管理产物">
              <label className="art-bulk-select-all">
                <input
                  type="checkbox"
                  checked={
                    visibleItems.length > 0 &&
                    visibleItems.every((item) => selectedIds.has(item.id))
                  }
                  onChange={() => {
                    const all = visibleItems.every((item) => selectedIds.has(item.id));
                    setSelectedIds(
                      all ? new Set() : new Set(visibleItems.map((item) => item.id)),
                    );
                    setBulkConfirm(false);
                  }}
                />
                全选当前列表
              </label>
              <span className="art-bulk-count">已选 {selectedIds.size} 项</span>
              {selectedIds.size > 0 ? (
                bulkConfirm ? (
                  <span className="art-confirm-inline" role="alertdialog" aria-label="确认批量删除">
                    <span>删除所选 {selectedIds.size} 个产物及全部版本?</span>
                    <button
                      type="button"
                      className="btn tiny danger"
                      disabled={bulkDeleting}
                      onClick={() => void bulkDelete()}
                    >
                      {bulkDeleting ? "删除中…" : "确认删除"}
                    </button>
                    <button
                      type="button"
                      className="btn tiny ghost"
                      disabled={bulkDeleting}
                      onClick={() => setBulkConfirm(false)}
                    >
                      取消
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    className="btn tiny danger-ghost"
                    onClick={() => setBulkConfirm(true)}
                  >
                    <Trash2 className="w-3 h-3" aria-hidden="true" />
                    批量删除
                  </button>
                )
              ) : null}
            </div>
          ) : null}
          {artifacts.data && items.length === 0 ? (
            <div className="art-state art-empty">
              <p>暂无产物。可通过 AI 工具栏生成总结、练习题或翻译,结果会保存在这里。</p>
            </div>
          ) : null}
          {items.length > 0 && visibleItems.length === 0 ? (
            <div className="art-state" role="status">
              <p>没有符合筛选条件的产物。</p>
              <button
                type="button"
                className="btn tiny ghost"
                onClick={() => {
                  setQuery("");
                  setKindFilter("all");
                }}
              >
                清除筛选
              </button>
            </div>
          ) : null}
          {listDeleteError ? (
            <p className="art-error" role="alert">
              {listDeleteError}
            </p>
          ) : null}
          <ul className="art-list">
            {visibleItems.map((item) => (
              <li key={item.id} className="art-card">
                <label className="art-select" title={`选择:${item.title}`}>
                  <input
                    type="checkbox"
                    checked={selectedIds.has(item.id)}
                    onChange={() => toggleSelected(item.id)}
                    aria-label={`选择产物:${item.title}`}
                  />
                </label>
                <button
                  type="button"
                  className={`art-item${item.id === selectedId ? " current" : ""}`}
                  onClick={() => {
                    selectArtifact(item.id);
                    setReading(false);
                  }}
                  aria-current={item.id === selectedId ? "true" : undefined}
                >
                  <span className="art-card-head">
                    <span className="art-kind-badge">{kindLabel(item.kind)}</span>
                    <span className="art-card-title">{item.title}</span>
                  </span>
                  <span className="art-badge-line">
                    引用 {item.citationCount} · 版本 {item.currentVersionNo?.toString() ?? "—"}
                    {item.coverageComplete === false ? " · 范围未完整覆盖" : ""}
                  </span>
                </button>
                <div className="art-card-actions">
                  <button
                    type="button"
                    className="btn tiny ghost"
                    title={`阅览:${item.title}`}
                    aria-label="阅览该产物"
                    onClick={() => {
                      // 在主阅读区打开:像文档一样整页滚动阅读,
                      // 右栏窄面板不再塞入长正文。
                      openArtifactReading({
                        artifactId: item.id,
                        versionId: item.currentVersionId ?? null,
                        documentId: item.documentId ?? null,
                      });
                    }}
                  >
                    <BookOpen className="w-3 h-3" aria-hidden="true" />
                    <span>阅览</span>
                  </button>
                  {pendingDeleteId === item.id ? null : (
                    <button
                      type="button"
                      className="btn tiny danger-ghost"
                      title={`删除:${item.title}`}
                      aria-label="删除该产物"
                      disabled={previewingDeleteId === item.id}
                      onClick={() => void requestListDelete(item)}
                    >
                      <Trash2 className="w-3 h-3" aria-hidden="true" />
                      <span>删除</span>
                    </button>
                  )}
                </div>
                {pendingDeleteId === item.id ? (
                  <div className="art-confirm-row" role="alertdialog" aria-label="确认删除产物">
                    <span>
                      {pendingDeletePreview === null
                        ? "正在获取删除影响面…"
                        : `删除“${item.title}”:${pendingDeletePreview.versionCount} 个版本、${pendingDeletePreview.citationCount} 处引用、${pendingDeletePreview.exportRecordCount} 条导出记录?`}
                    </span>
                    <button
                      type="button"
                      className="btn tiny danger"
                      disabled={listDeleting || pendingDeletePreview === null}
                      onClick={() => void deleteFromList(item)}
                    >
                      {listDeleting ? "删除中…" : "确认"}
                    </button>
                    <button
                      type="button"
                      className="btn tiny ghost"
                      disabled={listDeleting}
                      onClick={() => {
                        setPendingDeleteId(null);
                        setPendingDeletePreview(null);
                      }}
                    >
                      取消
                    </button>
                  </div>
                ) : null}
              </li>
            ))}
          </ul>
          {detail.isError ? <p role="alert">产物详情加载失败。</p> : null}
          {artifact !== undefined && activeVersion !== undefined ? (
            <div className="art-detail">
              {versions.length > 1 ? (
                <label className="art-version-select">
                  <span>版本</span>
                  <select
                    value={activeVersion.id}
                    aria-label="选择产物版本"
                    onChange={(event) => setSelectedVersionId(event.target.value)}
                  >
                    {versions.map((version) => (
                      <option key={version.id} value={version.id}>
                        v{version.versionNo} · {ORIGIN_LABELS[version.origin] ?? version.origin} ·{" "}
                        {STATE_LABELS[version.state] ?? version.state}
                        {version.id === artifact.currentVersionId ? " · 当前" : ""}
                      </option>
                    ))}
                  </select>
                </label>
              ) : null}
              {activeVersion.coverage.complete === false ? (
                <p className="art-coverage-warning" role="status">
                  本产物只覆盖所选范围,未完整覆盖全文;不要据此推断全文结论。
                </p>
              ) : null}
              {reading ? (
                <ArtifactReader
                  artifact={artifact}
                  version={activeVersion}
                  client={client}
                  onSaved={() => {
                    void queryClient.invalidateQueries({ queryKey: ["artifacts"] });
                    void queryClient.invalidateQueries({ queryKey: ["artifact", artifact.id] });
                  }}
                  onBack={() => setReading(false)}
                  exportControls={
                    <button
                      type="button"
                      className="btn tiny"
                      onClick={() => setReading(false)}
                      aria-label="回到产物详情(可导出)"
                    >
                      <Download className="w-3.5 h-3.5" aria-hidden="true" />
                      <span>导出/详情</span>
                    </button>
                  }
                />
              ) : (
              <ArtifactDetail
                artifact={artifact}
                version={activeVersion}
                onDeleted={() => {
                  setSelectedId(null);
                  setSelectedVersionId(null);
                  setReading(false);
                  void queryClient.invalidateQueries({
                    queryKey: ["artifacts", documentId],
                  });
                  void queryClient.invalidateQueries({ queryKey: ["tasks"] });
                }}
              />
              )}
            </div>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
