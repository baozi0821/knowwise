// M08 模型设置与隐私辅助逻辑(《M08》§3–§9 的前端纯逻辑)。
// 不直接调用 Tauri 命令;视图层经 react-query 调用,这里只做:
// 能力状态解读、授权矩阵、密钥输入策略、清计划勾选与确认门。
import type { ClearCategory } from "../../shared/generated/ClearCategory";
import type { ClearPreview } from "../../shared/generated/ClearPreview";
import type { GrantCheckView } from "../../shared/generated/GrantCheckView";
import type { ModelCapability } from "../../shared/generated/ModelCapability";
import type { ModelDegradationReason } from "../../shared/generated/ModelDegradationReason";
import type { ModelPurpose } from "../../shared/generated/ModelPurpose";
import type { ModelStatusView } from "../../shared/generated/ModelStatusView";
import type { ProcessingLocation } from "../../shared/generated/ProcessingLocation";

// —— 能力状态(§3)——

export const CAPABILITY_LABELS: Record<ModelCapability, string> = {
  ready: "可用",
  generate_only: "仅生成",
  embed_only: "仅嵌入",
  unavailable: "不可用",
  misconfigured: "配置不完整",
};

export const DEGRADATION_LABELS: Record<ModelDegradationReason, string> = {
  no_provider: "未配置任何提供方",
  provider_unreachable: "提供方不可达",
  secret_missing: "远程提供方缺少密钥",
  base_url_missing: "远程提供方缺少地址",
  generate_reference_missing: "未设置生成模型",
  embed_reference_missing: "未设置嵌入模型",
  reference_model_missing: "参考模型不在可用列表",
  reference_provider_missing: "参考提供方不存在或未启用",
};

/** AI 问答是否可用(§3:无生成模型时禁用并说明原因,不阻塞启动)。 */
export function canGenerate(status: ModelStatusView): boolean {
  return (
    status.capability === "ready" ||
    status.capability === "generate_only" ||
    status.capability === "misconfigured"
  );
}

/** 是否进入 FTS-only(无嵌入模型时不声称完整语义检索)。 */
export function isFtsOnly(status: ModelStatusView): boolean {
  return (
    status.capability === "embed_only" || status.capability === "unavailable"
  );
}

export function capabilitySummary(status: ModelStatusView): string {
  const label = CAPABILITY_LABELS[status.capability];
  if (status.reasons.length === 0) return label;
  return `${label}(${status.reasons.map((r) => DEGRADATION_LABELS[r]).join("、")})`;
}

// —— 隐私授权矩阵(§5)——

export const PURPOSE_LABELS: Record<ModelPurpose, string> = {
  generate: "生成",
  embed: "嵌入",
  vision: "图片",
};

export const LOCATION_LABELS: Record<ProcessingLocation, string> = {
  local: "本机",
  remote: "远程",
};

export type GrantMatrixRow = {
  location: ProcessingLocation;
  providerId: string;
  purpose: ModelPurpose;
  granted: boolean;
  capabilityReady: boolean;
  executable: boolean;
  /** 无依据承诺禁用:链路不可执行时的简短原因。 */
  hint: string | null;
};

export function grantMatrix(checks: GrantCheckView[]): GrantMatrixRow[] {
  return checks.map((c) => ({
    location: c.processingLocation,
    providerId: c.providerId,
    purpose: c.purpose,
    granted: c.granted,
    capabilityReady: c.capabilityReady,
    executable: c.executable,
    hint: !c.granted
      ? "尚未授权"
      : !c.capabilityReady
        ? "提供方当前不可用"
        : null,
  }));
}

/** 首次对文档使用 AI 前必须确认的链路(生成或嵌入、本地或远程)。 */
export function requiredGrantFor(
  location: ProcessingLocation,
  purpose: ModelPurpose,
): { location: ProcessingLocation; purpose: ModelPurpose } {
  return { location, purpose };
}

// —— 密钥输入策略(§6)——

/** 密钥写入后前端只保留"已配置";内存中不留副本。 */
export type SecretDraftState = {
  value: string;
  touched: boolean;
};

export function secretDraftValid(draft: SecretDraftState): boolean {
  // 非空且无首尾空白;长度上限防御(真实校验在 Rust 端)。
  const v = draft.value;
  return (
    draft.touched &&
    v.trim().length > 0 &&
    v.length <= 512 &&
    v === v.trim()
  );
}

/** 密钥显示只以掩码呈现;任何界面文本不回显完整密钥。 */
export function maskSecretConfigured(configured: boolean, updatedAt?: number): string {
  if (!configured) return "未配置";
  if (updatedAt === undefined) return "已配置";
  return `已配置(更新于 ${new Date(updatedAt).toLocaleString()})`;
}

// —— 清计划勾选与确认门(§8)——

export const CLEAR_CATEGORY_LABELS: Record<ClearCategory, string> = {
  fts_vector_cache: "FTS/向量/缓存",
  parse_cache: "解析缓存",
  conversations: "会话与消息",
  artifacts: "产物与版本",
  agent_reasoning: "思考记录(可选保存)",
  insight_cards: "动态卡片/画像缓存",
  translation_memory: "翻译记忆",
  annotations: "标注",
  bookmarks: "书签",
  notes: "笔记",
  clarifications: "待澄清",
  tags: "标签",
};

/** 全部类别,固定展示顺序(可重建 → 派生 → 用户资产)。 */
export const ALL_CLEAR_CATEGORIES: ClearCategory[] = [
  "fts_vector_cache",
  "parse_cache",
  "conversations",
  "artifacts",
  "agent_reasoning",
  "insight_cards",
  "translation_memory",
  "annotations",
  "bookmarks",
  "notes",
  "clarifications",
  "tags",
];

/** 用户资产类:默认不选,必须明确勾选(《M08》§8)。 */
export const USER_ASSET_CATEGORIES: ClearCategory[] = [
  "annotations",
  "bookmarks",
  "notes",
  "clarifications",
  "tags",
];

/** 初始勾选:仅可重建数据;用户资产一律不默认选中。 */
export function defaultClearSelection(): ClearCategory[] {
  return ["fts_vector_cache"];
}

export function isUserAsset(category: ClearCategory): boolean {
  return USER_ASSET_CATEGORIES.includes(category);
}

/** 执行确认:含用户资产或受保护块时必须逐字确认文档 ID。 */
export function clearConfirmationRequired(preview: ClearPreview): boolean {
  return preview.requiresExplicitConfirmation || preview.protectedBlocks.length > 0;
}

export function confirmationMatches(preview: ClearPreview, input: string): boolean {
  return input.trim() === preview.documentId;
}

/** 计划是否已过期(执行前 UI 提示重新预览;服务端仍终裁)。 */
export function planExpired(preview: ClearPreview, now: number): boolean {
  return now >= preview.expiresAt;
}

/** 预览摘要:不可用类别如实展示,不静默跳过。 */
export function previewLine(preview: ClearPreview, category: ClearCategory): string {
  const item = preview.categories.find((c) => c.category === category);
  if (!item) return "未包含在本次计划";
  if (!item.available) return "暂不可用(负责模块未接入)";
  const count = item.count ?? 0;
  const protectedCount = item.protectedCount ?? 0;
  if (protectedCount > 0) {
    return `将清除 ${count} 项,受保护 ${protectedCount} 项不会被清除`;
  }
  return `将清除 ${count} 项`;
}

// —— 供应商披露文案(§5/SEC-06:只展示已核实信息,禁止"完全本地/绝对安全")——

export const VENDOR_RETENTION_DISCLOSURE =
  "数据送达提供方后,其留存、训练与删除策略以提供方条款为准;Knowwise 不代为删除已送达的数据。";

export const LOCAL_PROCESSING_DISCLOSURE =
  "本机处理不经过网络,但不等于绝对安全;本机模型仍会处理文档内容。";
