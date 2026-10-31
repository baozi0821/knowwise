// M04 解析宿主:PDF 字节 → pdf.js 文本项投影(PdfPageInput)与文档句柄。
// 文本层提取不渲染;2D OCR 需要时复用同一文档句柄按页渲染(300 DPI)。
import { loadPdfjs } from "../../modules/reader/pdfDocument";
import type { PdfPageInput } from "./pdf_parser";

type PdfjsTextItem = {
  str?: unknown;
  transform?: unknown;
  width?: unknown;
  height?: unknown;
  dir?: unknown;
};

export type PdfParserSource = {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  pdfDoc: any;
  pages: PdfPageInput[];
};

function toPdfItem(item: PdfjsTextItem) {
  if (typeof item.str !== "string" || item.str.trim().length === 0) return null;
  if (!Array.isArray(item.transform) || item.transform.length < 6) return null;
  const transform = item.transform.slice(0, 6).map((v) => Number(v));
  if (transform.some((v) => !Number.isFinite(v))) return null;
  return {
    str: item.str,
    transform: transform as [number, number, number, number, number, number],
    width: typeof item.width === "number" ? item.width : 0,
    height: typeof item.height === "number" ? item.height : 0,
    dir: item.dir === "rtl" ? ("rtl" as const) : ("ltr" as const),
  };
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function loadPageInput(pdfDoc: any, pageNumber: number): Promise<PdfPageInput> {
  const page = await pdfDoc.getPage(pageNumber);
  const viewport = page.getViewport({ scale: 1 });
  const content = await page.getTextContent();
  const items: NonNullable<ReturnType<typeof toPdfItem>>[] = [];
  for (const raw of content.items as unknown[]) {
    const parsed = toPdfItem(raw as PdfjsTextItem);
    if (parsed !== null) items.push(parsed);
  }
  const input: PdfPageInput = {
    pageNumber,
    items,
    rotation: typeof page.rotate === "number" ? page.rotate : 0,
    width: viewport.width,
    height: viewport.height,
  };
  page.cleanup();
  return input;
}

export async function loadPdfParserDocument(bytes: Uint8Array): Promise<PdfParserSource> {
  const pdfjs = await loadPdfjs();
  const pdfDoc = await pdfjs.getDocument({ data: bytes.slice() }).promise;
  const pages: PdfPageInput[] = [];
  for (let pageNumber = 1; pageNumber <= pdfDoc.numPages; pageNumber += 1) {
    pages.push(await loadPageInput(pdfDoc, pageNumber));
  }
  return { pdfDoc, pages };
}

/**
 * 只加载指定页(指定页 OCR 用):不遍历全文档文本层,
 * 大文档也能快速进入识别;页码越界会被忽略。
 */
export async function loadPdfParserDocumentForPages(
  bytes: Uint8Array,
  pageNumbers: readonly number[],
): Promise<PdfParserSource> {
  const pdfjs = await loadPdfjs();
  const pdfDoc = await pdfjs.getDocument({ data: bytes.slice() }).promise;
  const wanted = Array.from(new Set(pageNumbers))
    .filter((pageNumber) => Number.isInteger(pageNumber) && pageNumber >= 1 && pageNumber <= pdfDoc.numPages)
    .sort((a, b) => a - b);
  const pages: PdfPageInput[] = [];
  for (const pageNumber of wanted) {
    pages.push(await loadPageInput(pdfDoc, pageNumber));
  }
  return { pdfDoc, pages };
}

export async function loadPdfParserPages(bytes: Uint8Array): Promise<PdfPageInput[]> {
  const source = await loadPdfParserDocument(bytes);
  await source.pdfDoc.destroy?.();
  return source.pages;
}
