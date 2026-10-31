// M15 前端图片附件:压缩、字节预算与上传。
//
// 约束(与服务端一致):长边 ≤1568、JPEG q80、单张 ≤1.5MB、每条消息最多 3 张。
// 图片以 data URL 仅经 IPC 传输;消息正文只保存 attachment:<id> 引用。
import type { CommandClient } from "../../app/api/client";
import type { AttachmentView } from "../../shared/generated/AttachmentView";

export const MAX_ATTACHMENTS = 3;
export const MAX_ATTACHMENT_BYTES = 1_500_000;
export const MAX_TOTAL_ATTACHMENT_BYTES = 5_000_000;
export const MAX_IMAGE_EDGE = 1568;
export const JPEG_QUALITY = 0.8;

export type CompressedImage = {
  dataUrl: string;
  mime: "image/jpeg";
  width: number;
  height: number;
  byteSize: number;
};

export type ComposerAttachment = {
  id: string;
  dataUrl: string;
  byteSize: number;
  name?: string;
};

/** base64 data URL 的实际字节数(不解码内容)。 */
export function dataUrlByteSize(dataUrl: string): number {
  const comma = dataUrl.indexOf(",");
  if (comma < 0) return 0;
  const encoded = dataUrl.slice(comma + 1).replace(/\s+/g, "");
  if (encoded.length === 0) return 0;
  const padding = encoded.endsWith("==") ? 2 : encoded.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor((encoded.length * 3) / 4) - padding);
}

export function totalAttachmentBytes(items: Array<{ byteSize: number }>): number {
  return items.reduce((sum, item) => sum + item.byteSize, 0);
}

function loadImage(dataUrl: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error("attachment_image_decode_failed"));
    image.src = dataUrl;
  });
}

/**
 * 用 canvas 压缩图片:长边 ≤1568、JPEG q80;若仍超过 1.5MB,
 * 逐级降低质量/尺寸直到满足预算,绝不把超限字节交给后端。
 */
export async function compressImageDataUrl(dataUrl: string): Promise<CompressedImage> {
  const image = await loadImage(dataUrl);
  const sourceWidth = image.naturalWidth || image.width;
  const sourceHeight = image.naturalHeight || image.height;
  if (sourceWidth <= 0 || sourceHeight <= 0) {
    throw new Error("attachment_image_decode_failed");
  }
  const firstScale = Math.min(1, MAX_IMAGE_EDGE / Math.max(sourceWidth, sourceHeight));
  let width = Math.max(1, Math.round(sourceWidth * firstScale));
  let height = Math.max(1, Math.round(sourceHeight * firstScale));
  const canvas = document.createElement("canvas");
  const context = canvas.getContext("2d");
  if (context === null) throw new Error("attachment_canvas_unavailable");

  const qualities = [JPEG_QUALITY, 0.7, 0.6, 0.5];
  for (let round = 0; round < 6; round += 1) {
    canvas.width = width;
    canvas.height = height;
    // 透明 PNG 转 JPEG 时以白底避免黑块。
    context.fillStyle = "#ffffff";
    context.fillRect(0, 0, width, height);
    context.drawImage(image, 0, 0, width, height);
    for (const quality of qualities) {
      const output = canvas.toDataURL("image/jpeg", quality);
      const byteSize = dataUrlByteSize(output);
      if (byteSize > 0 && byteSize <= MAX_ATTACHMENT_BYTES) {
        return {
          dataUrl: output,
          mime: "image/jpeg",
          width,
          height,
          byteSize,
        };
      }
    }
    if (Math.max(width, height) <= 640) break;
    width = Math.max(1, Math.round(width * 0.8));
    height = Math.max(1, Math.round(height * 0.8));
  }
  throw new Error("attachment_too_large");
}

/** 上传压缩后的图片;返回可随 ai_ask 发送的附件 ID。 */
export async function uploadImageAttachment(
  client: CommandClient,
  documentId: string,
  image: CompressedImage,
  name?: string,
): Promise<ComposerAttachment> {
  const view = await client.call<AttachmentView>(
    "attachment_upload",
    {
      documentId,
      dataUrl: image.dataUrl,
      width: image.width,
      height: image.height,
      name,
    },
    { timeoutMs: 60_000 },
  );
  return {
    id: view.id,
    dataUrl: image.dataUrl,
    byteSize: view.byteSize,
    name: view.name ?? name,
  };
}

/** 从用户经系统对话框选择的本地图片读取 data URL(后端只做只读导入)。 */
export async function readLocalImage(
  client: CommandClient,
  path: string,
): Promise<{ mime: string; dataUrl: string }> {
  return client.call<{ mime: string; dataUrl: string }>("attachment_import_local", { path });
}
