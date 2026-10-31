// M03 拥有:渲染字节读取(《M03》§3)。经 CommandClient 走 reader_read_range,
// 令牌来自渲染计划;PDF 分块 range 传输,Markdown/DOCX 复用同一端口整读。
// 原文件只读;取消由查询卸载时丢弃。
import type { CommandClient } from "../../app/api/client";
import type { BinaryChunk } from "../../shared/generated/BinaryChunk";
import type { RenderPlan } from "../../shared/generated/RenderPlan";
import { createRangeReader } from "../../workers/render_adapters/range_reader";

export async function readAllDocumentBytes(
  client: CommandClient,
  plan: RenderPlan,
): Promise<Uint8Array> {
  const token = plan.accessToken?.token;
  if (token === undefined) {
    throw new Error("渲染计划缺少访问令牌");
  }
  const reader = createRangeReader(
    {
      readRange: (offset, length) =>
        client.call<BinaryChunk>("reader_read_range", {
          accessToken: token,
          offset,
          length,
        }),
    },
    { fileSize: plan.fileSize, chunkSize: plan.chunkSize },
  );
  return reader.readAll();
}

export function bytesToText(bytes: Uint8Array): string {
  return new TextDecoder("utf-8").decode(bytes);
}
