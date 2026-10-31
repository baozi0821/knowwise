// 指定页 OCR:阅读器内按需渲染所选页 → 本地 tesseract 识别 → 落库重建索引。
//
// 约束(与解析桥同一口径):
// - 页图只在内存停留,不落库、不上传;识别结果经 page_ocr_save_recognition 写入;
// - 文本层合格的页直接跳过(text_layer_ok),不制造重复 OCR 文本;
// - 可取消:页间检查 shouldStop,已识别页仍会保存;
// - 全文识别进行中时,由调用方先确认停止全文识别(同一代次只允许一个写入者)。
import type { CommandClient } from "../../app/api/client";
import { waitForActiveOcrRun } from "../../app/parserRuntime";
import type { TaskView } from "../../shared/generated/TaskView";
import type { PageOcrStatusView } from "../../shared/generated/PageOcrStatusView";
import { loadPdfParserDocumentForPages } from "../../workers/parser/pdf_pages";
import { parsePdfPage, resetPdfOrdinal } from "../../workers/parser/pdf_parser";
import { runPdfOcr } from "../../workers/ocr/pdf_ocr";
import { OCR_ENGINE_VERSION } from "../../workers/ocr/types";

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export const MAX_PAGES_PER_OCR_RUN = 50;

/** 解析“1-5,8,10”式页码输入;返回排序去重后的页码与第一条错误。 */
export function parsePageSpec(
  input: string,
  pageCount: number,
): { pages: number[]; error: string | null } {
  const normalized = input.replace(/[，、；;]/g, ",").replace(/[—–]/g, "-").trim();
  if (normalized === "") return { pages: [], error: "请输入页码,例如 1-5,8" };
  const pages = new Set<number>();
  for (const raw of normalized.split(/[\s,]+/)) {
    if (raw === "") continue;
    const range = /^(\d+)\s*-\s*(\d+)$/.exec(raw);
    if (range !== null) {
      const start = Number(range[1]);
      const end = Number(range[2]);
      if (!Number.isInteger(start) || !Number.isInteger(end) || start > end) {
        return { pages: [], error: `页码区间无效:${raw}` };
      }
      if (end - start + 1 > MAX_PAGES_PER_OCR_RUN) {
        return { pages: [], error: `一次最多识别 ${MAX_PAGES_PER_OCR_RUN} 页` };
      }
      for (let page = start; page <= end; page += 1) pages.add(page);
      continue;
    }
    if (!/^\d+$/.test(raw)) return { pages: [], error: `页码无效:${raw}` };
    pages.add(Number(raw));
  }
  const list = Array.from(pages).sort((a, b) => a - b);
  if (list.length === 0) return { pages: [], error: "请输入页码,例如 1-5,8" };
  if (list.length > MAX_PAGES_PER_OCR_RUN) {
    return { pages: [], error: `一次最多识别 ${MAX_PAGES_PER_OCR_RUN} 页` };
  }
  if (pageCount > 0 && list.some((page) => page < 1 || page > pageCount)) {
    return { pages: [], error: `页码需在 1–${pageCount} 之间` };
  }
  return { pages: list, error: null };
}

/** 页码列表 → “1-3,5”紧凑展示。 */
export function formatPageSelection(pages: readonly number[]): string {
  const sorted = Array.from(new Set(pages)).sort((a, b) => a - b);
  const parts: string[] = [];
  let index = 0;
  while (index < sorted.length) {
    const start = sorted[index]!;
    let end = start;
    while (index + 1 < sorted.length && sorted[index + 1] === end + 1) {
      index += 1;
      end = sorted[index]!;
    }
    parts.push(start === end ? String(start) : `${start}-${end}`);
    index += 1;
  }
  return parts.join(",");
}

/** 当前文档是否有活动的全文解析任务(同代次写入前必须停止)。 */
export async function findActiveParseTask(
  client: CommandClient,
  documentId: string,
): Promise<TaskView | null> {
  const page = await client.call<{ items: TaskView[] }>("task_list", {
    activeOnly: true,
    limit: 50,
  });
  return (
    page.items.find(
      (task) =>
        task.documentId === documentId &&
        task.taskType === "parse_document" &&
        (task.status === "queued" || task.status === "running"),
    ) ?? null
  );
}

/**
 * 停止当前全文识别(用户确认后调用),等待代次离开进行中状态。
 * 返回被停止的任务 ID;没有活动任务时返回 null。
 */
export async function stopActiveParse(
  client: CommandClient,
  documentId: string,
): Promise<string | null> {
  const task = await findActiveParseTask(client, documentId);
  if (task === null) return null;
  await client.call<TaskView>("parse_cancel", {
    taskId: task.id,
    expectedRevision: task.stateVersion,
  });
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const status = await client.call<{ parseState: string }>("parse_get_status", { documentId });
    if (status.parseState !== "in_progress") break;
    await sleep(250);
  }
  // 运行器可能还在完成当前页并把已识别结果落库(salvage);等它退出后再写入,
  // 保证同一代次只有一个写入者,且已识别页不丢。
  const idle = await waitForActiveOcrRun(documentId, 180_000);
  if (!idle) {
    throw new Error("全文识别仍在收尾,请稍后重试(未开始识别所选页)");
  }
  return task.id;
}

/**
 * 大于单次上限时拆成 50 页一批顺序执行（全文重识别/自动补缺页用）。
 * 每批独立落库；停止时保留已保存批次。
 */
export async function recognizePagesChunked(
  input: RecognizePagesInput,
): Promise<RecognizePagesResult> {
  const pages = Array.from(new Set(input.pageNumbers))
    .filter((page) => Number.isInteger(page) && page >= 1)
    .sort((a, b) => a - b);
  const chunks: number[][] = [];
  for (let index = 0; index < pages.length; index += MAX_PAGES_PER_OCR_RUN) {
    chunks.push(pages.slice(index, index + MAX_PAGES_PER_OCR_RUN));
  }
  const aggregate: RecognizePagesResult = {
    saved: [],
    recognized: [],
    skipped: [],
    notProcessed: [],
    alreadyDone: [],
  };
  let cursor = 0;
  let completedBase = 0;
  for (const chunk of chunks) {
    if (input.shouldStop?.()) {
      aggregate.notProcessed.push(...pages.slice(cursor));
      break;
    }
    const result = await recognizePages({
      ...input,
      pageNumbers: chunk,
      onProgress: input.onProgress
        ? (progress) =>
            input.onProgress?.({
              ...progress,
              done: completedBase + progress.done,
              total: pages.length,
            })
        : undefined,
    });
    aggregate.saved.push(...result.saved);
    aggregate.recognized.push(...result.recognized);
    aggregate.skipped.push(...result.skipped);
    aggregate.notProcessed.push(...result.notProcessed);
    aggregate.alreadyDone.push(...result.alreadyDone);
    cursor += chunk.length;
    completedBase += chunk.length;
  }
  aggregate.alreadyDone = Array.from(new Set(aggregate.alreadyDone)).sort((a, b) => a - b);
  aggregate.recognized = Array.from(new Set(aggregate.recognized)).sort((a, b) => a - b);
  aggregate.skipped = Array.from(new Set(aggregate.skipped)).sort((a, b) => a - b);
  aggregate.notProcessed = Array.from(new Set(aggregate.notProcessed)).sort((a, b) => a - b);
  return aggregate;
}

export type RecognizePagesInput = {
  client: CommandClient;
  documentId: string;
  versionId: string;
  bytes: Uint8Array;
  pageNumbers: readonly number[];
  /** 默认 true：跳过已完成/低置信且有块的页，只补缺失/失败页。 */
  skipCompleted?: boolean;
  /** 默认 false：显式重识别已完成的页（会更新原块，不新增重复块）。 */
  force?: boolean;
  /** 当前解析代次；旧代次的 completed 不能当作本次已覆盖。 */
  currentGenerationId?: string;
  /** 已取到的页状态，避免重复 IPC。 */
  existingStatuses?: readonly PageOcrStatusView[];
  onProgress?: (progress: { pageNumber: number; status: string; done: number; total: number }) => void;
  shouldStop?: () => boolean;
  /** 每页开始前检查持久化暂停信号；暂停在当前页完成后生效。 */
  shouldPause?: () => Promise<boolean> | boolean;
};

export type RecognizePagesResult = {
  /** 本次落库的页状态（后端返回）。 */
  saved: PageOcrStatusView[];
  /** 本次实际识别（含无文字失败）的页。 */
  recognized: number[];
  /** 文本层合格而跳过的页。 */
  skipped: number[];
  /** 因取消/停止而未处理的页。 */
  notProcessed: number[];
  /** 因已完成而幂等跳过的页。 */
  alreadyDone: number[];
};

/**
 * 识别所选页并保存。调用前必须确保没有活动全文解析(见 stopActiveParse)。
 *
 * 默认只处理缺失/失败页：已完成的页不会被重新上报为 pending，也不会因为
 * 新范围请求而被降级。需要重识别时显式传 force=true；后端按 block_key
 * 原地更新，保留块 ID 与旧引用。
 */
let activeSelectedOcrKey: string | null = null;

/** 是否有指定页识别仍在后台运行(用于阻止并发启动第二次识别)。 */
export function hasActiveSelectedPageOcr(): boolean {
  return activeSelectedOcrKey !== null;
}

export async function recognizePages(input: RecognizePagesInput): Promise<RecognizePagesResult> {
  const { client, documentId, versionId, bytes } = input;
  if (input.pageNumbers.length === 0) {
    throw new Error("未选择页码");
  }
  const runKey = `${documentId}:${versionId}`;
  if (activeSelectedOcrKey === runKey) {
    throw new Error("上一次识别仍在后台收尾,请稍候再开始");
  }
  activeSelectedOcrKey = runKey;
  try {
    const requested = Array.from(new Set(input.pageNumbers))
      .filter((page) => Number.isInteger(page) && page >= 1)
      .sort((a, b) => a - b);
    const statuses =
      input.existingStatuses ??
      (await client.call<PageOcrStatusView[]>("page_ocr_get_status", {
        documentId,
        versionId,
      }));
    const statusByPage = new Map(statuses.map((row) => [row.pageNumber, row] as const));
    const skipCompleted = input.skipCompleted ?? true;
    const alreadyDone: number[] = [];
    const toProcess: number[] = [];
    for (const pageNumber of requested) {
      const status = statusByPage.get(pageNumber);
      if (status !== undefined && input.force !== true && skipCompleted) {
        const stale =
          input.currentGenerationId !== undefined &&
          status.generationId !== undefined &&
          status.generationId !== input.currentGenerationId;
        const hasBlocks =
          (status.status === "completed" || status.status === "low_confidence") &&
          status.blockCount > 0;
        if (!stale && (hasBlocks || status.status === "skipped")) {
          alreadyDone.push(pageNumber);
          continue;
        }
      }
      toProcess.push(pageNumber);
    }
    if (toProcess.length === 0) {
      return { saved: [], recognized: [], skipped: [], notProcessed: [], alreadyDone };
    }

    const source = await loadPdfParserDocumentForPages(bytes, toProcess);
    try {
      if (source.pages.length === 0) {
        throw new Error("所选页码不在文档范围内");
      }
      const total = source.pages.length;
      const finished = new Set<number>();
      const results = await runPdfOcr({
        pdfDoc: source.pdfDoc,
        pages: source.pages,
        shouldStop: input.shouldStop,
        shouldPause: input.shouldPause,
        report: async (entries) => {
          for (const entry of entries) {
            if (
              entry.status === "completed" ||
              entry.status === "low_confidence" ||
              entry.status === "failed" ||
              entry.status === "skipped"
            ) {
              finished.add(entry.pageNumber);
            }
            input.onProgress?.({
              pageNumber: entry.pageNumber,
              status: entry.status,
              done: finished.size,
              total,
            });
          }
        },
      });
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
      const recognized: number[] = [];
      const skipped: number[] = [];
      const notProcessed: number[] = [];
      for (const page of source.pages) {
        const result = results.get(page.pageNumber);
        if (result === undefined) {
          notProcessed.push(page.pageNumber);
          continue;
        }
        if (result.status === "skipped") {
          skipped.push(page.pageNumber);
          // 让后端把“有文本层、跳过”持久化，避免自动 OCR 反复重试同一页。
          payloadPages.push({
            pageNumber: page.pageNumber,
            status: "skipped",
            confidence: null,
            engineVersion: OCR_ENGINE_VERSION,
            errorCode: null,
            blocks: [],
          });
          continue;
        }
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
        payloadPages.push({
          pageNumber: page.pageNumber,
          // 识别结果为 0 块时如实标 failed(OCR_NO_TEXT),不把空页记成“已完成”;
          // 后端会把该页 region 标 failed,整篇仍按 partial/failed 如实呈现。
          status: blocks.length === 0 ? "failed" : result.status,
          confidence: result.confidence ?? null,
          engineVersion: OCR_ENGINE_VERSION,
          errorCode: blocks.length === 0 ? "OCR_NO_TEXT" : result.errorCode ?? null,
          blocks,
        });
        recognized.push(page.pageNumber);
      }
      if (payloadPages.length === 0) {
        return { saved: [], recognized, skipped, notProcessed, alreadyDone };
      }
      const saved = await client.call<PageOcrStatusView[]>(
        "page_ocr_save_recognition",
        {
          documentId,
          versionId,
          pages: payloadPages,
          idempotencyKey: client.newIdempotencyKey(),
        },
        { timeoutMs: 120_000 },
      );
      const savedPages = new Set(payloadPages.map((page) => page.pageNumber));
      // 命令返回的是整篇页状态(含未选页);对话框只统计本次保存的页,避免误报。
      return {
        saved: saved.filter((status) => savedPages.has(status.pageNumber)),
        recognized,
        skipped,
        notProcessed,
        alreadyDone,
      };
    } finally {
      await source.pdfDoc.destroy?.();
    }
  } finally {
    if (activeSelectedOcrKey === runKey) activeSelectedOcrKey = null;
  }
}
