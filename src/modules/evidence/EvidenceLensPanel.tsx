// M05 证据透镜:回答旁的引用原文与校验状态(P1)。
// 只展示已落库的 citation;结构校验与语义支持分开显示,不把“有链接”当“已证实”。
import { useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronDown, ChevronUp, ShieldCheck } from "lucide-react";
import type { EvidenceLensView } from "../../shared/generated/EvidenceLensView";
import { useServices } from "../../app/services";
import { lensSections } from "./presentation";

export function EvidenceLensPanel({
  messageId,
  onCitationClick,
}: {
  messageId: string;
  onCitationClick: (citationId: string) => void;
}): ReactNode {
  const { client } = useServices();
  const [open, setOpen] = useState(false);
  const lens = useQuery({
    queryKey: ["evidence-lens", messageId],
    queryFn: () =>
      client.call<EvidenceLensView>("evidence_get_lens", {
        ownerType: "message",
        ownerId: `message:${messageId}`,
      }),
    enabled: open,
    retry: false,
  });

  const sections = lens.data ? lensSections(lens.data) : null;

  return (
    <div className="evidence-lens">
      <button
        type="button"
        className="qa-retry-btn"
        aria-expanded={open}
        aria-label={open ? "收起证据透镜" : "打开证据透镜"}
        onClick={() => setOpen((value) => !value)}
      >
        <ShieldCheck className="w-3.5 h-3.5" aria-hidden="true" />
        <span>证据</span>
        {open ? (
          <ChevronUp className="w-3 h-3" aria-hidden="true" />
        ) : (
          <ChevronDown className="w-3 h-3" aria-hidden="true" />
        )}
      </button>
      {open ? (
        <section className="evidence-lens-panel" aria-label="证据透镜">
          {lens.isLoading ? <p role="status">正在读取引用…</p> : null}
          {lens.isError ? (
            <div role="alert">
              <p>证据透镜加载失败;引用本身仍可点击定位。</p>
              <button
                type="button"
                className="btn tiny ghost"
                onClick={() => void lens.refetch()}
              >
                重试
              </button>
            </div>
          ) : null}
          {sections ? (
            <>
              {sections.evidenceList.length === 0 ? (
                <p className="evidence-lens-empty">本轮回答没有可展示的引用。</p>
              ) : (
                <ul className="evidence-lens-list">
                  {sections.evidenceList.map(({ citation, verdict }) => {
                    const place = [
                      citation.pageNumber !== undefined
                        ? `第 ${citation.pageNumber} 页`
                        : null,
                      citation.headingPath?.length
                        ? citation.headingPath.join(" › ")
                        : null,
                    ]
                      .filter(Boolean)
                      .join(" · ");
                    return (
                      <li key={citation.citationId} className="evidence-lens-item">
                        <div className="evidence-lens-meta">
                          <span className={`evidence-tier tier-${verdict.tierBadge}`}>
                            {verdict.tierLabel}
                          </span>
                          <span
                            className={`evidence-support support-${citation.supportStatus}`}
                          >
                            {verdict.supportLabel}
                          </span>
                          {place ? <span className="evidence-place">{place}</span> : null}
                        </div>
                        <blockquote className="evidence-quote">
                          「{citation.quotedText}」
                        </blockquote>
                        {verdict.linkValid ? (
                          <button
                            type="button"
                            className="btn tiny ghost"
                            onClick={() => onCitationClick(citation.citationId)}
                          >
                            定位原文
                          </button>
                        ) : (
                          <p className="evidence-lens-note">结构校验未通过,不能回跳原文。</p>
                        )}
                      </li>
                    );
                  })}
                </ul>
              )}
              {sections.issues.length > 0 ? (
                <ul className="evidence-lens-issues" aria-label="缺失或冲突证据">
                  {sections.issues.map((issue) => (
                    <li key={`${issue.citationId}:${issue.issueKind}`}>
                      <strong>{issue.claimKey}</strong>: {issue.description}
                    </li>
                  ))}
                </ul>
              ) : null}
              {sections.feedbackCount > 0 ? (
                <p className="evidence-lens-note">
                  已收到 {sections.feedbackCount} 条引用反馈。
                </p>
              ) : null}
            </>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}
