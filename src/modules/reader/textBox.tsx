// 原文文字注释框:在正文任意位置插入一个可编辑文本框。
//
// 复用标注(annotation)存储:锚点是"点位置"的块/页坐标,noteText 用
// textbox:v1: 前缀保存文本与样式。好处:自动获得版本绑定、删除、隐私清除、
// 导出与回跳,不需要新表、新迁移。
//
// 交互(刻意做得像批注而不是弹窗):
// - 选「文字注释」工具后点正文位置 → 出现输入框,直接打字;
// - Ctrl/⌘ + Enter 保存,Esc 取消;
// - 已有文本框点击即可改内容与样式(颜色/加粗/字号)。
import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { AnnotationView } from "../../shared/generated/AnnotationView";
import type { RawSelectionInput } from "../../shared/generated/RawSelectionInput";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import type { CommandClient } from "../../app/api/client";
import type { AnchorView } from "../../shared/generated/AnchorView";
import { useReaderTools } from "./readerTools";
import {
  decodeTextBox,
  encodeTextBox,
  DEFAULT_TEXTBOX_STYLE,
  type TextBoxPayload,
  type TextBoxStyle,
} from "./annotations";
import { broadcastAnnotationChange } from "./annotationEvents";
import { ConfirmDialog } from "./ConfirmDialog";
import { isRevisionConflict, useAnnotationConflict } from "./annotationConflict";

export const TEXTBOX_COLORS = [
  "#f2b705",
  "#31c48d",
  "#3f83f8",
  "#f04f9e",
  "#9061f9",
  "#111827",
];

export type TextBoxTarget = {
  /** 放置位置:0..1 归一化坐标(相对承载容器)。 */
  x: number;
  y: number;
  /** 落库定位(页面/块);与 x/y 分开,便于缩放后仍能回到大致位置。 */
  locator: SourceLocator;
  /** 锚点文本(用于建立 Anchor;可为空字符串)。 */
  anchorText: string;
};

export type TextBoxEditorState = {
  /** 正在编辑的已存在标注(新建时为 null)。 */
  annotation: AnnotationView | null;
  /** 新建时的落点(编辑已有框时为 null)。 */
  target: TextBoxTarget | null;
  /** 初始内容与样式。 */
  text: string;
  style: TextBoxStyle;
  /** 屏幕坐标(编辑器浮层定位)。 */
  screenX: number;
  screenY: number;
};

export function TextBoxEditor({
  state,
  client,
  documentId,
  versionId,
  format,
  onClose,
  onSaved,
  onDeleted,
  onToast,
}: {
  state: TextBoxEditorState;
  client?: CommandClient;
  documentId?: string;
  versionId?: string;
  format: "pdf" | "markdown" | "docx";
  onClose: () => void;
  onSaved: (annotation: AnnotationView) => void;
  onDeleted?: () => void;
  onToast: (text: string) => void;
}): ReactNode {
  const [text, setText] = useState(state.text);
  const [style, setStyle] = useState<TextBoxStyle>(state.style);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const annotationConflict = useAnnotationConflict(client, documentId, onToast);

  useEffect(() => {
    areaRef.current?.focus();
  }, []);

  const save = useCallback(async () => {
    const body = text.trim();
    if (body.length === 0) {
      onClose();
      return;
    }
    if (!client || !documentId || !versionId) {
      onToast("当前文档不可写,无法保存注释");
      return;
    }
    setBusy(true);
    try {
      if (state.annotation !== null) {
        // 编辑已有:只更新文本与样式,不改锚点。
        const existing = decodeTextBox(state.annotation.noteText);
        const payload: TextBoxPayload = {
          x: existing?.x ?? 0.1,
          y: existing?.y ?? 0.1,
          width: existing?.width ?? 0.28,
          text: body,
          style,
        };
        const updated = await client.call<AnnotationView>("annotation_update", {
          annotationId: state.annotation.id,
          // color 是必填字段:更新内容/样式时沿用原色,漏传会被后端判为 INVALID_REQUEST。
          color: state.annotation.color,
          expectedRevision: state.annotation.revision,
          noteText: encodeTextBox(payload),
        });
        broadcastAnnotationChange({ type: "updated", annotation: updated });
        onSaved(updated);
      } else if (state.target !== null) {
        const anchorText =
          state.target.anchorText.trim().length > 0 ? state.target.anchorText : "文字注释";
        const rawSelection: RawSelectionInput = {
          documentId,
          documentVersionId: versionId,
          format,
          text: anchorText,
          blocks: [
            {
              renderKey: `textbox-${Date.now()}`,
              text: anchorText,
              blockOrder: 0,
              locator: state.target.locator,
            },
          ],
          primaryLocator: state.target.locator,
          // 页面坐标才是 page 选区;Markdown/DOCX 的文本框锚点必须是文本选区,
          // 否则 M05 会以定位类型不匹配拒绝创建锚点(文本框保存失败)。
          selectionKind: format === "pdf" ? "page" : "text",
        };
        const anchor = await client.call<AnchorView>("evidence_create_anchor", {
          rawSelection,
          idempotencyKey: client.newIdempotencyKey(),
        });
        const payload: TextBoxPayload = {
          x: state.target.x,
          y: state.target.y,
          width: 0.28,
          text: body,
          style,
        };
        const created = await client.call<AnnotationView>("annotation_create", {
          documentId,
          anchorId: anchor.id,
          color: "yellow",
          noteText: encodeTextBox(payload),
          idempotencyKey: client.newIdempotencyKey(),
        });
        broadcastAnnotationChange({ type: "created", annotation: created });
        onSaved(created);
      }
      onClose();
    } catch (error) {
      const existingBox =
        state.annotation === null ? null : decodeTextBox(state.annotation.noteText);
      if (isRevisionConflict(error) && state.annotation !== null && existingBox !== null) {
        annotationConflict.open(
          {
            anchorId: state.annotation.anchorId,
            annotationId: state.annotation.id,
            color: state.annotation.color,
            noteText: encodeTextBox({
              x: existingBox.x,
              y: existingBox.y,
              width: existingBox.width,
              text: text.trim(),
              style,
            }),
          },
          "这条文字注释已被其他操作修改(修订冲突)。请选择如何处理。",
        );
      } else {
        onToast("注释未保存,请重试");
      }
    } finally {
      setBusy(false);
    }
  }, [
    client,
    documentId,
    format,
    onClose,
    onSaved,
    onToast,
    state.annotation,
    state.target,
    style,
    text,
    versionId,
  ]);

  const remove = useCallback(async () => {
    if (!client || state.annotation === null) return;
    setDeleting(true);
    try {
      await client.call("annotation_delete", {
        annotationId: state.annotation.id,
        expectedRevision: state.annotation.revision,
      });
      broadcastAnnotationChange({ type: "deleted", annotationId: state.annotation.id });
      onDeleted?.();
    } catch {
      onToast("删除注释失败,请重试");
    } finally {
      setDeleting(false);
      setConfirmDelete(false);
    }
  }, [client, onDeleted, onToast, state.annotation]);

  return (
    <div
      className="textbox-editor"
      style={{ left: `${state.screenX}px`, top: `${state.screenY}px` }}
      role="dialog"
      aria-label="文字注释"
    >
      <textarea
        ref={areaRef}
        value={text}
        autoFocus
        placeholder="输入注释…(Ctrl+Enter 保存,Esc 取消)"
        aria-label="注释内容"
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.stopPropagation();
            onClose();
          }
          if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
            event.preventDefault();
            void save();
          }
        }}
        rows={3}
      />
      <div className="textbox-editor-tools" role="toolbar" aria-label="注释样式">
        {TEXTBOX_COLORS.map((hex) => (
          <button
            key={hex}
            type="button"
            className={`color-dot${style.color === hex ? " selected" : ""}`}
            style={{ backgroundColor: hex }}
            aria-label={`注释颜色 ${hex}`}
            title={`注释颜色 ${hex}`}
            onClick={() => setStyle((s) => ({ ...s, color: hex }))}
          />
        ))}
        <button
          type="button"
          className={`btn tiny ghost${style.bold ? " active" : ""}`}
          aria-pressed={style.bold}
          title="加粗"
          onClick={() => setStyle((s) => ({ ...s, bold: !s.bold }))}
        >
          B
        </button>
        <label className="textbox-size" title="字号">
          <span className="visually-hidden">字号</span>
          <input
            type="range"
            min={12}
            max={28}
            step={1}
            value={style.fontSize}
            aria-label="字号"
            onChange={(event) =>
              setStyle((s) => ({ ...s, fontSize: Number(event.target.value) }))
            }
          />
          <span>{style.fontSize}</span>
        </label>
        <span className="textbox-editor-spacer" />
        {state.annotation !== null ? (
          <button
            type="button"
            className="btn tiny danger-ghost"
            onClick={() => setConfirmDelete(true)}
            disabled={busy || deleting}
          >
            删除
          </button>
        ) : null}
        <button type="button" className="btn tiny ghost" onClick={onClose} disabled={busy}>
          取消
        </button>
        <button type="button" className="btn tiny" onClick={() => void save()} disabled={busy}>
          {busy ? "保存中…" : "保存"}
        </button>
      </div>
      <ConfirmDialog
        request={
          confirmDelete
            ? {
                title: "删除文字注释",
                message: "确定删除这条文字注释吗?删除后不可恢复。",
                confirmLabel: "删除",
                onConfirm: remove,
              }
            : null
        }
        busy={deleting}
        onCancel={() => {
          if (!deleting) setConfirmDelete(false);
        }}
      />
      {annotationConflict.dialog}
    </div>
  );
}

/** 渲染一个已保存的文本框(只读展示;点击进入编辑由宿主处理)。 */
export function TextBoxBubble({
  annotation,
  styleLeft,
  styleTop,
  styleWidth,
  onEdit,
}: {
  annotation: AnnotationView;
  styleLeft: string;
  styleTop: string;
  styleWidth: string;
  onEdit?: () => void;
}): ReactNode {
  const payload = decodeTextBox(annotation.noteText);
  if (payload === null) return null;
  const text = payload.text.trim();
  if (text.length === 0) return null;
  return (
    <button
      type="button"
      className="textbox-bubble"
      style={{
        left: styleLeft,
        top: styleTop,
        width: styleWidth,
        color: payload.style.color,
        fontWeight: payload.style.bold ? 650 : 400,
        fontSize: `${payload.style.fontSize}px`,
      }}
      title="点击编辑注释"
      aria-label={`文字注释:${text.slice(0, 40)}`}
      onClick={(event) => {
        event.stopPropagation();
        onEdit?.();
      }}
    >
      {text}
    </button>
  );
}

/** 当前是否处于"添加文字注释"工具。 */
export function useTextBoxTool(): boolean {
  return useReaderTools((s) => s.tool === "textbox");
}

export { DEFAULT_TEXTBOX_STYLE };
