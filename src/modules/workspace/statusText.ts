// M13 拥有:全局状态文案派生(《M13》§8)。顶部/底部状态使用明确文本;
// 颜色不作为唯一状态表达,所有状态都有文字说明。
// 纯函数,供组件与测试直接使用。

import type { FileState } from "../../shared/generated/FileState";
import type { ParseStatusView } from "../../shared/generated/ParseStatusView";
import type { ReaderStateInput } from "../reader/types";

export type SaveState = "idle" | "saving" | "saved" | "failed";

export function saveStateText(state: SaveState): string {
  switch (state) {
    case "idle":
      return "";
    case "saving":
      return "保存中…";
    case "saved":
      return "已保存";
    case "failed":
      return "保存失败:上一次修改未写入,可重试";
  }
}

/** 文档文件状态摘要(顶栏);缺失/待确认给出可执行指引。 */
export function fileStateText(state: FileState | undefined): string {
  switch (state) {
    case undefined:
      return "";
    case "present":
      return "文件正常";
    case "missing":
      return "原文件已移动或删除,阅读仍可进行;可重新连接文件";
    case "changed_pending_confirm":
      return "文件内容已变化,等待确认新版本";
    case "unreadable":
      return "原文件暂不可读(权限或磁盘)";
  }
}

/**
 * 解析状态 → 阅读横幅 aiPhase(M03 ReaderStateBanner 输入)。
 * 阅读能力与 AI 能力分离:解析失败不影响阅读展示(《M03》§8)。
 */
export function aiPhaseFromParseStatus(
  status: ParseStatusView | undefined,
): ReaderStateInput["aiPhase"] {
  if (!status) return "not_parsed";
  switch (status.capability) {
    case "hybrid_full":
      return status.parseState === "in_progress" ? "parsing" : "ready";
    case "fts_only":
      return "fts_only";
    case "partial_qa":
      return "partial";
    case "selection_qa":
      // 扫描件没有可用文本层:不要用“部分内容可问答”暗示已有文字。
      return status.coverage.hasTextLayer === false ? "no_text_layer" : "partial";
    case "view_only":
      return "not_parsed";
    case "failed":
      // OCR 尝试过但没识别到文字 ≠ 解析崩溃;用专门的语义提示。
      return status.coverage.hasTextLayer === false ? "ocr_no_text" : "failed";
    default:
      return "not_parsed";
  }
}

export function readableFromFileState(state: FileState | undefined): boolean {
  return state === undefined || state === "present" || state === "changed_pending_confirm";
}

export type ModelState = "unknown" | "unavailable" | "ready";

export function modelStateText(state: ModelState): string {
  switch (state) {
    case "unknown":
      return "模型状态:检测中(不阻塞阅读)";
    case "unavailable":
      return "模型不可用:AI 问答暂不可用;阅读、搜索与笔记不受影响";
    case "ready":
      return "模型就绪";
  }
}

export function offlineText(offline: boolean): string {
  return offline ? "离线:本地阅读与笔记可用,AI 请求暂停" : "";
}

export function backgroundTaskSummary(activeCount: number): string {
  if (activeCount <= 0) return "后台任务:无";
  return `后台任务:${activeCount} 个进行中(不阻塞阅读)`;
}

export function indexSummary(
  status: ParseStatusView | undefined,
): { level: "info" | "warning"; text: string } {
  if (!status) return { level: "info", text: "索引:未开始" };
  switch (status.parseState) {
    case "not_started":
      return { level: "info", text: "索引:未开始;选区可按需解析" };
    case "in_progress":
      return { level: "info", text: "索引:建立中(阅读不受影响)" };
    case "complete":
      return { level: "info", text: "索引:已完成" };
    case "partial":
      return { level: "warning", text: "索引:部分完成;未覆盖区域可用选区提问" };
    case "failed":
      return status.coverage.hasTextLayer === false
        ? { level: "warning", text: "索引:未识别到文字(扫描件);可重新识别或按选区提问" }
        : { level: "warning", text: "索引:失败,可重试" };
    default:
      return { level: "info", text: "索引:状态未知" };
  }
}
