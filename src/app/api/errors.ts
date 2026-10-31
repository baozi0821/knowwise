// M13 拥有:AppError → 用户可读文案与恢复动作映射(《02》§7、《M13》§8)。
// 前端视觉与恢复逻辑按稳定错误码分类,不解析自由文本;失败提示必须
// 包含原因与可执行动作,不允许只写“出错了”。

import type { AppError } from "../../shared/generated/AppError";
import type { UserAction } from "../../shared/generated/UserAction";

export const USER_ACTION_LABELS: Record<UserAction, string> = {
  retry: "重试",
  choose_scope: "缩小提问范围",
  reconnect_file: "重新连接文件",
  open_settings: "打开设置",
  grant_permission: "前往授权",
  wait_for_index: "等待索引完成",
  choose_other_file: "选择其他文件",
  none: "",
};

/** 错误码 → 标题文案(基线来自《02》§7;只增不改语义)。 */
export const ERROR_CODE_TITLES: Record<string, string> = {
  INTERNAL: "操作未完成",
  INVALID_REQUEST: "请求无效",
  CONFLICT_REVISION_MISMATCH: "内容已被其他修改更新",
  NOT_FOUND: "目标不存在",
  TASK_CANCELED: "任务已取消",
  TASK_INTERRUPTED: "任务被中断",
  TASK_NOT_RUNNING: "任务已不在运行",
  DB_CORRUPT: "本地数据库损坏",
  DB_MIGRATION_FAILED: "数据库升级失败",
  DOC_NOT_FOUND: "文档不存在",
  DOC_FILE_MISSING: "原文件已移动或删除",
  DOC_UNSUPPORTED_FORMAT: "暂不支持该文件格式",
  DOC_EMPTY: "文件为空",
  DOC_CORRUPT: "文件已损坏",
  DOC_ENCRYPTED: "文件已加密,无法读取",
  DOC_TOO_LARGE: "文件超出导入大小上限",
  DOC_VERSION_UNCONFIRMED: "文件内容变化待确认",
  FILE_ACCESS_EXPIRED: "文件访问已过期",
  READER_FORMAT_UNSUPPORTED_REGION: "该区域包含不支持的格式对象",
  PARSE_NO_TEXT_LAYER: "文档没有文本层(扫描件)",
  PARSE_PARTIAL: "仅部分内容完成解析",
  PARSE_FAILED: "解析失败",
  INDEX_FTS_ONLY: "仅关键词检索可用(未配置语义嵌入)",
  INDEX_GENERATION_UNAVAILABLE: "索引代次不可用",
  CONTEXT_COVERAGE_INCOMPLETE: "所选范围尚未完成索引",
  CONTEXT_BUDGET_EXCEEDED: "上下文超出预算",
  CONTEXT_SNAPSHOT_IMMUTABLE: "上下文快照已冻结,不可修改",
  PRIVACY_GRANT_REQUIRED: "需要先完成隐私授权",
  PRIVACY_EGRESS_DENIED: "外发请求被隐私策略拦截",
  PRIVACY_PLAN_EXPIRED: "清除计划已过期",
  SECRET_STORE_UNAVAILABLE: "系统凭据存储不可用",
  MODEL_UNAVAILABLE: "模型不可用",
  MODEL_TIMEOUT: "模型响应超时",
  MODEL_OUTPUT_INVALID: "模型输出未通过校验",
  MODEL_QUOTA_EXHAUSTED: "模型额度已用尽",
  CITATION_TARGET_MISSING: "引用目标不存在",
  CITATION_QUOTE_MISMATCH: "引文与原文不一致",
  ANCHOR_ORPHANED: "锚点已失去目标",
  ANCHOR_RELOCATION_NOT_CONFIRMED: "重定位尚未确认",
  ARTIFACT_VERSION_IMMUTABLE: "该产物版本不可修改",
  ARTIFACT_SCHEMA_INVALID: "产物内容未通过 Schema 校验",
  QUIZ_QUESTION_INVALID: "题目无效",
  EXPORT_TARGET_EXISTS: "导出目标文件已存在",
  EXPORT_PERMISSION_DENIED: "没有导出权限",
  EXPORT_DISK_FULL: "磁盘空间不足",
  // M13 壳层码(启动配置不可解析)与前端本地码(IPC 层)。
  SETTINGS_INVALID: "设置文件不可解析",
  IPC_TIMEOUT: "命令执行超时",
  IPC_OFFLINE: "当前离线",
  IPC_TRANSPORT: "与本地服务的通信失败",
  MODULE_INIT_FAILED: "模块初始化失败",
};

export type ErrorPresentation = {
  /** 稳定标题(按错误码)。 */
  title: string;
  /** 恢复动作标签;userAction 为 none 时为 null,界面不显示按钮。 */
  actionLabel: string | null;
  /** 是否可重试(驱动按钮语义)。 */
  retryable: boolean;
  /** 非敏感结构化细节的可读摘要(无则空数组)。 */
  detailLines: string[];
};

const DETAIL_KEY_LABELS: Record<string, string> = {
  reason: "原因",
  schemaVersion: "配置版本",
  version: "版本",
  count: "数量",
  status: "状态",
  attemptNo: "尝试次数",
};

export function describeAppError(error: AppError): ErrorPresentation {
  const title = ERROR_CODE_TITLES[error.code] ?? `操作失败(${error.code})`;
  const hasAction = error.userAction !== "none";
  const detailLines: string[] = [];
  if (error.details) {
    for (const [key, value] of Object.entries(error.details)) {
      if (value == null) continue;
      const label = DETAIL_KEY_LABELS[key] ?? key;
      detailLines.push(`${label}:${String(value)}`);
    }
  }
  return {
    title,
    actionLabel: hasAction ? USER_ACTION_LABELS[error.userAction] : null,
    retryable: error.retryable || error.userAction === "retry",
    detailLines,
  };
}

/** 合成前端本地错误信封(IPC 超时/离线/传输失败/模块初始化失败)。 */
export function synthAppError(
  code: "IPC_TIMEOUT" | "IPC_OFFLINE" | "IPC_TRANSPORT" | "MODULE_INIT_FAILED",
  detail?: string,
): AppError {
  const userAction =
    code === "IPC_OFFLINE" ? "none" : code === "MODULE_INIT_FAILED" ? "none" : "retry";
  const details: Record<string, string> = {};
  if (detail) details.reason = detail;
  return {
    schemaVersion: "error.v1",
    code,
    messageKey: `error.${code.toLowerCase()}`,
    retryable: code === "IPC_TIMEOUT" || code === "IPC_TRANSPORT",
    userAction,
    details,
    traceId: `fe-${Math.random().toString(36).slice(2, 10)}`,
  };
}
