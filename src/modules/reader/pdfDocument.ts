// M03 拥有:PDF 文档加载与文本层提取(pdf.js,仅 PDF 渲染时动态加载)。
// 输出每页文本行供虚拟化页面渲染与显示搜索复用;扫描页(无文本层)
// 产出空行数组,由视图如实呈现"无文本层",不产生伪文本(M04-A5 同源)。
export type PdfPageText = {
  pageNumber: number;
  lines: string[];
  /** scale=1 视口尺寸(user space 点);供占位高度与跳转精确计算。 */
  width?: number;
  height?: number;
};

export type PdfDocBundle = {
  pages: PdfPageText[];
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  pdfDoc?: any;
};

let workerConfigured = false;

export async function loadPdfjs() {
  const pdfjs = await import("pdfjs-dist");
  if (!workerConfigured) {
    const workerUrl = (await import("pdfjs-dist/build/pdf.worker.min.mjs?url")).default;
    pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;
    workerConfigured = true;
  }
  return pdfjs;
}

/** 字节 → 完整 PDF 文档实例与每页文本行。 */
export async function loadPdfDocument(bytes: Uint8Array): Promise<PdfDocBundle> {
  const pdfjs = await loadPdfjs();
  const doc = await pdfjs.getDocument({ data: bytes.slice() }).promise;
  const pages: PdfPageText[] = [];
  for (let p = 1; p <= doc.numPages; p += 1) {
    const page = await doc.getPage(p);
    const viewport = page.getViewport({ scale: 1 });
    const content = await page.getTextContent();
    const lines: string[] = [];
    let current = "";
    for (const item of content.items) {
      if (!("str" in item)) continue;
      current += item.str;
      if (item.hasEOL) {
        lines.push(current);
        current = "";
      }
    }
    if (current.trim().length > 0) lines.push(current);
    pages.push({
      pageNumber: p,
      lines,
      width: viewport.width,
      height: viewport.height,
    });
    page.cleanup();
  }
  return { pages, pdfDoc: doc };
}

/** 兼容旧接口:字节 → 每页文本行。 */
export async function extractPdfPageTexts(bytes: Uint8Array): Promise<PdfPageText[]> {
  const bundle = await loadPdfDocument(bytes);
  return bundle.pages;
}
