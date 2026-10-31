// 内联翻译(浏览器式):把正文按块翻译,并在原文旁直接显示译文。
//
// 设计要点:
// - 不创建任务、不落产物:它就是"读的时候顺手看一眼译文"。
// - 按块(段落/标题/列表项)分批请求,单批失败只影响该批,不整篇失败。
// - 译文与原文一一对应;失败块显示稳定错误码与"重试",绝不显示假译文。
// - 默认折叠为小字紧贴原文下方,不打断阅读节奏;可切换"仅显示译文"。
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useServices } from "../../app/services";
import { TRANSLATION_IPC_TIMEOUT_MS } from "./translationScopes";

const BLOCK_SELECTOR =
  "p, h1, h2, h3, h4, h5, h6, li, blockquote, td, th, figcaption";
const MAX_BLOCKS = 240;
const BATCH = 12;

export type InlineTranslationState = {
  loading: boolean;
  /** 原文 → 译文(仅成功项)。 */
  done: Map<string, string>;
  /** 原文 → 错误码(失败项,可重试)。 */
  failed: Map<string, string>;
};

export const EMPTY_INLINE_TRANSLATION: InlineTranslationState = {
  loading: false,
  done: new Map(),
  failed: new Map(),
};

/**
 * 读取正文块文本并翻译。`rootRef` 指向正文根节点。
 * 只在 `enabled` 为真且目标语言变化时重新请求。
 */
export function useInlineTranslation(
  rootRef: React.RefObject<HTMLElement | null>,
  documentId: string | null,
  versionId: string | null,
  enabled: boolean,
  target: string,
): InlineTranslationState {
  const { client } = useServices();
  const [state, setState] = useState<InlineTranslationState>(EMPTY_INLINE_TRANSLATION);
  const runIdRef = useRef(0);

  useEffect(() => {
    if (!enabled || documentId === null) {
      setState(EMPTY_INLINE_TRANSLATION);
      return;
    }
    const root = rootRef.current;
    if (root === null) return;
    const blocks = Array.from(root.querySelectorAll<HTMLElement>(BLOCK_SELECTOR))
      .filter((el) => !el.closest(".inline-translation"))
      .map((el) => (el.textContent ?? "").trim())
      .filter((text) => text.length > 1)
      .slice(0, MAX_BLOCKS);
    if (blocks.length === 0) {
      setState(EMPTY_INLINE_TRANSLATION);
      return;
    }
    const unique = Array.from(new Set(blocks));
    const runId = ++runIdRef.current;
    setState({ loading: true, done: new Map(), failed: new Map() });

    void (async () => {
      const done = new Map<string, string>();
      const failed = new Map<string, string>();
      for (let i = 0; i < unique.length; i += BATCH) {
        if (runIdRef.current !== runId) return;
        const chunk = unique.slice(i, i + BATCH);
        try {
          const result = await client.call<{
            targetLanguage: string;
            segments: Array<{ source: string; translated: string | null; errorCode: string | null }>;
          }>("translation_inline", {
            documentId,
            targetLanguage: target,
            segments: chunk,
          }, { timeoutMs: TRANSLATION_IPC_TIMEOUT_MS });
          for (const segment of result.segments) {
            if (segment.translated !== null && segment.translated.trim() !== "") {
              done.set(segment.source, segment.translated);
            } else {
              failed.set(segment.source, segment.errorCode ?? "translation_failed");
            }
          }
        } catch (error) {
          const code =
            typeof error === "object" && error !== null && "appError" in error
              ? String((error as { appError: { code?: string } }).appError.code ?? "translation_failed")
              : "translation_failed";
          for (const source of chunk) failed.set(source, code);
        }
        if (runIdRef.current !== runId) return;
        setState({ loading: true, done: new Map(done), failed: new Map(failed) });
      }
      if (runIdRef.current === runId) {
        setState({ loading: false, done, failed });
      }
    })();

    return () => {
      // 语言切换/关闭时丢弃旧结果,避免把上一语言的译文贴到新语言下。
      runIdRef.current += 1;
    };
    // versionId 变化表示换文档内容,必须重新翻译。
  }, [client, documentId, versionId, enabled, target, rootRef]);

  return state;
}

/**
 * 把译文按块插入正文:每个块后面紧贴一条译文,失败块显示原因。
 * 用 DOM 操作而不是重渲染正文,避免破坏 Markdown 的既有结构。
 */
export function useInlineTranslationLayer(
  rootRef: React.RefObject<HTMLElement | null>,
  state: InlineTranslationState,
  enabled: boolean,
  targetLabel: string,
): void {
  useEffect(() => {
    const root = rootRef.current;
    if (root === null) return;
    // 先清理上一轮插入的节点。
    root.querySelectorAll(".inline-translation").forEach((node) => node.remove());
    if (!enabled) return;
    const blocks = Array.from(root.querySelectorAll<HTMLElement>(BLOCK_SELECTOR)).filter(
      (el) => !el.closest(".inline-translation"),
    );
    for (const el of blocks) {
      const text = (el.textContent ?? "").trim();
      if (text.length <= 1) continue;
      const translated = state.done.get(text);
      const failed = state.failed.get(text);
      if (translated === undefined && failed === undefined) continue;
      const node = document.createElement("span");
      node.className = `inline-translation${failed !== undefined ? " is-failed" : ""}`;
      node.setAttribute("data-translation-target", targetLabel);
      node.textContent =
        translated !== undefined
          ? translated
          : `译文不可用(${failed});可重试内联翻译`;
      el.insertAdjacentElement("afterend", node);
    }
    return () => {
      root.querySelectorAll(".inline-translation").forEach((node) => node.remove());
    };
  }, [rootRef, state, enabled, targetLabel]);
}

/** 内联译文样式(随正文排版走,不单独成卡片)。 */
export function InlineTranslationStyles(): ReactNode {
  return (
    <style>{`
      .inline-translation {
        display: block;
        margin: 0.15em 0 0.7em;
        padding-left: 0.7em;
        border-left: 2px solid color-mix(in srgb, var(--accent) 45%, transparent);
        font-size: 0.9em;
        line-height: 1.65;
        color: color-mix(in srgb, var(--ink) 72%, transparent);
        white-space: pre-wrap;
      }
      .inline-translation.is-failed {
        border-left-color: var(--danger, #dc2626);
        color: var(--danger, #dc2626);
        font-size: 0.82em;
      }
    `}</style>
  );
}
