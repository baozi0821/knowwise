// 阅读区工具栏的紧凑扩展:阅读简报导出与阅读偏好。
// 这两项原先各占一整行(.reader-brief-bar / .reader-prefs-bar),
// 会把阅读工具栏推离标签栏;现在作为工具栏末端的控制组渲染,
// 状态仍由 ReaderRegion 持有,经 context 下发(MD/DOCX 与 PDF 工具栏共用)。
import { createContext, useContext, type ReactNode, type RefObject } from "react";
import { FileDown } from "lucide-react";
import {
  READER_FONT_SIZE_OPTIONS,
  READER_LINE_SPACING_OPTIONS,
  READER_MEASURE_OPTIONS,
  type ReaderPrefs,
} from "./readerPrefs";

export type ReaderToolbarExtrasState = {
  showPrefs: boolean;
  prefsOpen: boolean;
  onTogglePrefs: () => void;
  prefs: ReaderPrefs;
  onPrefs: (patch: Partial<ReaderPrefs>) => void;
  onResetPrefs: () => void;
  prefsBarRef: RefObject<HTMLDivElement>;
  briefFormat: "markdown" | "html";
  onBriefFormat: (format: "markdown" | "html") => void;
  briefBusy: boolean;
  briefNotice: string | null;
  onExportBrief: () => void;
};

const ReaderToolbarExtrasContext = createContext<ReaderToolbarExtrasState | null>(null);

export function ReaderToolbarExtrasProvider({
  state,
  children,
}: {
  state: ReaderToolbarExtrasState;
  children: ReactNode;
}): ReactNode {
  return (
    <ReaderToolbarExtrasContext.Provider value={state}>
      {children}
    </ReaderToolbarExtrasContext.Provider>
  );
}

export function ReaderToolbarExtras(): ReactNode {
  const state = useContext(ReaderToolbarExtrasContext);
  if (state === null) return null;
  return (
    <div className="reader-toolbar-group reader-toolbar-extras" ref={state.prefsBarRef}>
      {state.briefNotice !== null ? (
        <span className="reader-toolbar-note" role="status" title={state.briefNotice}>
          {state.briefNotice}
        </span>
      ) : null}
      <button
        type="button"
        className="icon-btn"
        aria-label="导出阅读简报"
        title="导出阅读简报(原文引用 + 问答 + 标注)"
        disabled={state.briefBusy}
        onClick={state.onExportBrief}
      >
        <FileDown className="w-4 h-4" aria-hidden="true" />
      </button>
      <select
        className="reader-brief-format"
        aria-label="简报格式"
        value={state.briefFormat}
        disabled={state.briefBusy}
        onChange={(event) =>
          state.onBriefFormat(event.target.value === "html" ? "html" : "markdown")
        }
      >
        <option value="markdown">Markdown</option>
        <option value="html">HTML</option>
      </select>
      {state.showPrefs ? (
        <div className="reader-prefs-wrap">
          <button
            type="button"
            className={`icon-btn reader-prefs-trigger${state.prefsOpen ? " active" : ""}`}
            title="阅读偏好:字号 / 行距 / 版心"
            aria-label="阅读偏好"
            aria-haspopup="dialog"
            aria-expanded={state.prefsOpen}
            onClick={state.onTogglePrefs}
          >
            <span aria-hidden="true">Aa</span>
          </button>
          {state.prefsOpen ? (
            <div className="reader-prefs-popover" role="dialog" aria-label="阅读偏好">
              <div className="reader-prefs-row" role="radiogroup" aria-label="字号">
                <span className="reader-prefs-label" aria-hidden="true">
                  字号
                </span>
                {READER_FONT_SIZE_OPTIONS.map((option) => (
                  <button
                    key={option.value}
                    type="button"
                    role="radio"
                    aria-checked={state.prefs.fontSize === option.value}
                    className={`reader-prefs-option${
                      state.prefs.fontSize === option.value ? " active" : ""
                    }`}
                    onClick={() => state.onPrefs({ fontSize: option.value })}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
              <div className="reader-prefs-row" role="radiogroup" aria-label="行距">
                <span className="reader-prefs-label" aria-hidden="true">
                  行距
                </span>
                {READER_LINE_SPACING_OPTIONS.map((option) => (
                  <button
                    key={option.value}
                    type="button"
                    role="radio"
                    aria-checked={state.prefs.lineSpacing === option.value}
                    className={`reader-prefs-option${
                      state.prefs.lineSpacing === option.value ? " active" : ""
                    }`}
                    onClick={() => state.onPrefs({ lineSpacing: option.value })}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
              <div className="reader-prefs-row" role="radiogroup" aria-label="版心宽度">
                <span className="reader-prefs-label" aria-hidden="true">
                  版心
                </span>
                {READER_MEASURE_OPTIONS.map((option) => (
                  <button
                    key={option.value}
                    type="button"
                    role="radio"
                    aria-checked={state.prefs.measure === option.value}
                    className={`reader-prefs-option${
                      state.prefs.measure === option.value ? " active" : ""
                    }`}
                    onClick={() => state.onPrefs({ measure: option.value })}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
              <button
                type="button"
                className="reader-prefs-reset"
                onClick={state.onResetPrefs}
              >
                恢复默认
              </button>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
