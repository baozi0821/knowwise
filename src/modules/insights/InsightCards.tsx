// 2A 动态卡片:正文全部来自文档/模型且每条事实带 sourceRefs。
// 模板只决定布局;没有来源或生成失败时不渲染固定文案。
// UI:浅色卡片 + 整卡命中层(主动作),内部「看原文」/动作保持真实可用。
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useServices } from "../../app/services";
import type { InsightCardAction } from "../../shared/generated/InsightCardAction";
import type { InsightCardV1 } from "../../shared/generated/InsightCardV1";
import "./insights.css";

export type CardActionHandler = (action: InsightCardAction) => void;

function pageRangeLabel(range?: { start: number; end: number }): string | null {
  if (!range) return null;
  return range.start === range.end ? `p.${range.start}` : `p.${range.start}–${range.end}`;
}

export function InsightCards({
  documentId,
  versionId,
  page,
  headingPath,
  kinds,
  onAction,
  compact = false,
}: {
  documentId: string;
  versionId: string;
  page?: number;
  headingPath?: string[];
  kinds: string[];
  onAction: CardActionHandler;
  compact?: boolean;
}) {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const [refreshing, setRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const kindsKey = kinds.join(",");
  // 卡片是文档级建议,不是“当前页”的实时内容。之前把 page / headingPath
  // 放进查询键:正文一滚动就换键,必然 cache miss,于是每一屏都发起一轮
  // insight_cards_get IPC(后端还要挑块、可能调模型)——这就是上下滚动
  // 卡顿的来源。现在查询键只按文档/版本/种类,滚动完全不触发查询。
  const queryKey = ["insight-cards", documentId, versionId, kindsKey];
  const requestRef = useRef({ documentId, versionId, page, headingPath, kinds });
  requestRef.current = { documentId, versionId, page, headingPath, kinds };

  const fullRequest = () => ({
    documentId: requestRef.current.documentId,
    versionId: requestRef.current.versionId,
    page: requestRef.current.page ?? 0,
    locale: "zh-CN",
    kinds: requestRef.current.kinds,
    headingPath:
      requestRef.current.headingPath && requestRef.current.headingPath.length > 0
        ? requestRef.current.headingPath
        : undefined,
  });

  const cards = useQuery({
    queryKey,
    queryFn: () => client.call<InsightCardV1[]>("insight_cards_get", fullRequest()),
    staleTime: 30 * 60_000,
    retry: 0,
  });

  // 停在该章节一段时间后才按新章节静默更新一次;只在空闲时执行,
  // 不参与滚动帧,也不因快速滚动反复请求。
  const sectionKey = `${headingPath?.join("/") ?? ""}|${page ?? 0}`;
  const lastSectionRef = useRef(sectionKey);
  useEffect(() => {
    if (lastSectionRef.current === sectionKey) return;
    lastSectionRef.current = sectionKey;
    if (!cards.isSuccess) return;
    let idleHandle: number | undefined;
    let timer: number | undefined;
    const run = () => {
      void queryClient
        .fetchQuery({
          queryKey,
          queryFn: () => client.call<InsightCardV1[]>("insight_cards_get", fullRequest()),
          staleTime: 30 * 60_000,
        })
        .catch(() => {
          // 保留现有卡片;滚动不该变成错误提示。
        });
    };
    const idle =
      typeof (
        globalThis as { requestIdleCallback?: (cb: () => void, opts?: { timeout: number }) => number }
      ).requestIdleCallback === "function";
    if (idle) {
      idleHandle = (
        globalThis as unknown as {
          requestIdleCallback: (cb: () => void, opts?: { timeout: number }) => number;
        }
      ).requestIdleCallback(run, { timeout: 4000 });
    } else {
      timer = window.setTimeout(run, 1500);
    }
    return () => {
      if (idleHandle !== undefined) {
        (
          globalThis as unknown as { cancelIdleCallback?: (h: number) => void }
        ).cancelIdleCallback?.(idleHandle);
      }
      if (timer !== undefined) window.clearTimeout(timer);
    };
    // 首次挂载不重算;仅章节变化时低频补齐。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sectionKey]);

  async function refresh(): Promise<void> {
    setRefreshing(true);
    setRefreshError(null);
    try {
      await client.call<InsightCardV1[]>("insight_cards_refresh", fullRequest());
      await queryClient.invalidateQueries({ queryKey });
    } catch {
      setRefreshError("卡片刷新失败;已保留当前内容。");
    } finally {
      setRefreshing(false);
    }
  }

  if (cards.isLoading) {
    return (
      <div className="insight-cards skeleton" role="status" aria-live="polite">
        <span>正在根据文档生成卡片…</span>
      </div>
    );
  }
  if (cards.isError) {
    return (
      <div className="insight-cards error" role="alert">
        <span>卡片暂时不可用(文档解析或模型未就绪)。</span>
        <button type="button" className="insight-retry" onClick={() => void cards.refetch()}>
          重试
        </button>
      </div>
    );
  }
  // 没有条目也没有动作的卡片(例如模型未产出问题)不渲染,避免空占位。
  const visibleCards = (cards.data ?? []).filter(
    (card) => card.items.length > 0 || card.actions.length > 0,
  );
  if (visibleCards.length === 0) return null;

  return (
    <div className={`insight-cards${compact ? " compact" : ""}`} aria-label="动态卡片">
      {visibleCards.map((card) => {
        // 没有 locator 的 show_evidence 无法真实回跳,不作为可点动作。
        const usableActions = card.actions.filter(
          (action) => action.tool !== "show_evidence" || Boolean(action.args.locator),
        );
        const primaryAction =
          usableActions.find((action) => action.tool === "ask") ?? usableActions[0];
        const inlineEvidence = card.items.some((item) => item.sourceRefs[0]?.locator);
        const footerActions = inlineEvidence
          ? usableActions.filter((action) => action.tool !== "show_evidence")
          : usableActions;
        const seenSources = new Set<string>();
        const range = pageRangeLabel(card.sourcePageRange);
        return (
          <article
            key={card.id}
            className={`insight-card${primaryAction ? " clickable" : ""}`}
            aria-label={card.title}
          >
            {primaryAction ? (
              <button
                type="button"
                className="insight-card-hit"
                aria-label={`${card.title}:${primaryAction.label}`}
                title={primaryAction.label}
                onClick={() => onAction(primaryAction)}
              />
            ) : null}

            <header className="insight-card-head">
              <h4>{card.title}</h4>
              <span className={`insight-origin origin-${card.generatedBy}`}>
                {card.generatedBy === "ai" ? "AI 生成" : "原文抽取"}
              </span>
              <button
                type="button"
                className="icon-btn small insight-refresh"
                aria-label="刷新卡片"
                title="刷新卡片"
                disabled={refreshing}
                onClick={() => void refresh()}
              >
                <RefreshCw className="w-3 h-3" aria-hidden="true" />
              </button>
            </header>
            {card.subtitle ? <p className="insight-card-subtitle">{card.subtitle}</p> : null}
            <ul className="insight-card-items">
              {card.items.map((item, index) => {
                const source = item.sourceRefs[0];
                const firstForSource =
                  source !== undefined && !seenSources.has(source.blockId);
                if (source) seenSources.add(source.blockId);
                return (
                  <li key={`${card.id}-${index}`} className="insight-card-item">
                    <span className="insight-card-text">{item.text}</span>
                    {firstForSource && source.locator ? (
                      <button
                        type="button"
                        className="insight-evidence"
                        title="定位原文"
                        aria-label={`看原文${source.page !== null && source.page !== undefined ? ` 第 ${source.page} 页` : ""}`}
                        onClick={() =>
                          onAction({
                            id: "show_evidence",
                            label: "看原文",
                            tool: "show_evidence",
                            args: {
                              blockId: source.blockId,
                              locator: JSON.stringify(source.locator),
                            },
                          })
                        }
                      >
                        <ArrowUpRight className="w-3 h-3" aria-hidden="true" />
                        <span>{source.page !== null && source.page !== undefined ? `p.${source.page}` : "原文"}</span>
                      </button>
                    ) : firstForSource ? (
                      <span className="insight-source-hint">
                        {source.page !== null && source.page !== undefined ? `p.${source.page}` : "原文"}
                      </span>
                    ) : null}
                  </li>
                );
              })}
            </ul>
            <footer className="insight-card-foot">
              <span className="insight-source-range">
                {range ? `来源 ${range}` : "来源:文档原文"}
              </span>
              {footerActions.length > 0 ? (
                <span className="insight-card-actions">
                  {footerActions.map((action) => (
                    <button
                      key={action.id}
                      type="button"
                      className="insight-action"
                      onClick={() => onAction(action)}
                    >
                      {action.label}
                    </button>
                  ))}
                </span>
              ) : null}
            </footer>
          </article>
        );
      })}
      {refreshError ? (
        <p className="insight-error" role="alert">
          {refreshError}
        </p>
      ) : null}
    </div>
  );
}
