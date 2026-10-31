// 应用与偏好:隐私清除 + 诊断导出(真实命令,不伪造计数)。
//
// 隐私清除是"按文档"的计划式流程:预览(planId/planHash/数量)→
// 输入确认字符串(= 文档 ID)→ 执行 → 显示审计 ID。后端会重新预览并
// 校验 planHash,数据漂移时拒绝执行,前端只如实展示错误。
//
// 诊断导出必须先预览拿到 previewHash;路径显式给出(不覆盖已有文件),
// 默认脱敏,路径/模型明细仅在用户勾选后进入预览。
import { useCallback, useEffect, useState } from "react";
import type { CommandClient } from "../../app/api/client";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import { dialogSavePath } from "../../app/ipc/transport";
import type { DocumentSummary } from "../../shared/generated/DocumentSummary";
import type { ClearCategory } from "../../shared/generated/ClearCategory";
import type { ClearPreview } from "../../shared/generated/ClearPreview";
import type { ClearResult } from "../../shared/generated/ClearResult";
import type { DiagnosticsPreview } from "../../shared/generated/DiagnosticsPreview";
import type { DiagnosticsExportResult } from "../../shared/generated/DiagnosticsExportResult";

const CLEAR_CATEGORY_META: Array<{ id: ClearCategory; label: string; asset: boolean }> = [
  { id: "fts_vector_cache", label: "检索索引 / 向量缓存(可重建)", asset: false },
  { id: "parse_cache", label: "解析缓存(未被引用的块)", asset: false },
  { id: "conversations", label: "会话与消息", asset: true },
  { id: "artifacts", label: "产物与版本", asset: true },
  { id: "agent_reasoning", label: "思考记录(仅开启保存时存在)", asset: true },
  { id: "insight_cards", label: "洞察卡片 / 文档画像缓存", asset: false },
  { id: "translation_memory", label: "翻译记忆(不删已生成译文)", asset: false },
  { id: "annotations", label: "标注 / 笔迹 / 文本框", asset: true },
  { id: "bookmarks", label: "书签", asset: true },
  { id: "notes", label: "笔记", asset: true },
  { id: "clarifications", label: "待澄清项", asset: true },
  { id: "tags", label: "标签", asset: true },
];

function presentError(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    const presentation = describeAppError(error.appError);
    return (
      presentation.title +
      (presentation.actionLabel ? `(${presentation.actionLabel})` : "") +
      (presentation.detailLines.length > 0 ? ` — ${presentation.detailLines.join(";")}` : "")
    );
  }
  return fallback;
}

export function PrivacySection({
  client,
  diagnosticsIncludePaths,
  diagnosticsIncludeModelDetails,
  onError,
}: {
  client: CommandClient;
  diagnosticsIncludePaths: boolean;
  diagnosticsIncludeModelDetails: boolean;
  onError: (message: string) => void;
}) {
  const [documents, setDocuments] = useState<DocumentSummary[] | null>(null);
  const [documentsError, setDocumentsError] = useState<string | null>(null);
  const [documentId, setDocumentId] = useState("");
  const [selected, setSelected] = useState<Set<ClearCategory>>(() => new Set());
  const [preview, setPreview] = useState<ClearPreview | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [confirmation, setConfirmation] = useState("");
  const [executing, setExecuting] = useState(false);
  const [clearResult, setClearResult] = useState<ClearResult | null>(null);
  const [clearNotice, setClearNotice] = useState<string | null>(null);

  const [diagPreview, setDiagPreview] = useState<DiagnosticsPreview | null>(null);
  const [diagBusy, setDiagBusy] = useState(false);
  const [diagPath, setDiagPath] = useState("");
  const [diagResult, setDiagResult] = useState<DiagnosticsExportResult | null>(null);

  const loadDocuments = useCallback(async () => {
    try {
      const page = await client.call<{ items: DocumentSummary[] }>("document_list_recent", {
        limit: 100,
      });
      setDocuments(page.items);
      setDocumentsError(null);
      setDocumentId((current) =>
        current.length > 0 && page.items.some((item) => item.id === current)
          ? current
          : (page.items[0]?.id ?? ""),
      );
    } catch (error) {
      setDocuments([]);
      setDocumentsError(presentError(error, "文档列表加载失败"));
    }
  }, [client]);

  useEffect(() => {
    void loadDocuments();
  }, [loadDocuments]);

  function resetPlan() {
    setPreview(null);
    setConfirmation("");
    setClearResult(null);
    setClearNotice(null);
  }

  function toggleCategory(id: ClearCategory) {
    setSelected((previous) => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
    resetPlan();
  }

  async function runPreview() {
    if (documentId.length === 0 || selected.size === 0) return;
    setPreviewBusy(true);
    setClearResult(null);
    setClearNotice(null);
    try {
      const plan = await client.call<ClearPreview>("privacy_preview_clear", {
        documentId,
        categories: [...selected],
      });
      setPreview(plan);
      setConfirmation("");
    } catch (error) {
      setPreview(null);
      onError(presentError(error, "隐私清除预览失败"));
    } finally {
      setPreviewBusy(false);
    }
  }

  async function executeClear() {
    if (preview === null || confirmation !== documentId) return;
    setExecuting(true);
    try {
      const result = await client.call<ClearResult>("privacy_execute_clear", {
        documentId,
        clearPlanId: preview.planId,
        planHash: preview.planHash,
        confirmation,
        idempotencyKey: client.newIdempotencyKey(),
      });
      setClearResult(result);
      setClearNotice("清除已完成;原文件不受影响。");
      setPreview(null);
      setConfirmation("");
      await loadDocuments();
    } catch (error) {
      onError(presentError(error, "隐私清除未完成"));
    } finally {
      setExecuting(false);
    }
  }

  async function previewDiagnostics() {
    setDiagBusy(true);
    try {
      const preview = await client.call<DiagnosticsPreview>("diagnostics_preview", {
        includePaths: diagnosticsIncludePaths,
        includeModelDetails: diagnosticsIncludeModelDetails,
      });
      setDiagPreview(preview);
      setDiagResult(null);
    } catch (error) {
      setDiagPreview(null);
      onError(presentError(error, "诊断预览失败"));
    } finally {
      setDiagBusy(false);
    }
  }

  async function chooseDiagnosticsPath() {
    try {
      const picked = await dialogSavePath({
        title: "保存诊断包",
        defaultPath: "knowwise-diagnostics.json",
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (picked !== null) setDiagPath(picked);
    } catch {
      onError("无法打开保存对话框;可直接在路径输入框填写完整路径");
    }
  }

  async function exportDiagnostics() {
    if (diagPreview === null || diagPath.trim().length === 0) return;
    setDiagBusy(true);
    try {
      const result = await client.call<DiagnosticsExportResult>("diagnostics_export", {
        previewHash: diagPreview.previewHash,
        path: diagPath.trim(),
        idempotencyKey: client.newIdempotencyKey(),
      });
      setDiagResult(result);
    } catch (error) {
      onError(presentError(error, "诊断导出失败"));
    } finally {
      setDiagBusy(false);
    }
  }

  const selectedDocument = documents?.find((item) => item.id === documentId) ?? null;
  const previewByCategory = new Map(
    (preview?.categories ?? []).map((entry) => [entry.category, entry]),
  );
  const executable =
    preview !== null &&
    confirmation === documentId &&
    preview.categories.every((entry) => entry.available) &&
    !executing;

  return (
    <>
      <div className="settings-card privacy-clear-card">
        <h3>隐私清除(按文档)</h3>
        <p className="settings-hint">
          只删除所选文档在应用内生成的数据;原文件永远不受影响。执行前必须预览,
          并输入文档 ID 作为确认字符串。受证据保留保护的块不会被清除。
        </p>
        {documentsError ? (
          <p className="settings-error" role="alert">
            {documentsError}
          </p>
        ) : null}
        <div className="settings-row">
          <label className="settings-input grow">
            <span>目标文档</span>
            <select
              value={documentId}
              aria-label="隐私清除目标文档"
              onChange={(event) => {
                setDocumentId(event.target.value);
                resetPlan();
              }}
              disabled={documents === null || documents.length === 0}
            >
              {documents === null ? <option value="">加载中…</option> : null}
              {documents !== null && documents.length === 0 ? (
                <option value="">没有可清除的文档</option>
              ) : null}
              {(documents ?? []).map((document) => (
                <option key={document.id} value={document.id}>
                  {document.displayName || document.id.slice(0, 8)}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className="btn small ghost"
            onClick={() => void loadDocuments()}
            disabled={previewBusy || executing}
          >
            刷新文档
          </button>
        </div>

        <fieldset className="privacy-category-grid" aria-label="清除类别">
          {CLEAR_CATEGORY_META.map(({ id, label, asset }) => {
            const entry = previewByCategory.get(id);
            return (
              <label
                key={id}
                className={`privacy-category${asset ? " asset" : ""}`}
                title={entry?.available === false ? "该类别未注册贡献模块,不执行" : label}
              >
                <input
                  type="checkbox"
                  checked={selected.has(id)}
                  onChange={() => toggleCategory(id)}
                  disabled={entry?.available === false}
                  aria-label={`清除类别:${label}`}
                />
                <span className="privacy-category-label">{label}</span>
                {entry !== undefined && entry.available ? (
                  <span className="privacy-category-count">
                    {entry.count ?? 0}
                    {entry.protectedCount !== undefined && entry.protectedCount > 0
                      ? ` · 保留 ${entry.protectedCount}`
                      : ""}
                    {entry.affectedCitations !== undefined && entry.affectedCitations > 0
                      ? ` · 引用 ${entry.affectedCitations}`
                      : ""}
                  </span>
                ) : entry !== undefined ? (
                  <span className="privacy-category-count">不可用</span>
                ) : null}
              </label>
            );
          })}
        </fieldset>

        <div className="settings-row">
          <button
            type="button"
            className="btn small"
            onClick={() => void runPreview()}
            disabled={previewBusy || executing || documentId.length === 0 || selected.size === 0}
          >
            {previewBusy ? "预览中…" : "预览清除计划"}
          </button>
          {preview !== null ? (
            <span className="settings-hint">
              计划 {preview.planId.slice(0, 12)}… · 校验 {preview.planHash.slice(0, 12)}… ·
              到期 {new Date(preview.expiresAt).toLocaleTimeString("zh-CN")}
            </span>
          ) : null}
        </div>

        {preview !== null ? (
          <div className="privacy-confirm-block">
            <p className="settings-hint">
              输入文档 ID 确认:{selectedDocument?.displayName ?? ""}
              <code className="privacy-confirm-id">{documentId}</code>
            </p>
            <div className="settings-row">
              <label className="settings-input grow">
                <span>确认字符串</span>
                <input
                  value={confirmation}
                  aria-label="隐私清除确认字符串"
                  onChange={(event) => setConfirmation(event.target.value)}
                  placeholder="粘贴上面的文档 ID"
                  spellCheck={false}
                />
              </label>
              <button
                type="button"
                className="btn small danger"
                onClick={() => void executeClear()}
                disabled={!executable}
              >
                {executing ? "执行中…" : "执行清除"}
              </button>
            </div>
          </div>
        ) : null}

        {clearNotice ? (
          <p className="settings-ok" role="status">
            {clearNotice}
          </p>
        ) : null}
        {clearResult !== null ? (
          <div className="privacy-result" role="status">
            <p>
              审计 ID:<code>{clearResult.auditId}</code> · 取消活动任务{" "}
              {clearResult.canceledTasks} 个
            </p>
            <ul>
              {clearResult.executed.map((entry) => (
                <li key={entry.category}>
                  {CLEAR_CATEGORY_META.find((meta) => meta.id === entry.category)?.label ??
                    entry.category}
                  :删除 {entry.deleted}
                </li>
              ))}
            </ul>
          </div>
        ) : null}
      </div>

      <div className="settings-card">
        <h3>诊断导出</h3>
        <p className="settings-hint">
          默认脱敏:不包含正文、提示词、密钥与文件路径。文件路径与模型明细只有在你
          勾选上方的“诊断默认包含…”后才会进入预览。导出使用显式路径,不覆盖已有文件。
        </p>
        <div className="settings-row">
          <button
            type="button"
            className="btn small"
            onClick={() => void previewDiagnostics()}
            disabled={diagBusy}
          >
            {diagBusy && diagPreview === null ? "预览中…" : "预览诊断"}
          </button>
          {diagPreview !== null ? (
            <span className="settings-hint">
              previewHash:<code>{diagPreview.previewHash.slice(0, 16)}…</code> ·{" "}
              {diagPreview.appVersion} / {diagPreview.os} · 路径{" "}
              {diagPreview.includePaths ? "已包含" : "已脱敏"} · 模型明细{" "}
              {diagPreview.includeModelDetails ? "已包含" : "已脱敏"}
            </span>
          ) : null}
        </div>
        {diagPreview !== null ? (
          <>
            <div className="settings-row">
              <label className="settings-input grow">
                <span>保存路径(完整文件路径,不覆盖已有文件)</span>
                <input
                  value={diagPath}
                  aria-label="诊断保存路径"
                  onChange={(event) => setDiagPath(event.target.value)}
                  placeholder="C:\Temp\knowwise-diagnostics.json"
                  spellCheck={false}
                />
              </label>
              <button type="button" className="btn small ghost" onClick={() => void chooseDiagnosticsPath()}>
                选择位置…
              </button>
              <button
                type="button"
                className="btn small"
                onClick={() => void exportDiagnostics()}
                disabled={diagBusy || diagPath.trim().length === 0}
              >
                {diagBusy ? "导出中…" : "导出诊断"}
              </button>
            </div>
            <div className="privacy-result" role="status">
              <p>
                任务统计:
                {diagPreview.taskCounts.length === 0
                  ? "无"
                  : diagPreview.taskCounts
                      .map((entry) => `${entry.key}=${entry.count}`)
                      .join(" · ")}
              </p>
              {diagPreview.dataDirPath !== null ? (
                <p>
                  数据目录:<code>{diagPreview.dataDirPath}</code>
                </p>
              ) : null}
            </div>
          </>
        ) : null}
        {diagResult !== null ? (
          <p className="settings-ok" role="status">
            已导出 {diagResult.bytes} 字节 → <code>{diagResult.path}</code>
          </p>
        ) : null}
      </div>
    </>
  );
}
