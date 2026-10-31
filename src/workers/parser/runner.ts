// M04 解析 Job 执行器:消费 parser.request 票据,完成
// 读取输入 → 解析 → 分批提交 → complete/fail 的完整桥协议(《02》§5)。
// 桥端口注入(生产为 Tauri invoke,测试为替身);PDF 页文本由注入的
// 页提供器给出(pdfjs 在渲染侧经 range transport 取页)。
import type { ParseBridgePort, ParseJobTicket } from "./protocol";
import { chunkIntoBatches, decodeBase64, decodeUtf8 } from "./protocol";
import type { ParserBatchInput } from "../../shared/generated/ParserBatchInput";
import { parseMarkdownToBlocks, markdownRegion } from "./markdown_parser";
import { parseDocxHtml } from "./docx_parser";
import { parsePdfPage, resetPdfOrdinal, type PdfPageInput } from "./pdf_parser";

export type TicketPayload = {
  ticket: ParseJobTicket;
  /** 已由宿主预转换的文本/Mammoth HTML;缺省时由 Worker 读取原始字节。 */
  input?: string;
  /** PDF:逐页文本项提供器(pdfjs getTextContent 投影)。 */
  pdfPages?: () => PdfPageInput[];
  /** 已完成且有 OCR 块的页:不重跑 OCR,也不重复提交解析批次。 */
  skipPages?: ReadonlySet<number>;
  /** PDF 文档句柄等临时资源的释放(页图不落库)。 */
  cleanup?: () => Promise<void>;
};

export type ParseJobResult =
  | { ok: true }
  | { ok: false; errorCode: string };

async function readAllInput(
  port: ParseBridgePort,
  ticket: ParseJobTicket,
): Promise<string> {
  const parts: string[] = [];
  let offset = 0;
  for (;;) {
    const chunk = await port.readInput(offset, ticket.chunkSize);
    if (chunk.length > 0) {
      parts.push(decodeUtf8(decodeBase64(chunk.dataBase64)));
    }
    offset = chunk.offset + chunk.length;
    if (chunk.eof) break;
  }
  return parts.join("");
}

/** 按格式解析 → 批次序列(纯函数,便于测试)。 */
export function buildBatches(
  format: ParseJobTicket["format"],
  input: string,
  pdfPages?: () => PdfPageInput[],
  skipPages?: ReadonlySet<number>,
): ParserBatchInput[] {
  if (format === "markdown") {
    const blocks = parseMarkdownToBlocks(input);
    return chunkIntoBatches(blocks, [markdownRegion(blocks.length)]);
  }
  if (format === "docx") {
    // input 为 Mammoth HTML 输出(Worker 内先行转换,输入字节为 docx zip)。
    const parsed = parseDocxHtml(input, []);
    return chunkIntoBatches(parsed.blocks, parsed.regions, parsed.unsupported);
  }
  if (format === "pdf") {
    if (!pdfPages) throw new Error("pdf parser requires page provider");
    resetPdfOrdinal();
    const blocks: ParserBatchInput["blocks"] = [];
    const regions: ParserBatchInput["regions"] = [];
    for (const page of pdfPages()) {
      if (skipPages?.has(page.pageNumber)) continue;
      const out = parsePdfPage(page);
      blocks.push(...out.blocks);
      regions.push(...out.regions);
    }
    return chunkIntoBatches(blocks, regions);
  }
  throw new Error(`unsupported format: ${format}`);
}

export function createParseJobRunner(port: ParseBridgePort) {
  return {
    async run(job: TicketPayload): Promise<ParseJobResult> {
      try {
        let input = job.input ?? "";
        // PDF 不整读文本:逐页提取(pdfjs 在渲染侧行 range transport)。
        if (job.ticket.format !== "pdf" && job.input === undefined) {
          input = await readAllInput(port, job.ticket);
        }
        const batches = buildBatches(
          job.ticket.format,
          input,
          job.pdfPages,
          job.skipPages,
        );
        for (const batch of batches) {
          await port.submitBatch(batch);
        }
        await port.complete();
        return { ok: true };
      } catch (e) {
        // 桥错误(取消/令牌失效)不能再 fail:仅解析阶段错误走 fail。
        const message = e instanceof Error ? e.message : String(e);
        if (/cancel|expired|invalid_request/i.test(message)) {
          return { ok: false, errorCode: "BRIDGE_REJECTED" };
        }
        try {
          await port.fail("PARSE_FAILED");
        } catch {
          // 任务已终态:忽略 fail 回执。
        }
        return { ok: false, errorCode: "PARSE_FAILED" };
      } finally {
        await job.cleanup?.().catch(() => {});
      }
    },
  };
}
