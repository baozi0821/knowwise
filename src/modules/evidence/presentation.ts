// M05 证据透镜展示逻辑(《M05》§5、《00》§3.2)。
// 结构校验与语义支持分离:有链接但无支持证据 ⇒ 显示"未核验";
// 支持来源(模型/规则/用户)必须可见;笔记与 AI 草稿使用不同视觉标签。
import type { CitationSourceType } from "../../shared/generated/CitationSourceType";
import type { EvidenceLensView } from "../../shared/generated/EvidenceLensView";
import type { EvidenceResolution } from "../../shared/generated/EvidenceResolution";
import type { LensCitationView } from "../../shared/generated/LensCitationView";
import type { LensIssueView } from "../../shared/generated/LensIssueView";
import type { PdfRect } from "../../shared/generated/PdfRect";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import type { SourceTier } from "../../shared/generated/SourceTier";

/** 来源层级徽标:仅 original 允许与原文相同的视觉标签(《M05》§5)。 */
export type TierBadge = "original" | "user_note" | "ai_draft";

export const TIER_LABELS: Record<TierBadge, string> = {
  original: "原文",
  user_note: "用户笔记",
  ai_draft: "AI 草稿",
};

export type CitationVerdict = {
  /** 可作为原文链接回跳:仅原文层级且结构有效。 */
  linkValid: boolean;
  /** 已核验:只有用户确认(supportSource=user)才算已核验。 */
  verified: boolean;
  /** 支持状态的可读标签(含判断来源)。 */
  supportLabel: string;
  /** 来源层级徽标。 */
  tierBadge: TierBadge;
  tierLabel: string;
};

function tierBadgeOf(tier: SourceTier): TierBadge {
  // SourceTier 序列化为 "original" | "user_note" | "ai_draft",与徽标同构。
  return tier as TierBadge;
}

/** 单条证据的展示判定(测试锚点 M05-A4)。 */
export function citationVerdict(c: LensCitationView): CitationVerdict {
  const tierBadge = tierBadgeOf(c.sourceTier);
  const linkValid = tierBadge === "original" && c.structuralStatus === "valid";
  const verified =
    linkValid && c.supportStatus === "supported" && c.supportSource === "user";
  let supportLabel: string;
  if (c.structuralStatus !== "valid") {
    supportLabel = `结构校验未通过(${c.structuralStatus})`;
  } else {
    switch (c.supportStatus) {
      case "supported":
        supportLabel =
          c.supportSource === "user"
            ? "已核验(用户确认)"
            : c.supportSource === "model"
              ? "模型判断支持"
              : c.supportSource === "rule"
                ? "规则判断支持"
                : "已支持";
        break;
      case "insufficient":
        supportLabel = "证据不足";
        break;
      case "conflict":
        supportLabel = "与原文冲突";
        break;
      default:
        supportLabel = "未核验";
    }
  }
  return {
    linkValid,
    verified,
    supportLabel,
    tierBadge,
    tierLabel: TIER_LABELS[tierBadge],
  };
}

/** 透镜三区域视图模型:结论段、证据列表(按原文顺序)、范围与反馈。 */
export type LensSection = {
  /** 结论段标识(claimKey 分组,保序去重)。 */
  claims: string[];
  /** 证据列表:按 order 升序,含判定。 */
  evidenceList: Array<{ citation: LensCitationView; verdict: CitationVerdict }>;
  /** 缺失证据与冲突(第三区域输入)。 */
  issues: LensIssueView[];
  /** 累计反馈数。 */
  feedbackCount: number;
};

export function lensSections(lens: EvidenceLensView): LensSection {
  const claims: string[] = [];
  for (const c of lens.citations) {
    if (!claims.includes(c.claimKey)) claims.push(c.claimKey);
  }
  const evidenceList = [...lens.citations]
    .sort((a, b) => a.order - b.order)
    .map((citation) => ({ citation, verdict: citationVerdict(citation) }));
  return {
    claims,
    evidenceList,
    issues: [...lens.missingEvidence, ...lens.conflicts],
    feedbackCount: lens.feedbackCount,
  };
}

/** 回跳请求:交给 M03 阅读器导航端口;PDF 页未渲染时由阅读器等待 ready。 */
export type NavigateRequest =
  | { kind: "pdf"; pageNumber: number; rects: PdfRect[] }
  | { kind: "markdown"; startOffset: number; endOffset: number }
  | { kind: "docx"; bodyBlockStart: number; bodyBlockEnd: number };

function locatorToRequest(loc: SourceLocator): NavigateRequest | null {
  if (loc.kind === "pdf") {
    return { kind: "pdf", pageNumber: loc.pageNumber, rects: loc.rects };
  }
  if (loc.kind === "markdown") {
    return { kind: "markdown", startOffset: loc.startOffset, endOffset: loc.endOffset };
  }
  if (loc.kind === "docx") {
    return {
      kind: "docx",
      bodyBlockStart: loc.bodyBlockStart,
      bodyBlockEnd: loc.bodyBlockEnd,
    };
  }
  // composite:取首个可转换 part。
  const first = loc.parts.find((p) => p.kind !== "composite");
  return first ? locatorToRequest(first) : null;
}

/**
 * 由解析结果构造回跳请求;navigate 模式才允许跳转,
 * snapshot 模式一律显示快照面板与原因(不打开错误页,测试锚点 M05-A5)。
 */
export function buildNavigateRequest(res: EvidenceResolution): NavigateRequest | null {
  if (res.mode !== "navigate" || res.structuralStatus !== "valid" || !res.locator) {
    return null;
  }
  return locatorToRequest(res.locator);
}

/** 快照面板的可读原因。 */
export function snapshotReasonLabel(res: EvidenceResolution): string {
  switch (res.reason ?? "") {
    case "superseded_version":
      return "文档已有新版本,默认打开原文快照";
    case "non_document_source":
      return "该证据来自笔记或 AI 草稿,不是原文位置";
    case "structural_quote_mismatch":
      return "引文与原文不匹配";
    case "structural_target_missing":
      return "原文位置已不存在";
    case "structural_version_mismatch":
      return "引文版本与文档不一致";
    case "structural_locator_invalid":
      return "原文定位不可用";
    default:
      return "显示原文快照";
  }
}

/** 供 UI 使用:文档事实只信 original 层级证据(《M05》§3)。 */
export function isAuthoritativeFactSource(t: CitationSourceType): boolean {
  return t === "content_block" || t === "anchor";
}
