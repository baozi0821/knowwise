// 标注修订冲突(M06 乐观并发):不静默覆盖,也不把冲突伪装成成功。
// 三个动作:
// - keep:重新读取最新标注,再用我的内容写入(标注已被删除则提示无法保留);
// - overwrite:同 keep,但标注缺失时用同一 anchor 重建(明确覆盖);
// - discard:放弃我的修改,以最新数据为准。
import { useState, type ReactNode } from "react";
import type { AnnotationView } from "../../shared/generated/AnnotationView";
import type { AnnotationColor } from "../../shared/generated/AnnotationColor";
import type { CommandClient } from "../../app/api/client";
import { ApiError } from "../../app/api/client";
import { broadcastAnnotationChange } from "./annotationEvents";

export function isRevisionConflict(error: unknown): boolean {
  return error instanceof ApiError && error.appError.code === "CONFLICT_REVISION_MISMATCH";
}

export type ConflictChange = {
  anchorId: string;
  /** 发生冲突的标注 ID;同一 anchor 可能有多条标注,优先按 ID 定位。 */
  annotationId?: string;
  color: AnnotationColor;
  /** null = 删除该标注。 */
  noteText: string | null;
  /** 可读备注;undefined = 不修改备注。 */
  remark?: string;
};

export type ConflictMode = "keep" | "overwrite" | "discard";

/** 应用一次冲突决策;返回是否真正写入/删除。 */
export async function resolveAnnotationConflict(
  client: CommandClient,
  documentId: string,
  change: ConflictChange,
  mode: ConflictMode,
): Promise<boolean> {
  if (mode === "discard") return false;
  const list = await client.call<AnnotationView[]>("annotation_list", { documentId });
  const current =
    (change.annotationId === undefined
      ? undefined
      : list.find((annotation) => annotation.id === change.annotationId)) ??
    list.find((annotation) => annotation.anchorId === change.anchorId);
  if (change.noteText === null) {
    if (current === undefined) return false;
    await client.call("annotation_delete", {
      annotationId: current.id,
      expectedRevision: current.revision,
    });
    broadcastAnnotationChange({ type: "deleted", annotationId: current.id });
    return true;
  }
  if (current !== undefined) {
    const updated = await client.call<AnnotationView>("annotation_update", {
      annotationId: current.id,
      color: change.color,
      noteText: change.noteText,
      ...(change.remark !== undefined ? { remark: change.remark } : {}),
      expectedRevision: current.revision,
    });
    broadcastAnnotationChange({ type: "updated", annotation: updated });
    return true;
  }
  if (mode === "overwrite") {
    const created = await client.call<AnnotationView>("annotation_create", {
      documentId,
      anchorId: change.anchorId,
      color: change.color,
      noteText: change.noteText,
      ...(change.remark !== undefined ? { remark: change.remark } : {}),
      idempotencyKey: client.newIdempotencyKey(),
    });
    broadcastAnnotationChange({ type: "created", annotation: created });
    return true;
  }
  throw new Error("annotation_missing");
}

export type ConflictRequest = {
  change: ConflictChange;
  message: string;
  onResolved?: () => void;
};

export function AnnotationConflictDialog({
  request,
  busy,
  onChoose,
  onCancel,
}: {
  request: ConflictRequest | null;
  busy: boolean;
  onChoose: (mode: ConflictMode) => void;
  onCancel: () => void;
}): ReactNode {
  if (request === null) return null;
  return (
    <div className="kw-confirm-overlay" role="presentation">
      <div
        className="kw-confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-label="标注修订冲突"
      >
        <div className="kw-confirm-head">
          <strong>标注已被修改</strong>
        </div>
        <p className="kw-confirm-message">{request.message}</p>
        <div className="kw-confirm-actions conflict-actions">
          <button type="button" className="btn small ghost" disabled={busy} onClick={onCancel}>
            放弃我的修改
          </button>
          <button
            type="button"
            className="btn small ghost"
            disabled={busy}
            onClick={() => onChoose("keep")}
          >
            重新加载并保留我的修改
          </button>
          <button
            type="button"
            className="btn small danger"
            disabled={busy}
            onClick={() => onChoose("overwrite")}
          >
            覆盖
          </button>
        </div>
      </div>
    </div>
  );
}

/** 便捷状态容器:视图层捕获冲突后调用 open。 */
export function useAnnotationConflict(
  client: CommandClient | undefined,
  documentId: string | undefined,
  onDone?: (message: string) => void,
): {
  request: ConflictRequest | null;
  busy: boolean;
  open: (change: ConflictChange, message: string) => void;
  dialog: ReactNode;
} {
  const [request, setRequest] = useState<ConflictRequest | null>(null);
  const [busy, setBusy] = useState(false);
  const choose = async (mode: ConflictMode) => {
    if (request === null || client === undefined || documentId === undefined) return;
    setBusy(true);
    try {
      const wrote = await resolveAnnotationConflict(client, documentId, request.change, mode);
      onDone?.(
        mode === "discard"
          ? "已放弃我的修改,显示最新数据"
          : wrote
            ? mode === "overwrite"
              ? "已覆盖为我的修改"
              : "已保留我的修改"
            : "没有需要写入的变化",
      );
    } catch {
      onDone?.("冲突未解决:标注可能已被删除;可选择覆盖重建或放弃");
    } finally {
      setBusy(false);
      setRequest(null);
      request.onResolved?.();
    }
  };
  return {
    request,
    busy,
    open: (change, message) => setRequest({ change, message }),
    dialog: (
      <AnnotationConflictDialog
        request={request}
        busy={busy}
        onChoose={(mode) => void choose(mode)}
        onCancel={() => void choose("discard")}
      />
    ),
  };
}
