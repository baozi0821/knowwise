// M13/M04 组装:消费 `parser.request` 票据,驱动解析 Worker 完成
// 字节读取 → 格式解析 → 批次提交 → complete/fail。事件仍由 EventBridge
// 唯一订阅;本模块只注册一个 parser.request handler。
import type { CommandClient } from "./api/client";
import type { EventBridge } from "./events/bridge";
import type { BinaryChunk } from "../shared/generated/BinaryChunk";
import type { ParseJobTicket } from "../shared/generated/ParseJobTicket";
import type { PageOcrControlView } from "../shared/generated/PageOcrControlView";
import type { PageOcrStatusView } from "../shared/generated/PageOcrStatusView";
import type { ModelSettingsView } from "../shared/generated/ModelSettingsView";
import type { ParserBatchInput } from "../shared/generated/ParserBatchInput";
import type { ParseBridgePort } from "../workers/parser/protocol";
import { decodeBase64 } from "../workers/parser/protocol";
import { createParseJobRunner, type TicketPayload } from "../workers/parser/runner";
import { loadPdfParserDocument } from "../workers/parser/pdf_pages";
import { parsePdfPage, resetPdfOrdinal, type PdfPageInput } from "../workers/parser/pdf_parser";
import { runPdfOcr } from "../workers/ocr/pdf_ocr";
import { OCR_ENGINE_VERSION, type OcrPageResult, type PageOcrReportEntry } from "../workers/ocr/types";

function createPort(client: CommandClient, token: string): ParseBridgePort {
  return {
    readInput: (offset, length) =>
      client.call<BinaryChunk>("parser_read_input", {
        parseJobToken: token,
        offset,
        length,
      }),
    submitBatch: (batch: ParserBatchInput) =>
      client.call("parser_submit_batch", { parseJobToken: token, batch }),
    complete: () => client.call("parser_complete", { parseJobToken: token }, { timeoutMs: 300_000 }),
    fail: (errorCode) => client.call("parser_fail", { parseJobToken: token, errorCode }),
  };
}

async function readAllBytes(
  port: ParseBridgePort,
  ticket: ParseJobTicket,
): Promise<Uint8Array> {
  const chunks: Uint8Array[] = [];
  let offset = 0;
  for (;;) {
    const chunk = await port.readInput(offset, ticket.chunkSize);
    if (chunk.length > 0) chunks.push(decodeBase64(chunk.dataBase64));
    offset = chunk.offset + chunk.length;
    if (chunk.eof) break;
  }
  const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const out = new Uint8Array(total);
  let cursor = 0;
  for (const chunk of chunks) {
    out.set(chunk, cursor);
    cursor += chunk.length;
  }
  return out;
}

async function priorityPageFor(
  client: CommandClient,
  ticket: ParseJobTicket,
): Promise<number | undefined> {
  try {
    const position = await client.call<{
      locator?: { kind?: string; pageNumber?: number };
    } | null>("reader_get_position", { documentId: ticket.documentId });
    if (position?.locator?.kind === "pdf" && typeof position.locator.pageNumber === "number") {
      return position.locator.pageNumber;
    }
  } catch {
    // 位置不可用不影响 OCR;按页序执行。
  }
  return undefined;
}

async function parseStillRunning(
  client: CommandClient,
  documentId: string,
): Promise<boolean | null> {
  try {
    const status = await client.call<{ parseState: string }>("parse_get_status", {
      documentId,
    });
    return status.parseState === "in_progress";
  } catch {
    return null;
  }
}

/**
 * 全文 OCR 被停止时,把内存中已完成的页落库(取消后不再有批次提交)。
 * 只写已完成/低置信且有块的页;不伪造、不把空页记成可问答。
 * 取消方(指定页 OCR)会等待运行器退出后再写入,保持单写入者。
 */
async function salvageCanceledOcr(
  client: CommandClient,
  ticket: ParseJobTicket,
  pages: PdfPageInput[],
  results: Map<number, OcrPageResult>,
): Promise<number> {
  const recognized = pages.filter((page) => {
    const result = results.get(page.pageNumber);
    return (
      result !== undefined &&
      result.blocks.length > 0 &&
      (result.status === "completed" || result.status === "low_confidence")
    );
  });
  if (recognized.length === 0) return 0;
  resetPdfOrdinal();
  const payloadPages: Array<{
    pageNumber: number;
    status: string;
    confidence: number | null;
    engineVersion: string;
    errorCode: string | null;
    blocks: Array<{
      text: string;
      confidence: number | null;
      kind: string;
      locator: unknown;
    }>;
  }> = [];
  for (const page of recognized) {
    const result = results.get(page.pageNumber)!;
    page.ocr = result;
    const parsed = parsePdfPage(page);
    const blocks = parsed.blocks
      .filter((block) => block.origin === "ocr")
      .map((block) => ({
        text: block.text,
        confidence: block.ocrConfidence ?? null,
        kind: block.kind,
        locator: block.locator,
      }));
    if (blocks.length === 0) continue;
    payloadPages.push({
      pageNumber: page.pageNumber,
      status: result.status,
      confidence: result.confidence ?? null,
      engineVersion: OCR_ENGINE_VERSION,
      errorCode: result.errorCode ?? null,
      blocks,
    });
  }
  for (let index = 0; index < payloadPages.length; index += 50) {
    await client.call(
      "page_ocr_save_recognition",
      {
        documentId: ticket.documentId,
        versionId: ticket.versionId,
        pages: payloadPages.slice(index, index + 50),
        idempotencyKey: `salvage-${ticket.generationId}-${index}`,
      },
      { timeoutMs: 120_000 },
    );
  }
  return payloadPages.length;
}

async function autoOcrEnabled(client: CommandClient): Promise<boolean> {
  try {
    const view = await client.call<ModelSettingsView>("settings_get", {});
    return view.settings.autoOcr !== false;
  } catch {
    // 设置不可读时保持既有自动行为，不静默关闭 OCR。
    return true;
  }
}

async function prepareJob(
  port: ParseBridgePort,
  ticket: ParseJobTicket,
  client: CommandClient,
): Promise<TicketPayload> {
  if (ticket.format === "pdf") {
    const bytes = await readAllBytes(port, ticket);
    const source = await loadPdfParserDocument(bytes);
    const priorityPage = await priorityPageFor(client, ticket);
    // 暂停:每页开始前读取持久化控制信号;当前页完成后才生效。
    // 若任务已被取消/结束,立即停止等待,避免暂停中的运行器永久挂起。
    let stopRequested = false;
    const waitWhilePaused = async (): Promise<boolean> => {
      let control: PageOcrControlView;
      try {
        control = await client.call<PageOcrControlView>("page_ocr_control_get", {
          documentId: ticket.documentId,
          versionId: ticket.versionId,
        });
      } catch {
        return false;
      }
      if (!control.paused) return false;
      try {
        const status = await client.call<{ parseState: string }>("parse_get_status", {
          documentId: ticket.documentId,
        });
        if (status.parseState !== "in_progress") {
          stopRequested = true;
          return false;
        }
      } catch {
        // 状态不可读时不清除暂停,继续等待下一次探测。
      }
      return true;
    };
    const ocrResults = new Map<number, OcrPageResult>();
    const recognize = await autoOcrEnabled(client);
    // 当前代次已完成且有块的页不再重跑 OCR；同时跳过其解析批次提交，
    // 保留既有块 ID/内容。新代次（generationId 不同）不会命中此集合。
    const alreadyCovered = new Set<number>();
    try {
      const statuses = await client.call<PageOcrStatusView[]>("page_ocr_get_status", {
        documentId: ticket.documentId,
        versionId: ticket.versionId,
      });
      for (const row of statuses) {
        if (
          row.generationId === ticket.generationId &&
          (row.status === "completed" || row.status === "low_confidence") &&
          row.blockCount > 0
        ) {
          alreadyCovered.add(row.pageNumber);
        }
      }
    } catch {
      // 状态不可读时按未覆盖处理，最多多跑一次 OCR。
    }
    const pagesToScan = source.pages.filter((page) => !alreadyCovered.has(page.pageNumber));
    try {
      await runPdfOcr({
        pdfDoc: source.pdfDoc,
        pages: pagesToScan,
        priorityPage,
        shouldStop: () => stopRequested,
        shouldPause: waitWhilePaused,
        results: ocrResults,
        recognize,
        report: (entries: PageOcrReportEntry[]) =>
          client.call("page_ocr_report", {
            parseJobToken: ticket.token,
            entries,
          }),
      });
    } catch (error) {
      // 取消发生在页上报期间:已完成页的结果仍在 ocrResults 中,落库后再退出。
      if ((await parseStillRunning(client, ticket.documentId)) === false) {
        await salvageCanceledOcr(client, ticket, source.pages, ocrResults).catch(() => 0);
      }
      throw error;
    }
    for (const page of source.pages) {
      page.ocr = ocrResults.get(page.pageNumber) ?? {
        status: "skipped",
        attempted: false,
        blocks: [],
        reasons: ["text_layer_ok"],
      };
    }
    // 暂停期间任务被停止:运行器已退出循环,先落库已识别页,避免“有状态无块”丢失。
    if ((await parseStillRunning(client, ticket.documentId)) === false) {
      const saved = await salvageCanceledOcr(client, ticket, source.pages, ocrResults).catch(
        () => 0,
      );
      if (saved > 0) throw new Error("OCR_STOPPED");
    }
    return {
      ticket,
      pdfPages: () => source.pages,
      skipPages: alreadyCovered,
      cleanup: async () => {
        await source.pdfDoc.destroy?.();
      },
    };
  }
  if (ticket.format === "docx") {
    const bytes = await readAllBytes(port, ticket);
    const mammoth = await import("mammoth");
    const converted = await mammoth.convertToHtml({
      arrayBuffer: bytes.slice().buffer as ArrayBuffer,
    });
    return { ticket, input: converted.value };
  }
  // Markdown 等文本格式由 Worker 经受控 token 整读,避免重复解码。
  return { ticket };
}

const activeOcrRuns = new Map<string, Promise<void>>();

/**
 * 等待该文档的解析运行器退出(含取消后的 OCR 落库收尾)。
 * 指定页 OCR 在停止全文识别后调用,避免同一代次双写入。
 */
export async function waitForActiveOcrRun(
  documentId: string,
  timeoutMs = 180_000,
): Promise<boolean> {
  const run = activeOcrRuns.get(documentId);
  if (run === undefined) return true;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<boolean>((resolve) => {
    timer = setTimeout(() => resolve(false), timeoutMs);
  });
  try {
    return await Promise.race([run.then(() => true), timeout]);
  } finally {
    if (timer !== null) clearTimeout(timer);
  }
}

export function installParserRuntime(bridge: EventBridge, client: CommandClient): void {
  bridge.register({
    moduleId: "m04.parser-runtime",
    eventTypes: ["parser.request"],
    handle: (event) => {
      const ticket = event.payload as unknown as ParseJobTicket;
      if (
        ticket === null ||
        typeof ticket !== "object" ||
        typeof ticket.token !== "string" ||
        typeof ticket.format !== "string"
      ) {
        return;
      }
      const port = createPort(client, ticket.token);
      const run = (async () => {
        try {
          const job = await prepareJob(port, ticket, client);
          await createParseJobRunner(port).run(job);
        } catch {
          // 宿主预转换/pdf.js 失败也要把任务收敛为失败,而不是永久 running。
          await port.fail("PARSE_FAILED").catch(() => {});
        }
      })();
      if (ticket.format === "pdf") {
        activeOcrRuns.set(ticket.documentId, run);
        void run.then(() => {
          if (activeOcrRuns.get(ticket.documentId) === run) {
            activeOcrRuns.delete(ticket.documentId);
          }
        });
      }
    },
  });
}
