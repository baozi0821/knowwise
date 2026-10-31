// M03 拥有:正文渲染容器(格式分发)。字节经 useQuery 按版本缓存(不可变,
// staleTime 无限);DOCX 的 mammoth 转换与 PDF 真实渲染各自独立查询。
// 失败呈现可重试状态,不伪装加载中(《M03》§8);MD/DOCX/PDF 的阅读位置
// 均经 createPositionSaver 防抖 900ms 保存,卸载时 flush;打开时按保存的
// locator 恢复(PDF 页、MD/DOCX 精确偏移 → 标题降级)。
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState, type ReactNode } from "react";
import DOMPurify from "dompurify";
import type { CommandClient } from "../../app/api/client";
import type { RenderPlan } from "../../shared/generated/RenderPlan";
import type { PageOcrStatusView } from "../../shared/generated/PageOcrStatusView";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import { bytesToText, readAllDocumentBytes } from "./documentBytes";
import { loadPdfDocument } from "./pdfDocument";
import { createPositionSaver, type PositionSaver } from "./position";
import type { ReaderPrefs } from "./readerPrefs";
import { DocxHtmlView, MarkdownTextView, PdfPagesView } from "./views";

/** DOCX 字节 → mammoth HTML → 净化(mammoth 仅在 DOCX 渲染时动态加载)。 */
async function docxBytesToCleanHtml(bytes: Uint8Array): Promise<string> {
  const mammoth = await import("mammoth");
  const converted = await mammoth.convertToHtml({
    arrayBuffer: bytes.slice().buffer as ArrayBuffer,
  });
  return DOMPurify.sanitize(converted.value, {
    ALLOWED_TAGS: [
      "p", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote",
      "table", "thead", "tbody", "tr", "td", "th", "strong", "em", "a",
      "br", "hr", "span", "code", "pre", "img",
    ],
    ALLOWED_ATTR: ["href", "src", "alt", "colspan", "rowspan"],
  });
}

export function DocumentBody({
  client,
  plan,
  initialPage = 1,
  initialLocator,
  prefs,
  documentTitle,
  onViewportChange,
}: {
  client: CommandClient;
  plan: RenderPlan;
  initialPage?: number;
  /** 上次保存的位置;undefined 表示位置查询未返回,null 表示没有保存位置。 */
  initialLocator?: SourceLocator | null;
  /** 阅读偏好(仅 Markdown/DOCX 生效)。 */
  prefs?: ReaderPrefs;
  /** 文档名(划词「复制为引用」的出处)。 */
  documentTitle?: string;
  /** 当前视口(仅 PDF 有页码);供右栏“当前页 ±N”提问范围使用。 */
  onViewportChange?: (viewport: { pageNumber?: number; pageCount?: number; headingPath?: string[] }) => void;
}): ReactNode {
  const bytes = useQuery({
    queryKey: ["reader", "bytes", plan.versionId],
    queryFn: () => readAllDocumentBytes(client, plan),
    staleTime: Infinity,
    retry: 1,
  });

  // 阅读位置防抖保存(《M03》§7):PDF 按页、MD/DOCX 按块 offset + headingPath;
  // 卸载 flush 未落盘位置。只有命令成功返回后才算已保存(由 saver 状态承载)。
  // 保存参数取 input 自带的文档/版本(而非首帧闭包),切换标签页后仍写到正确文档。
  const saverRef = useRef<PositionSaver | null>(null);
  if (saverRef.current === null) {
    saverRef.current = createPositionSaver((input) =>
      client.call("reader_save_position", {
        documentId: input.documentId,
        versionId: input.versionId,
        idempotencyKey: client.newIdempotencyKey(),
        locator: input.locator,
      }),
    );
  }
  useEffect(() => {
    const saver = saverRef.current;
    return () => {
      void saver?.flush();
    };
  }, []);

  // 切换文档/版本前立即 flush 上一篇未落盘的位置(避免读到旧位置)。
  const lastTargetRef = useRef(`${plan.documentId}:${plan.versionId}`);
  useEffect(() => {
    const target = `${plan.documentId}:${plan.versionId}`;
    if (lastTargetRef.current === target) return;
    lastTargetRef.current = target;
    void saverRef.current?.flush();
  }, [plan.documentId, plan.versionId]);

  /** MD/DOCX 滚动位置 → 防抖保存(仅当前文档/本版本)。 */
  const notifyPosition = (locator: SourceLocator): void => {
    saverRef.current?.notify({
      documentId: plan.documentId,
      versionId: plan.versionId,
      locator,
    });
  };
  // 恢复 locator:undefined=位置查询未返回(视图等待),null=无保存位置。
  const restoreLocator = initialLocator;
  // 切换文档/版本时视图整体重建:恢复与滚动跟踪不会串到上一篇文档。
  const viewKey = `${plan.documentId}:${plan.versionId}`;

  const docxHtml = useQuery({
    queryKey: ["reader", "docx-html", plan.versionId],
    queryFn: () => docxBytesToCleanHtml(bytes.data!),
    enabled: plan.format === "docx" && bytes.data !== undefined,
    staleTime: Infinity,
    retry: 1,
    // 同上:重新净化一遍 DOCX HTML 在长文档上要几十毫秒。
    gcTime: 10 * 60_000,
  });

  const pdfBundle = useQuery({
    queryKey: ["reader", "pdf-bundle", plan.versionId],
    queryFn: () => loadPdfDocument(bytes.data!),
    enabled: plan.format === "pdf" && bytes.data !== undefined,
    staleTime: Infinity,
    retry: 1,
    // 切回已打开过的文档时不重新解码 PDF:整篇重排一次是切文档卡顿的
    // 主要成本之一,而 pdf.js 文档对象本身可安全复用。
    gcTime: 10 * 60_000,
  });

  const queryClient = useQueryClient();
  const [ocrBusy, setOcrBusy] = useState(false);
  const [ocrError, setOcrError] = useState<string | null>(null);
  const pageOcr = useQuery({
    queryKey: ["page-ocr", plan.versionId],
    queryFn: () =>
      client.call<PageOcrStatusView[]>("page_ocr_get_status", {
        documentId: plan.documentId,
        versionId: plan.versionId,
      }),
    enabled: plan.format === "pdf",
    staleTime: 3_000,
    refetchInterval: (query) =>
      (query.state.data ?? []).some(
        (row) =>
          row.status === "pending" ||
          row.status === "rendering" ||
          row.status === "ocr_running",
      )
        ? 1_500
        : false,
  });
  const ocrByPage = new Map(
    (pageOcr.data ?? []).map((row) => [row.pageNumber, row] as const),
  );

  async function reprocessOcr(): Promise<void> {
    setOcrBusy(true);
    setOcrError(null);
    try {
      await client.call("page_ocr_reprocess", {
        documentId: plan.documentId,
        versionId: plan.versionId,
        idempotencyKey: client.newIdempotencyKey(),
      });
      await queryClient.invalidateQueries({ queryKey: ["page-ocr", plan.versionId] });
    } catch (error) {
      setOcrError(error instanceof Error ? error.message : "重新识别提交失败");
    } finally {
      setOcrBusy(false);
    }
  }

  if (bytes.isLoading) {
    return (
      <p role="status" className="reader-body-status">
        正文加载中…(约 {Math.max(1, Math.round(plan.fileSize / 1024))} KB)
      </p>
    );
  }
  if (bytes.isError || bytes.data === undefined) {
    return (
      <div role="alert" className="reader-body-status">
        <p>正文加载失败;已保存数据不受影响。</p>
        <button type="button" onClick={() => void bytes.refetch()}>
          重试
        </button>
      </div>
    );
  }

  switch (plan.format) {
    case "markdown":
      return (
        <MarkdownTextView
          key={viewKey}
          text={bytesToText(bytes.data)}
          documentId={plan.documentId}
          versionId={plan.versionId}
          client={client}
          documentTitle={documentTitle}
          prefs={prefs}
          initialLocator={restoreLocator}
          onPositionChange={notifyPosition}
          onViewportChange={(next) => onViewportChange?.({ headingPath: next.headingPath })}
        />
      );
    case "docx":
      if (docxHtml.isLoading) {
        return (
          <p role="status" className="reader-body-status">
            正文转换中…
          </p>
        );
      }
      if (docxHtml.isError || docxHtml.data === undefined) {
        return (
          <div role="alert" className="reader-body-status">
            <p>DOCX 转换失败;原文件不受影响。</p>
            <button type="button" onClick={() => void docxHtml.refetch()}>
              重试
            </button>
          </div>
        );
      }
      return (
        <DocxHtmlView
          key={viewKey}
          html={docxHtml.data}
          documentId={plan.documentId}
          versionId={plan.versionId}
          client={client}
          documentTitle={documentTitle}
          prefs={prefs}
          initialLocator={restoreLocator}
          onPositionChange={notifyPosition}
          onViewportChange={(next) => onViewportChange?.({ headingPath: next.headingPath })}
        />
      );
    case "pdf":
      if (bytes.isLoading || pdfBundle.isLoading) {
        return (
          <p role="status" className="reader-body-status">
            PDF 文本层提取中…
          </p>
        );
      }
      if (pdfBundle.isError || pdfBundle.data === undefined) {
        return (
          <div role="alert" className="reader-body-status">
            <p>
              {bytes.isError
                ? "PDF 字节读取失败;阅读可重试,不影响已保存数据。"
                : "PDF 解析失败;阅读可重试,不影响已保存数据。"}
            </p>
            <button
              type="button"
              onClick={() => {
                // 字节查询失败时只重取 pdfBundle 是空操作;先重取字节再解码。
                void bytes.refetch().then((result) => {
                  if (result.data !== undefined) void pdfBundle.refetch();
                });
              }}
            >
              重试
            </button>
          </div>
        );
      }
      return (
        <>
          {ocrError ? (
            <p className="reader-ocr-error" role="alert">
              重新识别未提交:{ocrError}
            </p>
          ) : null}
        <PdfPagesView
          key={viewKey}
          pages={pdfBundle.data.pages}
          pdfDoc={pdfBundle.data.pdfDoc}
          initialPage={initialPage}
          documentId={plan.documentId}
          versionId={plan.versionId}
          client={client}
          documentTitle={documentTitle}
          pageOcr={ocrByPage}
          onReprocessOcr={() => void reprocessOcr()}
          ocrBusy={ocrBusy}
          pdfBytes={bytes.data ?? null}
          onVisiblePageChange={(pageNumber) => {
            onViewportChange?.({
              pageNumber,
              pageCount: pdfBundle.data.pages.length,
            });
            saverRef.current?.notify({
              documentId: plan.documentId,
              versionId: plan.versionId,
              locator: {
                kind: "pdf",
                pageNumber,
                rects: [],
                pageRotation: "0",
                coordinateSpace: "user",
              },
            });
          }}
        />
        </>
      );
  }
}
