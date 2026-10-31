// M03 §8 阅读状态与 AI 状态分离的展示组件。
// 阅读能力与 AI 能力分别表述;失败不伪装加载中;提供文本说明(非仅颜色)。
import type { ReaderStateInput } from "./types";

export function readerStateText(input: ReaderStateInput): {
  level: "info" | "warning" | "error";
  text: string;
} {
  if (!input.readable) {
    return {
      level: "error",
      // 文件缺失/损坏:保留已有数据;阅读错误与本地笔记分开表述。
      text: input.fileError
        ? `无法阅读:${input.fileError};已保存的笔记与标注不受影响。`
        : "无法阅读:文件缺失或已损坏,已保存的笔记与标注不受影响。",
    };
  }
  switch (input.aiPhase) {
    case "not_parsed":
      return { level: "info", text: "全文 AI 尚未就绪;选区可按需解析。" };
    case "parsing":
      return { level: "info", text: "正在建立全文索引(不阻塞阅读与翻页)。" };
    case "partial":
      // 部分索引不再占用正文上方横幅;覆盖范围由阅读区底部的页覆盖/OCR 面板如实呈现。
      return { level: "info", text: "" };
    case "ready":
      return { level: "info", text: "可问答:全文语义索引已就绪。" };
    case "fts_only":
      return {
        level: "info",
        // 能力来源与降级可见(《M03》§8)。
        text: "可问答:仅关键词检索(FTS),未配置语义嵌入模型。",
      };
    case "no_text_layer":
      return {
        level: "warning",
        text: "本文档无文本层(扫描件):可用阅读器工具栏「识别指定页」按需 OCR,或用选区提问。",
      };
    case "failed":
      return {
        level: "error",
        text: "全文解析失败:阅读不受影响,可在工具栏「识别指定页」按需识别后重试建立索引。",
      };
    case "ocr_no_text":
      return {
        level: "warning",
        text: "扫描件未识别到可用文字(OCR 未返回文本);可重新识别指定页,或按选区提问。",
      };
    default:
      return { level: "info", text: "" };
  }
}

export function ReaderStateBanner({ input }: { input: ReaderStateInput }) {
  const { level, text } = readerStateText(input);
  if (text === "") return null;
  return (
    <div
      role="status"
      aria-live="polite"
      data-level={level}
      style={{
        padding: "6px 12px",
        fontSize: 13,
        borderRadius: 6,
        border: "1px solid",
        borderColor:
          level === "error" ? "#d92d20" : level === "warning" ? "#b54708" : "#364152",
        background:
          level === "error"
            ? "#fef3f2"
            : level === "warning"
              ? "#fffaeb"
              : "#f2f4f7",
      }}
    >
      {text}
    </div>
  );
}
