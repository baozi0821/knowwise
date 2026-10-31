// M03 显示搜索(《M03》§3.1):按需读取页面文本,逐页异步搜索;
// 新查询出现时取消旧搜索;只维护内存结果,不写 ContentBlock、不走 FTS。
export type ReaderSearchResult = {
  pageNumber: number;
  /** 命中附近摘要(前后各约 24 字符)。 */
  snippet: string;
  matchIndex: number;
};

export type SearchState = {
  results: ReaderSearchResult[];
  running: boolean;
  /** 被新查询取代的查询(已取消)。 */
  superseded: boolean;
};

export type PageTextSource = (
  pageNumber: number,
) => Promise<string | null>;

/** 不区分大小写与折叠空白的子串匹配;返回页内全部命中。 */
export function findMatches(pageText: string, query: string): number[] {
  const q = query.trim();
  if (q.length === 0) return [];
  const hay = pageText.replace(/\s+/g, " ").toLowerCase();
  const needle = q.replace(/\s+/g, " ").toLowerCase();
  const hits: number[] = [];
  let from = 0;
  for (;;) {
    const at = hay.indexOf(needle, from);
    if (at === -1) break;
    hits.push(at);
    from = at + Math.max(needle.length, 1);
  }
  return hits;
}

export function snippetAround(text: string, index: number, radius = 24): string {
  const flat = text.replace(/\s+/g, " ");
  const start = Math.max(0, index - radius);
  const end = Math.min(flat.length, index + radius);
  const prefix = start > 0 ? "…" : "";
  const suffix = end < flat.length ? "…" : "";
  return `${prefix}${flat.slice(start, end)}${suffix}`;
}

/**
 * 创建可取消的逐页搜索。run() 每次调用使先前结果的归属失效
 * (epoch 递增);旧查询的迟到页结果被丢弃。
 */
export function createReaderSearch(fetchPageText: PageTextSource) {
  let epoch = 0;
  return {
    async run(
      query: string,
      pageNumbers: number[],
      limit = 100,
    ): Promise<SearchState> {
      const mine = ++epoch;
      const results: ReaderSearchResult[] = [];
      for (const pageNumber of pageNumbers) {
        // 逐页异步:新查询已启动时立即停止旧遍历(取消未用页的读取)。
        if (mine !== epoch) {
          return { results, running: false, superseded: true };
        }
        const text = await fetchPageText(pageNumber);
        if (mine !== epoch) {
          return { results, running: false, superseded: true };
        }
        if (text === null) continue;
        for (const matchIndex of findMatches(text, query)) {
          results.push({
            pageNumber,
            snippet: snippetAround(text, matchIndex),
            matchIndex,
          });
          if (results.length >= limit) {
            return { results, running: false, superseded: false };
          }
        }
      }
      return { results, running: false, superseded: false };
    },
    /** 主动取消进行中的查询(如用户关闭搜索栏)。 */
    cancel(): void {
      epoch += 1;
    },
  };
}
