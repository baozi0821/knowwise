// M04 解析 Worker 桥协议(《02》§5、《M04》§3)。
// Worker 不受信任:只能凭 parseJobToken 读取受控分块、提交有界批次;
// 批次默认 ≤32 块;Rust 校验令牌、任务、Attempt、批次序号与取消状态。
import type { BinaryChunk } from "../../shared/generated/BinaryChunk";
import type { BatchAck } from "../../shared/generated/BatchAck";
import type { ParseJobTicket } from "../../shared/generated/ParseJobTicket";
import type { ParserBatchInput } from "../../shared/generated/ParserBatchInput";

export type ParseBridgePort = {
  readInput(offset: number, length: number): Promise<BinaryChunk>;
  submitBatch(batch: ParserBatchInput): Promise<BatchAck>;
  complete(): Promise<unknown>;
  fail(errorCode: string): Promise<unknown>;
};

export type { ParseJobTicket, ParserBatchInput, BatchAck };

export const PARSER_MAX_BLOCKS_PER_BATCH = 32;
export const PARSER_BATCH_TEXT_LIMIT_BYTES = 1024 * 1024;

export function decodeBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

export function decodeUtf8(bytes: Uint8Array): string {
  return new TextDecoder("utf-8").decode(bytes);
}

/**
 * 块序列 → 有界批次(《02》§5):≤32 块且文本总量 ≤1 MiB;
 * 序列号从 1 起、逐批 +1。区域与受限对象跟随首个批次提交。
 */
export function chunkIntoBatches(
  blocks: import("../../shared/generated/ParserBlockInput").ParserBlockInput[],
  regions: import("../../shared/generated/ParserRegionInput").ParserRegionInput[] = [],
  unsupported: import("../../shared/generated/ParserUnsupportedInput").ParserUnsupportedInput[] = [],
): ParserBatchInput[] {
  const batches: ParserBatchInput[] = [];
  let current: typeof blocks = [];
  let currentBytes = 0;
  let seq = 1;
  const flush = () => {
    if (current.length === 0 && batches.length > 0) return;
    batches.push({
      batchSeq: seq,
      blocks: current,
      regions: seq === 1 ? regions : [],
      unsupportedRegions: seq === 1 ? unsupported : [],
    });
    seq += 1;
    current = [];
    currentBytes = 0;
  };
  for (const block of blocks) {
    const bytes = block.text.length;
    if (
      current.length >= PARSER_MAX_BLOCKS_PER_BATCH ||
      currentBytes + bytes > PARSER_BATCH_TEXT_LIMIT_BYTES
    ) {
      flush();
    }
    current.push(block);
    currentBytes += bytes;
  }
  if (current.length > 0 || batches.length === 0) flush();
  return batches;
}
