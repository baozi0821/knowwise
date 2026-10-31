// 统一 Markdown 渲染器:GFM(表格/任务清单/删除线)+ 代码语法高亮
// (highlight.js)+ 数学公式(KaTeX)+ Mermaid 图表 + kwink 手绘围栏
// (笔记内嵌图形,SVG 渲染)。文档视图与笔记正文共用。
import { useEffect, useState, type ReactNode } from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import rehypeHighlight from "rehype-highlight";
import { decodeInk } from "./annotations";
import { inkStrokeElements } from "./InkSvg";
import "katex/dist/katex.min.css";
import "highlight.js/styles/github-dark.css";

/** 从 <pre><code class="language-x"> 提取语言与源码。 */
function extractCode(children: ReactNode): { language: string | null; code: string } {
  let language: string | null = null;
  let code = "";
  const visit = (node: ReactNode): void => {
    if (node === null || node === undefined) return;
    if (typeof node === "string" || typeof node === "number") {
      code += String(node);
      return;
    }
    if (Array.isArray(node)) {
      node.forEach(visit);
      return;
    }
    if (typeof node === "object" && "props" in (node as unknown as Record<string, unknown>)) {
      const props = (node as unknown as { props?: { className?: string; children?: ReactNode } })
        .props;
      const cls = props?.className ?? "";
      const match = /language-([\w-]+)/.exec(cls);
      if (match !== null) language = match[1]!;
      visit(props?.children ?? null);
    }
  };
  visit(children);
  return { language, code: code.replace(/\n$/, "") };
}

function MermaidBlock({ code }: { code: string }): ReactNode {
  const [svg, setSvg] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const mermaid = (await import("mermaid")).default;
        mermaid.initialize({
          startOnLoad: false,
          securityLevel: "strict",
          theme: "neutral",
          fontFamily: "inherit",
        });
        const id = `mmd-${Math.random().toString(36).slice(2)}`;
        const result = await mermaid.render(id, code);
        if (!cancelled) setSvg(result.svg);
      } catch {
        if (!cancelled) setFailed(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [code]);
  if (failed) {
    return (
      <pre className="md-mermaid-fallback">
        <code>{code}</code>
      </pre>
    );
  }
  if (svg === null) {
    return <div className="md-mermaid-pending" role="status">图表渲染中…</div>;
  }
  return <div className="md-mermaid" dangerouslySetInnerHTML={{ __html: svg }} />;
}

/** kwink 围栏:紧凑笔迹 JSON → SVG(笔记内嵌手绘图形)。 */
function KwinkBlock({ code }: { code: string }): ReactNode {
  const strokes = decodeInk(`ink:v1:${code}`);
  if (strokes === null) {
    return (
      <pre className="md-mermaid-fallback">
        <code>{code}</code>
      </pre>
    );
  }
  return (
    <svg
      className="md-kwink"
      viewBox="0 0 600 220"
      preserveAspectRatio="xMidYMid meet"
      role="img"
      aria-label="手绘图形"
    >
      {inkStrokeElements(strokes, 600, 220)}
    </svg>
  );
}

function PreBlock({ children }: { children?: ReactNode }): ReactNode {
  const { language, code } = extractCode(children);
  if (language === "mermaid") return <MermaidBlock code={code} />;
  if (language === "kwink") return <KwinkBlock code={code} />;
  return <pre>{children}</pre>;
}

/**
 * 图片 URL 过滤:默认策略会拦下 data: 协议,但笔记截图必须以
 * `![截图](data:image/png;base64,…)` 内嵌;聊天图片使用
 * `attachment:<id>` 引用,真正 src 由调用方经 IPC 换取。
 * 其余仍交给 react-markdown 的默认安全策略(禁止 javascript: 等)。
 */
export function markdownUrlTransform(url: string): string {
  const trimmed = url.trim();
  if (/^data:image\//i.test(trimmed)) return trimmed;
  // attachment:<id> 只是引用;真正 src 由调用方通过 IPC 换取,不发起网络请求。
  if (/^attachment:[A-Za-z0-9_-]+$/i.test(trimmed)) return trimmed;
  return defaultUrlTransform(url);
}

export type MarkdownImageResolver = (src: string) => string | null | undefined;

/** 笔记/聊天里的图片统一自适应宽度,不撑破面板与阅读版心。 */
function MarkdownImage({
  src,
  alt,
  resolveImage,
}: {
  src?: string | Blob;
  alt?: string;
  resolveImage?: MarkdownImageResolver;
}): ReactNode {
  const source = typeof src === "string" ? src : "";
  if (source.startsWith("attachment:")) {
    const resolved = resolveImage?.(source);
    if (!resolved) {
      return (
        <span className="md-image-pending" role="status">
          图片加载中…
        </span>
      );
    }
    return <img className="md-image" src={resolved} alt={alt ?? ""} loading="lazy" />;
  }
  return <img className="md-image" src={source} alt={alt ?? ""} loading="lazy" />;
}

export function Markdown({
  children,
  imageResolver,
}: {
  children: string;
  imageResolver?: MarkdownImageResolver;
}): ReactNode {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm, remarkMath]}
      rehypePlugins={[rehypeKatex, [rehypeHighlight, { detect: true, ignoreMissing: true }]]}
      urlTransform={markdownUrlTransform}
      components={{
        pre: PreBlock,
        img: (props) => <MarkdownImage {...props} resolveImage={imageResolver} />,
      }}
    >
      {children}
    </ReactMarkdown>
  );
}
