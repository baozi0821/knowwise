// 2D 离线 OCR 引擎:本地 worker/core/lang,300 DPI,灰度+Otsu+去噪,
// 显式请求 blocks/hOCR/TSV 以取得 bbox 与置信度。页图只在内存中停留。
import Tesseract from "tesseract.js";
import type { OcrBlock } from "./types";
import { OCR_DPI } from "./types";
import type { PdfRect } from "../../shared/generated/PdfRect";

type PdfDocLike = {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  getPage(pageNumber: number): Promise<any>;
};

type ViewportLike = {
  width: number;
  height: number;
  convertToPdfPoint(x: number, y: number): [number, number];
};

type TesseractParagraph = {
  text?: unknown;
  confidence?: unknown;
  bbox?: { x0?: unknown; y0?: unknown; x1?: unknown; y1?: unknown };
  lines?: Array<{ bbox?: { y0?: unknown; y1?: unknown } }>;
};

type TesseractPage = {
  text?: unknown;
  confidence?: unknown;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  blocks?: any[] | null;
};

export class OcrEngineError extends Error {
  constructor(
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "OcrEngineError";
  }
}

function assetUrl(path: string): string {
  return new URL(path, document.baseURI).toString();
}

function toNumber(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function clamp01(value: number): number {
  return Math.min(1, Math.max(0, value));
}

/** 灰度 + Otsu 二值化 + 3x3 去噪;不改变原始 ImageData。 */
export function preprocessForOcr(image: ImageData): ImageData {
  const { width, height, data } = image;
  const out = new ImageData(width, height);
  const target = out.data;
  const histogram = new Array<number>(256).fill(0);
  const gray = new Uint8Array(width * height);
  for (let i = 0, p = 0; i < data.length; i += 4, p += 1) {
    const value = Math.round(data[i]! * 0.299 + data[i + 1]! * 0.587 + data[i + 2]! * 0.114);
    gray[p] = value;
    histogram[value] = (histogram[value] ?? 0) + 1;
  }

  const total = width * height;
  let sum = 0;
  for (let t = 0; t < 256; t += 1) sum += t * histogram[t]!;
  let sumB = 0;
  let weightB = 0;
  let best = 0;
  let threshold = 127;
  for (let t = 0; t < 256; t += 1) {
    weightB += histogram[t]!;
    if (weightB === 0) continue;
    const weightF = total - weightB;
    if (weightF === 0) break;
    sumB += t * histogram[t]!;
    const meanB = sumB / weightB;
    const meanF = (sum - sumB) / weightF;
    const between = weightB * weightF * (meanB - meanF) * (meanB - meanF);
    if (between > best) {
      best = between;
      threshold = t;
    }
  }

  const binary = new Uint8Array(width * height);
  for (let p = 0; p < binary.length; p += 1) {
    binary[p] = gray[p]! > threshold ? 255 : 0;
  }
  // 去噪:孤立像素(8 邻域全部相反)翻转为多数色。
  const clean = binary.slice();
  for (let y = 1; y < height - 1; y += 1) {
    for (let x = 1; x < width - 1; x += 1) {
      const p = y * width + x;
      const center = binary[p]!;
      let same = 0;
      for (let dy = -1; dy <= 1; dy += 1) {
        for (let dx = -1; dx <= 1; dx += 1) {
          if (dx === 0 && dy === 0) continue;
          if (binary[p + dy * width + dx] === center) same += 1;
        }
      }
      if (same === 0) clean[p] = center === 0 ? 255 : 0;
    }
  }
  for (let p = 0; p < clean.length; p += 1) {
    const offset = p * 4;
    target[offset] = clean[p]!;
    target[offset + 1] = clean[p]!;
    target[offset + 2] = clean[p]!;
    target[offset + 3] = 255;
  }
  return out;
}

/** 以文档 URL 为基准的本地资产路径;运行时不访问 CDN。 */
export function ocrAssetPaths(): {
  workerPath: string;
  corePath: string;
  langPath: string;
} {
  return {
    workerPath: assetUrl("ocr/worker.min.js"),
    corePath: assetUrl("ocr/core"),
    langPath: assetUrl("ocr/lang"),
  };
}

async function imageDataToPngBlob(image: ImageData): Promise<Blob> {
  if (typeof OffscreenCanvas !== "undefined") {
    const canvas = new OffscreenCanvas(image.width, image.height);
    const context = canvas.getContext("2d");
    if (!context) throw new OcrEngineError("OCR_RENDER_FAILED", "无法创建 OCR 画布");
    context.putImageData(image, 0, 0);
    return await canvas.convertToBlob({ type: "image/png" });
  }
  const canvas = document.createElement("canvas");
  canvas.width = image.width;
  canvas.height = image.height;
  const context = canvas.getContext("2d");
  if (!context) throw new OcrEngineError("OCR_RENDER_FAILED", "无法创建 OCR 画布");
  context.putImageData(image, 0, 0);
  const blob = await new Promise<Blob | null>((resolve) =>
    canvas.toBlob((value) => resolve(value), "image/png"),
  );
  canvas.width = 0;
  canvas.height = 0;
  if (!blob) throw new OcrEngineError("OCR_RENDER_FAILED", "OCR 图像编码失败");
  return blob;
}

export type OcrSession = {
  recognize(
    image: ImageData,
    viewport: ViewportLike,
  ): Promise<{ blocks: OcrBlock[]; confidence: number }>;
  dispose(): Promise<void>;
};

export async function createOcrSession(): Promise<OcrSession> {
  const paths = ocrAssetPaths();
  let worker: Tesseract.Worker | null = null;
  try {
    worker = await Tesseract.createWorker(["chi_sim", "eng"], 1, {
      ...paths,
      workerBlobURL: false,
      gzip: true,
      legacyCore: false,
      legacyLang: false,
      errorHandler: () => {
        // 不把 provider/页图内容写入普通日志。
      },
    });
    await worker.setParameters({
      tessedit_pageseg_mode: Tesseract.PSM.AUTO,
      preserve_interword_spaces: "1",
      user_defined_dpi: String(OCR_DPI),
    });
  } catch {
    if (worker) await worker.terminate().catch(() => {});
    throw new OcrEngineError("OCR_ENGINE_UNAVAILABLE", "本地 OCR 引擎未能启动");
  }

  const active = worker;
  return {
    async recognize(image, viewport) {
      let data: TesseractPage;
      try {
        // Tesseract v6 在 WebView 中直传 ImageData 会写入坏文件;
        // 统一编码为 PNG Blob(仍在内存,不落库)。
        const blob = await imageDataToPngBlob(image);
        const result = await active.recognize(blob, { rotateAuto: true }, {
          blocks: true,
          hocr: true,
          tsv: true,
        });
        data = result.data as unknown as TesseractPage;
      } catch {
        throw new OcrEngineError("OCR_RECOGNIZE_FAILED", "OCR 识别失败");
      }
      const confidence = clamp01(toNumber(data.confidence) / 100);
      return { blocks: ocrPageToBlocks(data, viewport), confidence };
    },
    async dispose() {
      await active.terminate().catch(() => {});
    },
  };
}

/** Tesseract 页数据 → 按段落输出的持久块(bbox 已换算到 PDF user space)。 */
export function ocrPageToBlocks(data: TesseractPage, viewport: ViewportLike): OcrBlock[] {
  const paragraphs: TesseractParagraph[] = [];
  for (const block of data.blocks ?? []) {
    for (const paragraph of block?.paragraphs ?? []) {
      paragraphs.push(paragraph as TesseractParagraph);
    }
  }
  const heights = paragraphs
    .map((p) => {
      const y0 = toNumber(p.bbox?.y0);
      const y1 = toNumber(p.bbox?.y1);
      return Math.abs(y1 - y0);
    })
    .filter((h) => h > 0)
    .sort((a, b) => a - b);
  // 取偏低中位数,避免偶数样本把标题高度抬到难以触发的阈值。
  const medianHeight = heights[Math.floor((heights.length - 1) / 2)] ?? 0;

  const blocks: OcrBlock[] = [];
  for (const paragraph of paragraphs) {
    const raw = typeof paragraph.text === "string" ? paragraph.text.trim() : "";
    const text = raw
      .replace(/\s+/g, " ")
      .replace(/(?<=[\u4e00-\u9fff])\s+(?=[\u4e00-\u9fff])/g, "")
      .trim();
    if (text.length === 0) continue;
    const x0 = toNumber(paragraph.bbox?.x0);
    const y0 = toNumber(paragraph.bbox?.y0);
    const x1 = toNumber(paragraph.bbox?.x1);
    const y1 = toNumber(paragraph.bbox?.y1);
    if (!(x1 > x0) || !(y1 > y0)) continue;
    const [px0, py0] = viewport.convertToPdfPoint(x0, y0);
    const [px1, py1] = viewport.convertToPdfPoint(x1, y1);
    const bbox: PdfRect = {
      x: Math.min(px0, px1),
      y: Math.min(py0, py1),
      width: Math.abs(px1 - px0),
      height: Math.abs(py1 - py0),
    };
    if (bbox.width <= 0 || bbox.height <= 0) continue;
    const lineCount = paragraph.lines?.length ?? 0;
    const height = y1 - y0;
    const heading =
      lineCount === 1 &&
      medianHeight > 0 &&
      height >= medianHeight * 1.15 &&
      text.length <= 80 &&
      !/[。！？.!?；;:：]$/.test(text);
    blocks.push({
      text,
      bbox,
      confidence: clamp01(toNumber(paragraph.confidence) / 100),
      kind: heading ? "heading" : "paragraph",
    });
    if (blocks.length >= 500) break;
  }
  return blocks;
}

export async function hashImage(image: ImageData): Promise<string> {
  const step = Math.max(1, Math.floor(image.data.length / 4096));
  const sampled = new Uint8Array(Math.ceil(image.data.length / step));
  let cursor = 0;
  for (let i = 0; i < image.data.length; i += step) {
    sampled[cursor] = image.data[i]!;
    cursor += 1;
  }
  const header = new TextEncoder().encode(`${image.width}x${image.height}:`);
  const payload = new Uint8Array(cursor + header.length);
  payload.set(sampled.subarray(0, cursor), 0);
  payload.set(header, cursor);
  const digest = await crypto.subtle.digest("SHA-256", payload);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

export type RenderedPageImage = {
  image: ImageData;
  viewport: ViewportLike;
  width: number;
  height: number;
};

/** PDF 页 → 300 DPI 图像(优先 OffscreenCanvas,失败回退隐藏 Canvas)。 */
export async function renderPageImage(
  pdfDoc: PdfDocLike,
  pageNumber: number,
): Promise<RenderedPageImage> {
  const page = await pdfDoc.getPage(pageNumber);
  const scale = OCR_DPI / 72;
  const viewport = page.getViewport({ scale }) as ViewportLike;
  const width = Math.max(1, Math.floor(viewport.width));
  const height = Math.max(1, Math.floor(viewport.height));
  const render = async (
    context: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D,
  ): Promise<void> => {
    await page.render({
      canvasContext: context as unknown as CanvasRenderingContext2D,
      viewport,
    }).promise;
  };
  try {
    if (typeof OffscreenCanvas !== "undefined") {
      const canvas = new OffscreenCanvas(width, height);
      const context = canvas.getContext("2d");
      if (!context) throw new Error("offscreen 2d unavailable");
      await render(context);
      const image = context.getImageData(0, 0, width, height);
      page.cleanup?.();
      return { image, viewport, width, height };
    }
  } catch {
    // WebView2 版本不支持 OffscreenCanvas/2D 时回退隐藏 Canvas。
  }
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) throw new OcrEngineError("OCR_RENDER_FAILED", "无法创建页面画布");
  await render(context);
  const image = context.getImageData(0, 0, width, height);
  page.cleanup?.();
  canvas.width = 0;
  canvas.height = 0;
  return { image, viewport, width, height };
}

export type { TesseractPage };
