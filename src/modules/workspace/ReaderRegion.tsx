// M13 拥有:阅读器挂载区(《M13》§4)。壳层只负责容器、状态横幅与阅读
// 位置;正文渲染由 M03 的 DocumentBody 按格式分发。阅读首屏不等语义
// 索引(《M03》§8)。安静状态(可阅读且索引就绪/未解析)不显示横幅,
// 仅在解析中、部分可用或异常时提示。笔记面板挂载于正文渲染区下方。
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import type { DocumentDetail } from "../../shared/generated/DocumentDetail";
import type { ParseStatusView } from "../../shared/generated/ParseStatusView";
import type { PageOcrStatusView } from "../../shared/generated/PageOcrStatusView";
import type { ReadingPosition } from "../../shared/generated/ReadingPosition";
import type { RenderPlan } from "../../shared/generated/RenderPlan";
import type { FileCheckResult } from "../../shared/generated/FileCheckResult";
import { useServices } from "../../app/services";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import { dialogPickDocument, dialogSavePath } from "../../app/ipc/transport";
import { qk } from "../../app/api/keys";
import { DocumentBody } from "../reader/DocumentBody";
import { ReaderStateBanner, readerStateText } from "../reader/ReaderStateBanner";
import { ReaderToolbarExtrasProvider } from "../reader/readerToolbarExtras";
import { useReaderPrefs } from "../reader/readerPrefs";
import { aiPhaseFromParseStatus, readableFromFileState } from "./statusText";
import { PageCoverageBar } from "./PageCoverageMap";
import { PageOcrProgressPanel } from "../reader/PageOcrProgressPanel";
import { useWorkspaceUi } from "./uiStore";
import { BookOpen } from "lucide-react";

export function ReaderRegion({
  documentId,
  versionId,
}: {
  documentId: string | null;
  versionId: string | null;
}) {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const [fileBusy, setFileBusy] = useState(false);
  const [fileCheck, setFileCheck] = useState<FileCheckResult | null>(null);
  const [fileMessage, setFileMessage] = useState<{ kind: "info" | "error"; text: string } | null>(
    null,
  );

  const detail = useQuery({
    queryKey: documentId ? qk.documentDetail(documentId) : ["document", "none"],
    queryFn: () => client.call<DocumentDetail>("document_get", { documentId }),
    enabled: documentId !== null,
  });
  const activeVersionId = versionId ?? detail.data?.currentVersionId ?? null;
  const parseStatus = useQuery({
    queryKey: documentId ? qk.parseStatus(documentId) : ["parse", "none"],
    queryFn: () => client.call<ParseStatusView>("parse_get_status", { documentId }),
    enabled: documentId !== null,
  });
  // 页覆盖移到阅读区底部后,OCR 状态由阅读器自己取(与文档同域)。
  const ocrStatus = useQuery({
    // 与事件失效键(2 段)保持一致;同版本共用缓存。
    queryKey: ["page-ocr", activeVersionId ?? "none"],
    queryFn: () =>
      client.call<PageOcrStatusView[]>("page_ocr_get_status", {
        documentId: documentId!,
        versionId: activeVersionId!,
      }),
    enabled: documentId !== null && activeVersionId !== null,
    staleTime: 2_000,
    refetchInterval: () => (parseStatus.data?.parseState === "in_progress" ? 1_500 : false),
  });
  const plan = useQuery({
    // 无文档时用 "none" 占位键(组件随后早退,不发起查询)。
    queryKey: qk.renderPlan(documentId ?? "none", versionId),
    queryFn: () =>
      client.call<RenderPlan>("reader_get_render_plan", {
        // enabled 已保证非空;查询键同样以非空文档为域。
        documentId: documentId!,
        versionId: versionId ?? detail.data?.currentVersionId ?? undefined,
      }),
    enabled:
      documentId !== null && (versionId !== null || detail.data?.currentVersionId != null),
    // 快速连点标签时不再"先空白再重建":渲染区容器始终保持挂载,
    // 只把内部内容换成轻量加载态(见下方 ReaderMount)。
  });
  const position = useQuery({
    queryKey: ["reader", "position", documentId ?? "none"],
    queryFn: () =>
      client.call<ReadingPosition | null>("reader_get_position", { documentId: documentId! }),
    enabled: documentId !== null,
  });

  // 阅读偏好(字号/行距/版心):localStorage 持久化,作用于 MD/DOCX 正文。
  const { prefs, setPrefs, resetPrefs } = useReaderPrefs();
  const [prefsOpen, setPrefsOpen] = useState(false);
  const [briefFormat, setBriefFormat] = useState<"markdown" | "html">("markdown");
  const [briefBusy, setBriefBusy] = useState(false);
  const [briefNotice, setBriefNotice] = useState<string | null>(null);
  const prefsBarRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!prefsOpen) return;
    const onPointerDown = (event: MouseEvent) => {
      const bar = prefsBarRef.current;
      if (bar !== null && event.target instanceof Node && bar.contains(event.target)) return;
      setPrefsOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setPrefsOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [prefsOpen]);
  // 切换文档时收起偏好弹层,避免跨文档误操作。
  useEffect(() => {
    setPrefsOpen(false);
  }, [documentId, versionId]);

  async function exportBrief(): Promise<void> {
    if (documentId === null) return;
    const title = detail.data?.displayName ?? "文档";
    const path = await dialogSavePath({
      title: "导出阅读简报",
      defaultPath: `${title}-阅读简报.${briefFormat === "html" ? "html" : "md"}`,
      filters:
        briefFormat === "html"
          ? [{ name: "HTML", extensions: ["html", "htm"] }]
          : [{ name: "Markdown", extensions: ["md", "markdown"] }],
    });
    if (path === null) return;
    setBriefBusy(true);
    setBriefNotice(null);
    try {
      const result = await client.call<{
        bytesWritten: number;
        annotationCount: number;
        qaCount: number;
      }>("reading_brief_export", { documentId, format: briefFormat, path });
      setBriefNotice(
        `简报已导出:${result.annotationCount} 条标注、${result.qaCount} 轮问答,${Math.max(1, Math.round(result.bytesWritten / 1024))} KB`,
      );
    } catch {
      setBriefNotice("简报导出失败,请重试");
    } finally {
      setBriefBusy(false);
      window.setTimeout(() => setBriefNotice(null), 6000);
    }
  }

  async function refreshDocumentQueries(): Promise<void> {
    if (documentId === null) return;
    await queryClient.invalidateQueries({ queryKey: qk.documentDetail(documentId) });
    await queryClient.invalidateQueries({ queryKey: ["reader", "render-plan", documentId] });
    await queryClient.invalidateQueries({ queryKey: qk.parseStatus(documentId) });
    await queryClient.invalidateQueries({ queryKey: ["documents"] });
  }

  async function checkFile(): Promise<FileCheckResult | null> {
    if (documentId === null) return null;
    setFileBusy(true);
    try {
      const result = await client.call<FileCheckResult>("document_check_file", { documentId });
      setFileCheck(result);
      if (result.status === "missing") {
        setFileMessage({ kind: "error", text: "原文件仍不可访问,请重新连接文件。" });
      } else if (result.status === "changed_pending_confirm") {
        setFileMessage({
          kind: "info",
          text: "检测到文件内容变化,已生成待确认的新版本。确认后才会切换当前版本。",
        });
      } else if (result.status === "unchanged") {
        setFileMessage({ kind: "info", text: "文件未变化。" });
      } else {
        setFileMessage({ kind: "error", text: "文件存在但不可读取,请检查权限或磁盘。" });
      }
      return result;
    } catch (error) {
      setFileMessage({
        kind: "error",
        text:
          error instanceof ApiError
            ? describeAppError(error.appError).title
            : "检查文件失败,请重试",
      });
      return null;
    } finally {
      setFileBusy(false);
    }
  }

  async function reconnectFile(): Promise<void> {
    if (documentId === null) return;
    let path: string | null = null;
    try {
      path = await dialogPickDocument();
    } catch {
      setFileMessage({ kind: "error", text: "无法打开文件选择器,请重试。" });
      return;
    }
    if (path === null) return;
    setFileBusy(true);
    try {
      const result = await client.call<FileCheckResult>("document_reconnect", {
        documentId,
        newPath: path,
        idempotencyKey: client.newIdempotencyKey(),
      });
      setFileCheck(result);
      if (result.status === "unchanged") {
        setFileMessage({ kind: "info", text: "已重新连接文件,内容可正常读取。" });
      } else if (result.status === "changed_pending_confirm") {
        setFileMessage({
          kind: "info",
          text: "已连接文件,但内容与当前版本不同;请确认新版本后阅读。",
        });
      } else {
        setFileMessage({ kind: "error", text: "连接后仍无法读取该文件,请选择正确文件。" });
      }
      await refreshDocumentQueries();
    } catch (error) {
      setFileMessage({
        kind: "error",
        text:
          error instanceof ApiError
            ? describeAppError(error.appError).title
            : "重新连接失败,请确认文件可读",
      });
    } finally {
      setFileBusy(false);
    }
  }

  async function confirmVersion(): Promise<void> {
    if (documentId === null || detail.data == null) return;
    setFileBusy(true);
    try {
      let candidate = fileCheck?.candidateVersionId ?? null;
      if (candidate === null) {
        const checked = await client.call<FileCheckResult>("document_check_file", { documentId });
        setFileCheck(checked);
        candidate = checked.candidateVersionId;
      }
      if (candidate === null) {
        setFileMessage({ kind: "error", text: "没有可确认的新版本;请先检查文件变化。" });
        return;
      }
      // revision 来自刚读取的文档详情;期间若被其他操作推进,后端会拒绝。
      // idempotencyKey 是命令契约的必填字段:漏发会被 IPC 反序列化拒绝,
      // UI 只会显示“与本地服务的通信失败”,确认新版本永远不生效。
      await client.call("document_confirm_version", {
        documentId,
        versionId: candidate,
        expectedRevision: Number(detail.data.revision),
        idempotencyKey: client.newIdempotencyKey(),
      });
      setFileCheck(null);
      setFileMessage({ kind: "info", text: "已确认新版本,当前阅读已切换到该版本。" });
      await refreshDocumentQueries();
    } catch (error) {
      if (error instanceof ApiError) {
        const presentation = describeAppError(error.appError);
        const conflict = error.appError.code === "CONFLICT_REVISION_MISMATCH";
        setFileMessage({
          kind: "error",
          text: conflict
            ? `${presentation.title};已刷新文档修订号,请再次确认新版本。`
            : presentation.title,
        });
        if (conflict) await refreshDocumentQueries();
      } else {
        setFileMessage({ kind: "error", text: "确认新版本失败,请重试" });
      }
    } finally {
      setFileBusy(false);
    }
  }

  if (documentId === null) {
    return (
      <div className="reader-region empty" aria-label="阅读器">
        <BookOpen className="reader-empty-icon" aria-hidden />
        <p className="reader-empty-title">选择一个文档开始阅读</p>
        <p className="reader-empty-hint">支持 PDF、Word 与 Markdown,可同时打开多个标签页。</p>
      </div>
    );
  }

  const bannerInput = {
    readable: readableFromFileState(detail.data?.fileState),
    aiPhase: aiPhaseFromParseStatus(parseStatus.data),
    fileError: detail.data?.fileState === "missing" ? "原文件已移动或删除" : undefined,
  };
  const banner = readerStateText(bannerInput);
  // 安静状态不出横幅:可阅读且(就绪/未解析/仅 FTS)时不打扰。
  const quiet =
    banner.level === "info" && bannerInput.aiPhase !== "parsing" && bannerInput.readable;

  const showPrefs = plan.data !== undefined && plan.data.format !== "pdf";

  return (
    <div className="reader-region" aria-label="阅读器">
      {detail.data !== undefined && detail.data.fileState !== "present" ? (
        <div className="doc-file-notice" role="region" aria-label="文件状态">
          <span className="doc-file-notice-text">
            {detail.data.fileState === "missing"
              ? "原文件已移动或删除;标注与笔记仍保留。"
              : detail.data.fileState === "changed_pending_confirm"
                ? "文件内容已变化,待确认新版本。"
                : "文件当前不可读取。"}
            {fileCheck?.candidateVersionId != null ? (
              <code className="doc-file-candidate">
                候选版本 {fileCheck.candidateVersionId.slice(0, 8)}
              </code>
            ) : null}
          </span>
          <span className="doc-file-notice-actions">
            {detail.data.fileState === "missing" ? (
              <button
                type="button"
                className="btn tiny"
                disabled={fileBusy}
                onClick={() => void reconnectFile()}
              >
                重新连接文件
              </button>
            ) : null}
            <button
              type="button"
              className="btn tiny ghost"
              disabled={fileBusy}
              onClick={() => void checkFile()}
            >
              检查文件
            </button>
            {detail.data.fileState === "changed_pending_confirm" ||
            fileCheck?.candidateVersionId != null ? (
              <button
                type="button"
                className="btn tiny"
                disabled={fileBusy}
                onClick={() => void confirmVersion()}
              >
                确认新版本
              </button>
            ) : null}
          </span>
          {fileMessage !== null ? (
            <p
              className={`doc-file-notice-message ${fileMessage.kind}`}
              role={fileMessage.kind === "error" ? "alert" : "status"}
            >
              {fileMessage.text}
            </p>
          ) : null}
        </div>
      ) : fileMessage !== null ? (
        <p
          className={`doc-file-notice-message ${fileMessage.kind}`}
          role={fileMessage.kind === "error" ? "alert" : "status"}
        >
          {fileMessage.text}
        </p>
      ) : null}
      {!quiet ? <ReaderStateBanner input={bannerInput} /> : null}
      <ReaderToolbarExtrasProvider
        state={{
          showPrefs,
          prefsOpen,
          onTogglePrefs: () => setPrefsOpen((open) => !open),
          prefs,
          onPrefs: setPrefs,
          onResetPrefs: resetPrefs,
          prefsBarRef,
          briefFormat,
          onBriefFormat: setBriefFormat,
          briefBusy,
          briefNotice,
          onExportBrief: () => void exportBrief(),
        }}
      >
        {/* M03 正文渲染:格式分发与字节读取均属 M03;位置恢复按文档格式分发。 */}
        <div className="reader-mount" data-mount="reader-renderer" aria-label="正文渲染区">
          {plan.data ? (
            <DocumentBody
              client={client}
              plan={plan.data}
              initialPage={
                position.data?.locator.kind === "pdf" ? position.data.locator.pageNumber : 1
              }
              initialLocator={
                position.isFetched ? position.data?.locator ?? null : undefined
              }
              prefs={prefs}
              documentTitle={detail.data?.displayName}
              onViewportChange={(viewport) =>
                useWorkspaceUi.getState().setReaderViewport(documentId, viewport)
              }
            />
          ) : plan.isError ? (
            <div role="alert" className="reader-body-status">
              <p>正文加载失败,已保存数据不受影响。</p>
              <button type="button" className="btn" onClick={() => void plan.refetch()}>
                重试
              </button>
            </div>
          ) : (
            <p className="reader-body-status" role="status">
              正文准备中…
            </p>
          )}
        </div>
      </ReaderToolbarExtrasProvider>
      {documentId !== null &&
      activeVersionId !== null &&
      (ocrStatus.data ?? []).some((row) => row.status !== "skipped") ? (
        <div className="reader-ocr-progress">
          <PageOcrProgressPanel
            client={client}
            documentId={documentId}
            versionId={activeVersionId}
            parseState={parseStatus.data?.parseState}
            ocr={ocrStatus.data ?? []}
          />
        </div>
      ) : null}
      <div className="reader-coverage" aria-label="页覆盖">
        <PageCoverageBar
          parseCoverage={parseStatus.data?.coverage}
          ocr={ocrStatus.data ?? []}
        />
      </div>
    </div>
  );
}
