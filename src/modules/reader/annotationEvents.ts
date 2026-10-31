// 标注变更广播:阅读器创建/更新/删除标注后,笔记面板立即更新缓存,
// 不等待下一次轮询或重新进入页面。
import type { AnnotationView } from "../../shared/generated/AnnotationView";

export type AnnotationChangeDetail =
  | { type: "created"; annotation: AnnotationView }
  | { type: "updated"; annotation: AnnotationView }
  | { type: "deleted"; annotationId: string };

export const ANNOTATIONS_CHANGED_EVENT = "knowwise-annotations-changed";

/** 广播;detail 可选(旧调用只触发失效,不携带增量)。 */
export function broadcastAnnotationChange(detail?: AnnotationChangeDetail): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent(ANNOTATIONS_CHANGED_EVENT, { detail }));
}
