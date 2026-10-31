// 笔记 ↔ 原文双向链接:在正文右侧页边渲染小标记,不参与排版。
// 只在可见块显示;滚动只重算位置,不新增 IPC。点击标记打开并聚焦笔记。
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { NotebookPen } from "lucide-react";
import type { CommandClient } from "../../app/api/client";
import type { NoteView } from "../../shared/generated/NoteView";
import type { Page } from "../../shared/generated/Page";
import { qk } from "../../app/api/keys";
import { useReaderTools } from "./readerTools";
import { rangeFromOffsets } from "./annotations";

type Marker = {
  noteId: string;
  title: string;
  top: number;
  visible: boolean;
};

/** 取笔记正文的首行作为标记提示(不写入正文)。 */
function noteLabel(note: NoteView): string {
  const first = note.body
    .split(/\r?\n/)
    .map((line) => line.replace(/^[#>\-*\d.\s]+/, "").trim())
    .find((line) => line.length > 0);
  return (first ?? "笔记").slice(0, 36);
}

export function NoteLinksLayer({
  rootRef,
  wrapRef,
  client,
  documentId,
  versionId,
  format,
  enabled = true,
}: {
  rootRef: React.RefObject<HTMLElement | null>;
  wrapRef: React.RefObject<HTMLDivElement | null>;
  client?: CommandClient;
  documentId?: string;
  versionId?: string;
  format: "markdown" | "docx";
  enabled?: boolean;
}): ReactNode {
  const notes = useQuery({
    queryKey: qk.documentNotes(documentId ?? "none"),
    queryFn: () =>
      client!.call<Page<NoteView>>("note_list", { documentId: documentId! }),
    enabled: Boolean(client && documentId && enabled),
  });
  const [anchors, setAnchors] = useState<Array<{ note: NoteView; el: HTMLElement }>>([]);
  const [, setTick] = useState(0);
  const [hover, setHover] = useState<string | null>(null);

  // 笔记定位只在笔记数据变化时解析一次(block 元素缓存)。
  useEffect(() => {
    const root = rootRef.current;
    const items = notes.data?.items ?? [];
    if (root === null) {
      setAnchors([]);
      return;
    }
    const next: Array<{ note: NoteView; el: HTMLElement }> = [];
    for (const note of items) {
      const locator = note.locator;
      if (locator === undefined || locator === null) continue;
      let start = -1;
      let end = -1;
      if (format === "markdown") {
        if (locator.kind !== "markdown") continue;
        start = locator.startOffset;
        end = locator.endOffset;
      } else {
        if (locator.kind !== "docx") continue;
        start = locator.bodyBlockStart;
        end = locator.bodyBlockEnd;
      }
      if (start < 0 || end <= start) continue;
      const range = rangeFromOffsets(root, start, Math.max(start + 1, end));
      if (range === null) continue;
      const node =
        range.startContainer instanceof Element
          ? range.startContainer
          : range.startContainer.parentElement;
      const block = node?.closest("p,h1,h2,h3,h4,h5,h6,li,blockquote,pre,td,th");
      if (block instanceof HTMLElement) next.push({ note, el: block });
    }
    setAnchors(next);
  }, [format, notes.data, rootRef, versionId]);

  const reposition = useCallback(() => {
    setTick((tick) => tick + 1);
  }, []);

  useEffect(() => {
    reposition();
  }, [notes.data, reposition]);

  useEffect(() => {
    let raf = 0;
    const schedule = () => {
      if (raf !== 0) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        reposition();
      });
    };
    // 文档级捕获:即使挂载时 wrapRef 尚未就绪也不会漏掉滚动。
    document.addEventListener("scroll", schedule, { capture: true, passive: true });
    window.addEventListener("resize", schedule);
    schedule();
    return () => {
      document.removeEventListener("scroll", schedule, { capture: true });
      window.removeEventListener("resize", schedule);
      if (raf !== 0) cancelAnimationFrame(raf);
    };
  }, [reposition]);

  // 位置在渲染时按当前 DOM 读取:块必须仍在视口内才渲染标记。
  const wrap = wrapRef.current;
  const wrapRect = wrap?.getBoundingClientRect() ?? null;
  const markers: Marker[] = [];
  if (wrap !== null && wrapRect !== null) {
    for (const { note, el } of anchors) {
      const rect = el.getBoundingClientRect();
      const top = rect.top - wrapRect.top + wrap.scrollTop;
      // 用内容坐标 + 当前滚动位置判断可见(避免布局瞬间 rect 未更新)。
      const viewportTop = wrap.scrollTop;
      const visible =
        top >= viewportTop - 160 && top <= viewportTop + wrap.clientHeight + 160;
      markers.push({ noteId: note.id, title: noteLabel(note), top, visible });
    }
  }
  const visible = markers.filter((marker) => marker.visible);
  return (
    <div
      className="note-links-layer"
      aria-label="原文页边笔记标记"
      data-note-count={notes.data?.items.length ?? 0}
      data-anchor-count={anchors.length}
      data-marker-count={markers.length}
      data-wrap-top={wrapRect === null ? "null" : String(Math.round(wrapRect.top))}
      data-wrap-bottom={wrapRect === null ? "null" : String(Math.round(wrapRect.bottom))}
      data-marker-info={markers
        .map((marker) => `${Math.round(marker.top)}:${marker.visible ? 1 : 0}`)
        .join(",")}
    >
      {visible.map((marker) => (
        <button
          key={marker.noteId}
          type="button"
          className={`note-link-marker${hover === marker.noteId ? " hover" : ""}`}
          style={{ top: `${marker.top}px` }}
          title={`打开笔记:${marker.title}`}
          aria-label={`打开笔记:${marker.title}`}
          onMouseEnter={() => setHover(marker.noteId)}
          onMouseLeave={() => setHover(null)}
          onClick={() =>
            useReaderTools.getState().requestNoteFocus(marker.noteId)
          }
        >
          <NotebookPen className="w-3 h-3" aria-hidden />
        </button>
      ))}
    </div>
  );
}
