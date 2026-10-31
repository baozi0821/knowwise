// 页地图 / 覆盖条:一眼看出「AI 到底读了哪些页」。
//
// 只呈现真实页状态(解析覆盖 + OCR 状态 + 当前范围),不估算、不美化:
// - 已覆盖 = 解析完成且无缺失;部分 = 有缺失或低置信;
// - OCR 页单独标记,低置信页额外提示「请核对」;
// - 当前提问范围高亮,便于确认「这次问的是哪几页」;
// - 用形状/文字 + 颜色双重表达状态(不依赖颜色单独传达信息)。
import { useId, useState, type ReactNode } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import type { CoverageSummary } from "../../shared/generated/CoverageSummary";
import type { CoverageView } from "../../shared/generated/CoverageView";
import type { PageOcrStatusView } from "../../shared/generated/PageOcrStatusView";

type PageState = "covered" | "partial" | "missing" | "ocr" | "ocr_low";

export type PageMapEntry = {
  page: number;
  state: PageState;
  label: string;
};

const STATE_LABELS: Record<PageState, string> = {
  covered: "已覆盖",
  partial: "部分覆盖",
  missing: "未覆盖",
  ocr: "OCR 识别",
  ocr_low: "OCR 低置信",
};

/** 合并覆盖真相与 OCR 状态为逐页状态;OCR 优先于普通覆盖。 */
export function buildPageMap(
  coverage: CoverageSummary | undefined,
  ocr: readonly PageOcrStatusView[] | undefined,
): PageMapEntry[] {
  const pages = new Map<number, PageState>();
  const mark = (regions: string[] | undefined, state: PageState) => {
    for (const region of regions ?? []) {
      const match = /^page:(\d+)$/.exec(region);
      if (match) pages.set(Number(match[1]), state);
    }
  };
  mark(coverage?.completedRegions, "covered");
  mark(coverage?.partialRegions, "partial");
  mark(coverage?.missingRegions, "missing");

  for (const row of ocr ?? []) {
    const low = row.status === "low_confidence";
    const done = row.status === "completed" || low;
    if (!done) continue;
    // “识别完成但 0 块”没有可检索文本,不能算作已覆盖。
    if (row.blockCount === 0) {
      pages.set(row.pageNumber, "missing");
      continue;
    }
    pages.set(row.pageNumber, low ? "ocr_low" : "ocr");
  }

  return Array.from(pages.entries())
    .sort((a, b) => a[0] - b[0])
    .map(([page, state]) => ({ page, state, label: STATE_LABELS[state] }));
}

/**
 * 文档级覆盖(parse_get_status.coverage)转成逐页状态。
 * 没有快照覆盖时用它,这样“纯文本 PDF”也能看到页覆盖;
 * 解析失败页用 failedRegions 的 regionKey 标记为缺失。
 */
export function coverageFromParse(
  coverage: CoverageView | undefined,
): CoverageSummary | undefined {
  if (!coverage) return undefined;
  const failed = (coverage.failedRegions ?? []).map((region) => region.regionKey);
  const missing = failed.filter((key) => /^page:\d+$/.test(key));
  return {
    schemaVersion: "context_coverage.v1",
    parseState: "complete",
    scopeCovered: missing.length === 0,
    completedRegions: coverage.completedRegions ?? [],
    partialRegions: coverage.partialRegions ?? [],
    missingRegions: missing,
    notes: [],
  };
}

/** 覆盖条:范围内已读页 / 总页 + 完整或部分(与回答头一致的口径)。 */
export function PageCoverageBar({
  coverage,
  parseCoverage,
  ocr,
  activeRange,
}: {
  coverage?: CoverageSummary;
  /** 文档级解析覆盖(无快照时使用)。 */
  parseCoverage?: CoverageView;
  ocr?: readonly PageOcrStatusView[];
  activeRange?: { start: number; end: number } | null;
}): ReactNode {
  const [open, setOpen] = useState(false);
  const bodyId = useId();
  const entries = buildPageMap(coverage ?? coverageFromParse(parseCoverage), ocr);
  if (entries.length === 0) return null;
  const covered = entries.filter(
    (entry) => entry.state === "covered" || entry.state === "ocr",
  ).length;
  const effective = coverage ?? coverageFromParse(parseCoverage);
  const completeness = effective?.scopeCovered === false ? "部分" : "完整";
  const rangeLabel =
    activeRange !== undefined && activeRange !== null
      ? activeRange.start === activeRange.end
        ? `p.${activeRange.start}`
        : `p.${activeRange.start}–${activeRange.end}`
      : null;

  return (
    <section className={`page-map${open ? " is-open" : ""}`} aria-label="页覆盖地图">
      <button
        type="button"
        className="page-map-head"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => setOpen((value) => !value)}
        title={open ? "收起页覆盖地图" : "展开页覆盖地图"}
      >
        {open ? (
          <ChevronDown className="w-3.5 h-3.5" aria-hidden="true" />
        ) : (
          <ChevronRight className="w-3.5 h-3.5" aria-hidden="true" />
        )}
        <span className="page-map-title">页覆盖</span>
        <span className="page-map-summary">
          已读 {covered}/{entries.length} 页 · {completeness}
          {rangeLabel ? ` · 当前 ${rangeLabel}` : ""}
        </span>
        <span className="page-map-toggle">{open ? "收起" : "展开"}</span>
      </button>
      {/* 折叠时也保留一条极简色带:用户一眼就能看出哪些页已读/缺失,
          不必先展开。展开后显示页码与图例。 */}
      {!open ? (
        <ol className="page-map-mini" aria-hidden="true">
          {entries.map((entry) => {
            const inRange =
              activeRange !== undefined &&
              activeRange !== null &&
              entry.page >= activeRange.start &&
              entry.page <= activeRange.end;
            return (
              <li
                key={entry.page}
                className={`page-map-mini-cell state-${entry.state}${inRange ? " in-range" : ""}`}
                title={`第 ${entry.page} 页 · ${entry.label}`}
              />
            );
          })}
        </ol>
      ) : null}
      {open ? (
        <div id={bodyId} className="page-map-body">
          <ol className="page-map-strip">
            {entries.map((entry) => {
              const inRange =
                activeRange !== undefined &&
                activeRange !== null &&
                entry.page >= activeRange.start &&
                entry.page <= activeRange.end;
              return (
                <li
                  key={entry.page}
                  className={`page-map-cell state-${entry.state}${inRange ? " in-range" : ""}`}
                  title={`第 ${entry.page} 页 · ${entry.label}${inRange ? "(当前范围)" : ""}`}
                >
                  <span className="page-map-page">{entry.page}</span>
                  <span className="visually-hidden">
                    {entry.label}
                    {inRange ? ",当前提问范围" : ""}
                  </span>
                </li>
              );
            })}
          </ol>
          <ul className="page-map-legend" aria-label="图例">
            {(
              [
                ["state-covered", "已覆盖", "●"],
                ["state-partial", "部分", "◐"],
                ["state-missing", "未覆盖", "○"],
                ["state-ocr", "OCR", "◆"],
                ["state-ocr_low", "低置信", "◇"],
                ["in-range", "当前范围", "▣"],
              ] as const
            ).map(([state, label, glyph]) => (
              <li key={state} className={`legend-item ${state}`}>
                <span className="legend-glyph" aria-hidden="true">
                  {glyph}
                </span>
                <span>{label}</span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </section>
  );
}
