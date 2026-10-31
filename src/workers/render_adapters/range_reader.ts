// M03 渲染适配 Worker:PDF 字节范围传输(《M03》§3)。
// 按 1 MiB 左右分块经 reader_read_range 拉取;取消滚动后未使用的
// 范围请求;原文件只读。读取端口注入,便于测试。
import type { BinaryChunk } from "../../shared/generated/BinaryChunk";

export type RangeReadPort = {
  readRange(offset: number, length: number): Promise<BinaryChunk>;
};

export type RangeReader = {
  /** 顺序拉取全部字节(ProgressiveLoading 由 pdfjs 的 getData 支持)。 */
  readAll(): Promise<Uint8Array>;
  /** 取消未完成的拉取(视图切换/滚动离开)。 */
  cancel(): void;
};

export function createRangeReader(
  port: RangeReadPort,
  opts: { fileSize: number; chunkSize?: number },
): RangeReader {
  const chunkSize = opts.chunkSize ?? 1024 * 1024;
  let canceled = false;

  const decode = (b64: string): Uint8Array => {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
    return out;
  };

  return {
    async readAll() {
      const parts: Uint8Array[] = [];
      let offset = 0;
      while (offset < opts.fileSize && !canceled) {
        const chunk = await port.readRange(offset, Math.min(chunkSize, opts.fileSize - offset));
        if (canceled) break; // 取消后未使用的范围请求结果直接丢弃
        parts.push(decode(chunk.dataBase64));
        offset = chunk.offset + chunk.length;
        if (chunk.eof) break;
      }
      if (canceled) throw new Error("range read canceled");
      const total = parts.reduce((n, p) => n + p.length, 0);
      const out = new Uint8Array(total);
      let at = 0;
      for (const p of parts) {
        out.set(p, at);
        at += p.length;
      }
      return out;
    },
    cancel() {
      canceled = true;
    },
  };
}
