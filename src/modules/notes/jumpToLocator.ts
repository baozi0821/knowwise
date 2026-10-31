// 笔记/书签/待澄清 → 阅读器定位(事件由阅读器监听)。
import type { SourceLocator } from "../../shared/generated/SourceLocator";

export function jumpToLocator(documentId: string, locator: SourceLocator | null): void {
  if (locator === null) return;
  if (locator.kind === "pdf") {
    window.dispatchEvent(
      new CustomEvent("knowwise-goto", {
        detail: { documentId, locator: { kind: "pdf", pageNumber: locator.pageNumber } },
      }),
    );
    return;
  }
  if (locator.kind === "markdown") {
    window.dispatchEvent(
      new CustomEvent("knowwise-goto-text", {
        detail: { documentId, start: locator.startOffset, end: locator.endOffset },
      }),
    );
  }
  if (locator.kind === "docx") {
    window.dispatchEvent(
      new CustomEvent("knowwise-goto-text", {
        detail: { documentId, start: locator.bodyBlockStart, end: locator.bodyBlockEnd },
      }),
    );
  }
}
