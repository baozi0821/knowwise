// 文档内搜索(Acrobat Ctrl+F 式):PDF 按页文本命中列表跳转;
// Markdown/DOCX 遍历文本节点,全部命中以 CSS Highlight 标亮并可逐个跳转。
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Search, ChevronUp, ChevronDown, X } from "lucide-react";
import { findMatches, snippetAround } from "./search";
import type { PdfPageText } from "./pdfDocument";

type PdfHit = { pageNumber: number; snippet: string };

function PdfSearchResults({
  pages,
  query,
  onJump,
}: {
  pages: PdfPageText[];
  query: string;
  onJump: (pageNumber: number) => void;
}): ReactNode {
  const hits = useMemo<PdfHit[]>(() => {
    const q = query.trim();
    if (q.length === 0) return [];
    const out: PdfHit[] = [];
    for (const page of pages) {
      const text = page.lines.join("\n");
      for (const at of findMatches(text, q).slice(0, 3)) {
        out.push({ pageNumber: page.pageNumber, snippet: snippetAround(text, at) });
      }
      if (out.length > 60) break;
    }
    return out;
  }, [pages, query]);

  if (query.trim().length === 0) {
    return <p className="reader-search-hint">输入关键词,在全文中逐页查找。</p>;
  }
  if (hits.length === 0) {
    return <p className="reader-search-hint">没有找到「{query.trim()}」。</p>;
  }
  return (
    <ol className="reader-search-list" role="listbox" aria-label="搜索结果">
      {hits.slice(0, 40).map((h, i) => (
        <li key={i}>
          <button type="button" role="option" onClick={() => onJump(h.pageNumber)}>
            <span className="reader-search-page">第 {h.pageNumber} 页</span>
            <span className="reader-search-snippet">{h.snippet}</span>
          </button>
        </li>
      ))}
    </ol>
  );
}

function TextSearchResults({
  rootRef,
  query,
  register,
}: {
  rootRef: React.RefObject<HTMLElement | null>;
  query: string;
  register: (ranges: Range[]) => void;
}): ReactNode {
  useEffect(() => {
    const root = rootRef.current;
    if (root === null) {
      register([]);
      return;
    }
    const q = query.trim().toLowerCase();
    if (q.length === 0) {
      register([]);
      return;
    }
    const ranges: Range[] = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node !== null && ranges.length < 200) {
      const text = node.textContent ?? "";
      const lower = text.toLowerCase();
      let from = 0;
      for (;;) {
        const at = lower.indexOf(q, from);
        if (at === -1) break;
        try {
          const range = document.createRange();
          range.setStart(node, at);
          range.setEnd(node, at + q.length);
          ranges.push(range);
        } catch {
          // 跨界偏移忽略。
        }
        from = at + q.length;
      }
      node = walker.nextNode();
    }
    register(ranges);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query, rootRef]);

  return null;
}

/** 搜索条:输入 + 命中计数 + 上/下一个 + 结果列表。 */
export function ReaderSearch({
  open,
  onClose,
  mode,
  pages,
  rootRef,
  onJumpPage,
}: {
  open: boolean;
  onClose: () => void;
  mode: "pdf" | "text";
  pages?: PdfPageText[];
  rootRef?: React.RefObject<HTMLElement | null>;
  onJumpPage?: (pageNumber: number) => void;
}): ReactNode {
  const [query, setQuery] = useState("");
  const [ranges, setRanges] = useState<Range[]>([]);
  const [current, setCurrent] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) inputRef.current?.focus();
    else {
      setQuery("");
      setRanges([]);
      setCurrent(0);
    }
  }, [open]);

  const register = useCallback((next: Range[]) => {
    setRanges(next);
    setCurrent(0);
    const api = (window as unknown as { CSS?: { highlights?: Map<string, unknown> } }).CSS
      ?.highlights;
    if (api === undefined) return;
    const HighlightCtor = (
      window as unknown as { Highlight?: new (...r: Range[]) => unknown }
    ).Highlight;
    if (HighlightCtor === undefined) return;
    api.delete("kw-search");
    if (next.length > 0) api.set("kw-search", new HighlightCtor(...next));
  }, []);

  useEffect(() => {
    return () => {
      const api = (window as unknown as { CSS?: { highlights?: Map<string, unknown> } }).CSS
        ?.highlights;
      api?.delete("kw-search");
    };
  }, []);

  const goto = useCallback(
    (index: number) => {
      if (ranges.length === 0) return;
      const next = ((index % ranges.length) + ranges.length) % ranges.length;
      setCurrent(next);
      const range = ranges[next]!;
      const el =
        range.startContainer instanceof Element
          ? range.startContainer
          : range.startContainer.parentElement;
      el?.scrollIntoView({ behavior: "smooth", block: "center" });
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(range.cloneRange());
    },
    [ranges],
  );

  if (!open) return null;

  return (
    <div className="reader-search" role="search" aria-label="文档内搜索">
      <div className="reader-search-bar">
        <Search className="w-3.5 h-3.5" aria-hidden />
        <input
          ref={inputRef}
          type="search"
          value={query}
          placeholder="在本文档中搜索…"
          aria-label="搜索关键词"
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              if (mode === "text") goto(current + 1);
            }
            if (e.key === "Escape") onClose();
          }}
        />
        {mode === "text" && query.trim().length > 0 ? (
          <>
            <span className="reader-search-count">
              {ranges.length === 0 ? 0 : current + 1}/{ranges.length}
            </span>
            <button
              type="button"
              className="icon-btn small"
              aria-label="上一个"
              onClick={() => goto(current - 1)}
            >
              <ChevronUp className="w-3.5 h-3.5" />
            </button>
            <button
              type="button"
              className="icon-btn small"
              aria-label="下一个"
              onClick={() => goto(current + 1)}
            >
              <ChevronDown className="w-3.5 h-3.5" />
            </button>
          </>
        ) : null}
        <button type="button" className="icon-btn small" aria-label="关闭搜索" onClick={onClose}>
          <X className="w-3.5 h-3.5" />
        </button>
      </div>
      <div className="reader-search-body" ref={listRef}>
        {mode === "pdf" && pages !== undefined ? (
          <PdfSearchResults
            pages={pages}
            query={query}
            onJump={(pageNumber) => onJumpPage?.(pageNumber)}
          />
        ) : null}
        {mode === "text" && rootRef !== undefined ? (
          <>
            <TextSearchResults rootRef={rootRef} query={query} register={register} />
            {query.trim().length > 0 ? (
              <p className="reader-search-hint">
                {ranges.length === 0 ? `没有找到「${query.trim()}」。` : `共 ${ranges.length} 处命中,Enter 跳到下一处。`}
              </p>
            ) : (
              <p className="reader-search-hint">输入关键词,命中处将全部标亮。</p>
            )}
          </>
        ) : null}
      </div>
    </div>
  );
}
