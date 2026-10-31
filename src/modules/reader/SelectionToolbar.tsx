// 划词浮动工具条:高亮色板(块状/下划线两种样式)+ 提问 + 复制 + 复制为引用。
// 纯展示组件;创建/持久化由宿主视图完成。位置为 fixed 坐标。
import { useEffect, useState, type ReactNode } from "react";
import {
  Sparkles,
  Copy,
  Check,
  CircleHelp,
  MessageSquarePlus,
  Highlighter,
  Underline,
  NotebookPen,
  Quote,
  Languages,
} from "lucide-react";
import { ANNOTATION_PALETTE } from "./annotations";
import type { AnnotationColor } from "../../shared/generated/AnnotationColor";
import type { CommandClient } from "../../app/api/client";

type LocalTranslationStatus = {
  status: "ready" | "model_missing" | "backend_unavailable" | string;
  engineName: string;
  modelDir: string;
};

export type SelectionToolbarState = {
  text: string;
  x: number;
  y: number;
  /** MD/DOCX 选区所在标题路径(复制为引用时取章节名)。 */
  headingPath?: string[];
  /** Rich-text offsets captured while the DOM range is still alive. */
  start?: number;
  end?: number;
};

export function SelectionToolbar({
  state,
  onAnnotate,
  onNote,
  onAsk,
  onCopy,
  onClarify,
  onCopyCitation,
  onAiNote,
  onTranslate,
  copied,
  canAnnotate,
  client,
  targetLanguage,
}: {
  state: SelectionToolbarState;
  onAnnotate: (color: AnnotationColor, style: "highlight" | "underline") => void;
  /** 一键笔记:以选区为出处引用建立笔记并打开笔记面板。 */
  onNote: () => void;
  onAsk: () => void;
  onCopy: () => void;
  /** 把选区记入「待澄清」,稍后写答案或问 AI。 */
  onClarify?: () => void;
  /** 复制为 Markdown 引用块(引文 + 文档名 + 页码/章节)。 */
  onCopyCitation?: () => void;
  /** AI 自动针对选区提炼要点生成笔记。 */
  onAiNote?: () => void;
  /** 翻译选中文字(浏览器式:就地显示译文,可选目标语言)。 */
  onTranslate?: (targetLanguage?: string) => void;
  copied: boolean;
  /** 无 client/版本信息时隐藏标注入口,仅保留提问/复制。 */
  canAnnotate: boolean;
  /** 本地翻译可用性(真实命令;缺失时明确提示会回退远程)。 */
  client?: CommandClient;
  targetLanguage?: string;
}): ReactNode {
  const [localTranslation, setLocalTranslation] = useState<LocalTranslationStatus | null>(null);
  useEffect(() => {
    if (client === undefined) return;
    let cancelled = false;
    void client
      .call<LocalTranslationStatus>("translation_local_status", {
        sourceLanguage: "auto",
        targetLanguage: targetLanguage ?? "zh-CN",
      })
      .then((value) => {
        if (!cancelled) setLocalTranslation(value);
      })
      .catch(() => {
        if (!cancelled) setLocalTranslation(null);
      });
    return () => {
      cancelled = true;
    };
  }, [client, targetLanguage]);
  const localUnavailable =
    localTranslation !== null && localTranslation.status !== "ready";
  return (
    <div
      className="selection-popover"
      style={{ left: `${state.x}px`, top: `${state.y}px` }}
      role="toolbar"
      aria-label="选区操作"
    >
      {canAnnotate ? (
        <>
          <span className="selection-popover-group" aria-label="高亮颜色">
            <Highlighter className="w-3.5 h-3.5 text-amber-300 shrink-0" aria-hidden />
            {ANNOTATION_PALETTE.map((p) => (
              <button
                key={`hl-${p.color}`}
                type="button"
                className="color-dot"
                style={{ backgroundColor: p.hex }}
                title={`高亮·${p.label}`}
                aria-label={`高亮·${p.label}`}
                onClick={() => onAnnotate(p.color, "highlight")}
              />
            ))}
          </span>
          <span className="selection-popover-group" aria-label="下划线颜色">
            <Underline className="w-3.5 h-3.5 text-sky-300 shrink-0" aria-hidden />
            {ANNOTATION_PALETTE.map((p) => (
              <button
                key={`ul-${p.color}`}
                type="button"
                className="color-dot underline-dot"
                style={{ borderBottomColor: p.hex }}
                title={`下划线·${p.label}(标记重点,不是删除线)`}
                aria-label={`下划线·${p.label}`}
                onClick={() => onAnnotate(p.color, "underline")}
              />
            ))}
          </span>
          <span className="selection-popover-divider" aria-hidden />
        </>
      ) : null}
      {canAnnotate ? (
        <button type="button" className="popover-btn note-btn" onClick={onNote} title="笔记:引用这段原文写笔记">
          <NotebookPen className="w-3.5 h-3.5 text-amber-300" aria-hidden />
          <span>笔记</span>
        </button>
      ) : null}
      {canAnnotate && onAiNote ? (
        <button
          type="button"
          className="popover-btn ai-note-btn text-purple-600 dark:text-purple-300 font-medium"
          onClick={onAiNote}
          title="AI 自动提炼核心要点并生成便签笔记"
        >
          <Sparkles className="w-3.5 h-3.5 text-purple-400" aria-hidden />
          <span>AI 记笔记</span>
        </button>
      ) : null}
      {onClarify ? (
        <button
          type="button"
          className="popover-btn"
          onClick={onClarify}
          title="待澄清:先记下这句没读懂的地方,稍后写答案或问 AI"
        >
          <CircleHelp className="w-3.5 h-3.5 text-teal-300" aria-hidden />
          <span>待澄清</span>
        </button>
      ) : null}
      {onTranslate ? (
        <span className="selection-popover-group" aria-label="翻译选区">
          <Languages className="w-3.5 h-3.5 text-sky-300 shrink-0" aria-hidden />
          <button
            type="button"
            className="popover-btn"
            onClick={() => onTranslate()}
            title={
              localUnavailable
                ? "本地翻译模型缺失;将改用远程模型(需已完成授权)"
                : "把选中文字翻译成当前目标语言,并在原文下方就地显示译文"
            }
          >
            <span>翻译</span>
          </button>
          {localUnavailable ? (
            <span
              className="selection-local-translation"
              role="status"
              title={`本地引擎 ${localTranslation?.engineName ?? ""} 不可用(${localTranslation?.status});翻译会改用远程模型`}
            >
              本地翻译不可用
            </span>
          ) : null}
          <select
            className="popover-select"
            aria-label="翻译目标语言"
            defaultValue=""
            onChange={(event) => {
              const value = event.target.value;
              if (value === "") return;
              onTranslate(value);
              event.target.value = "";
            }}
          >
            <option value="">其他语言…</option>
            <option value="zh-CN">中文</option>
            <option value="en-US">English</option>
            <option value="ja-JP">日本語</option>
            <option value="ko-KR">한국어</option>
            <option value="de-DE">Deutsch</option>
            <option value="fr-FR">Français</option>
            <option value="es-ES">Español</option>
          </select>
        </span>
      ) : null}
      <button type="button" className="popover-btn" onClick={onAsk} title="就选区向 AI 提问">
        <MessageSquarePlus className="w-3.5 h-3.5 text-indigo-300" aria-hidden />
        <span>提问</span>
      </button>
      {onCopyCitation ? (
        <button
          type="button"
          className="popover-btn"
          onClick={onCopyCitation}
          title="复制为 Markdown 引用块(引文 + 文档名 + 页码/章节),可直接粘贴到笔记"
          aria-label="复制为引用"
        >
          <Quote className="w-3.5 h-3.5 text-amber-200" aria-hidden />
          <span>复制引用</span>
        </button>
      ) : null}
      <button type="button" className="popover-btn" onClick={onCopy} title="复制选中文字">
        {copied ? (
          <Check className="w-3.5 h-3.5 text-emerald-400" aria-hidden />
        ) : (
          <Copy className="w-3.5 h-3.5 text-slate-300" aria-hidden />
        )}
        <span>{copied ? "已复制" : "复制"}</span>
      </button>
    </div>
  );
}
