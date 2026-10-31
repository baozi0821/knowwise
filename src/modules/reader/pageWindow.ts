// M03 PDF 虚拟化窗口(《M03》§3):初始只请求当前页与相邻页;
// 非视口页面卸载 Canvas/TextLayer。纯函数,便于测试千页文档行为。
export type PageWindowInput = {
  /** 视口内可见的首/末页(1 基)。 */
  visibleStart: number;
  visibleEnd: number;
  pageCount: number;
  /** 当前页前后各保留的缓冲页数。 */
  buffer?: number;
};

/**
 * 计算需要渲染的页集合:可见区 ± buffer,夹在 [1, pageCount]。
 * 1000 页文档滚动时窗口大小恒定,不触发全量解析(M03-A1 的窗口预算)。
 */
export function pageWindow(input: PageWindowInput): number[] {
  const { visibleStart, visibleEnd, pageCount } = input;
  const buffer = input.buffer ?? 1;
  if (pageCount <= 0) return [];
  const start = Math.max(1, Math.min(visibleStart, visibleEnd) - buffer);
  const end = Math.min(pageCount, Math.max(visibleStart, visibleEnd) + buffer);
  const pages: number[] = [];
  for (let p = start; p <= end; p += 1) pages.push(p);
  return pages;
}

/**
 * 引用跳转的等待目标页(《M03》§3.2):先滚动到目标页所在窗口,
 * Canvas/TextLayer ready 后再应用高亮;等待有明确超时。
 */
export function windowForTarget(
  targetPage: number,
  pageCount: number,
  buffer = 1,
): number[] {
  return pageWindow({
    visibleStart: targetPage,
    visibleEnd: targetPage,
    pageCount,
    buffer,
  });
}

/** 等待 ready 的带超时轮询(引用跳转用;超时给失败而非错误高亮)。 */
export async function waitForReady(
  isReady: () => boolean,
  opts: { timeoutMs?: number; pollMs?: number } = {},
): Promise<boolean> {
  const timeoutMs = opts.timeoutMs ?? 5000;
  const pollMs = opts.pollMs ?? 50;
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (isReady()) return true;
    if (Date.now() >= deadline) return false;
    await new Promise((r) => setTimeout(r, pollMs));
  }
}
