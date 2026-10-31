// 2C 翻译产物:双语/仅原文/仅译文;低置信度提示;点击译文回原文。
import { useState } from "react";
import type { CommandClient } from "../../app/api/client";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import type { TranslationV1 } from "../../shared/generated/TranslationV1";
import "./translation.css";

const CONFIDENCE_LABELS: Record<string, string> = {
  high: "高置信度",
  medium: "中置信度",
  low: "低置信度",
};

export function TranslationView({
  client,
  content,
}: {
  client: CommandClient;
  content: TranslationV1;
}) {
  const [mode, setMode] = useState<"bilingual" | "original" | "translated">("bilingual");
  const [error, setError] = useState<string | null>(null);

  async function gotoSource(blockId: string) {
    setError(null);
    try {
      const locator = await client.call<SourceLocator | null>("evidence_resolve_block", {
        blockId,
      });
      if (!locator) {
        setError("无法定位该原文块;翻译仍可阅读。");
        return;
      }
      window.dispatchEvent(
        new CustomEvent("knowwise-goto", { detail: { documentId: "", locator } }),
      );
    } catch {
      setError("无法定位该原文块,请重试。");
    }
  }

  return (
    <div className="translation-view" aria-label="翻译对照">
      <div className="translation-head">
        <span>
          {content.sourceLanguage} → {content.targetLanguage} · {content.style}
        </span>
        <div className="translation-mode" role="group" aria-label="显示模式">
          {(
            [
              ["bilingual", "双语"],
              ["original", "仅原文"],
              ["translated", "仅译文"],
            ] as const
          ).map(([value, label]) => (
            <button
              key={value}
              type="button"
              className={`btn tiny${mode === value ? " primary" : ""}`}
              onClick={() => setMode(value)}
            >
              {label}
            </button>
          ))}
        </div>
      </div>
      <p className={`translation-coverage${content.coverage.complete ? "" : " partial"}`}>
        已翻译 {content.coverage.translatedSegments}/{content.coverage.totalSegments} 段
        {content.coverage.complete ? " · 完整" : " · 部分(部分段落失败,可重试)"}
      </p>
      {content.coverage.notes.map((note) => (
        <p key={note} className="translation-note" role="note">
          {note}
        </p>
      ))}
      <ol className="translation-segments">
        {content.segments.map((segment) => (
          <li key={segment.id} className={`translation-segment status-${segment.status}`}>
            <div className="translation-meta">
              {segment.pageNumber !== undefined && segment.pageNumber !== null
                ? `p.${segment.pageNumber} · `
                : ""}
              {CONFIDENCE_LABELS[segment.confidence] ?? segment.confidence}
              {segment.glossaryHits.length > 0
                ? ` · 术语 ${segment.glossaryHits.join("; ")}`
                : ""}
            </div>
            {mode !== "translated" ? (
              <p className="translation-source">{segment.sourceText}</p>
            ) : null}
            {mode !== "original" ? (
              segment.status === "failed" ? (
                <p className="translation-failed" role="alert">
                  本段翻译失败{segment.failureReason ? `(${segment.failureReason})` : ""};原文已保留。
                </p>
              ) : (
                <button
                  type="button"
                  className="translation-target"
                  title="定位原文"
                  onClick={() => void gotoSource(segment.sourceBlockId)}
                >
                  {segment.translatedText}
                </button>
              )
            ) : null}
          </li>
        ))}
      </ol>
      {content.glossary.length > 0 ? (
        <details className="translation-glossary">
          <summary>术语表({content.glossary.length})</summary>
          <ul>
            {content.glossary.map((entry) => (
              <li key={`${entry.source}-${entry.target}`}>
                {entry.source} → {entry.target}
              </li>
            ))}
          </ul>
        </details>
      ) : null}
      {error ? (
        <p className="translation-error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
