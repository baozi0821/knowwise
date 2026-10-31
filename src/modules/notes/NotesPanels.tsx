// 笔记面板的「书签」「待澄清」两个一等视图。
// 设计逻辑:
// - 书签 = 回看位置;可直接为书签写笔记,把"位置"变成"思考的入口"。
// - 待澄清 = 阅读中的疑问;写答案后自动生成并关联一条笔记,
//   也可以把疑问直接交给 AI;暂缓/重新打开保留状态而不是删除。
import { useState, type ReactNode } from "react";
import {
  Bookmark,
  Check,
  CircleHelp,
  MapPin,
  MessageSquarePlus,
  NotebookPen,
  Pencil,
  Plus,
  RotateCcw,
  Trash2,
  X,
} from "lucide-react";
import { useServices } from "../../app/services";
import { toAppError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import type { AppError } from "../../shared/generated/AppError";
import type { BookmarkView } from "../../shared/generated/BookmarkView";
import type { ClarificationView } from "../../shared/generated/ClarificationView";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import { locatorLabel } from "./noteItems";
import { jumpToLocator } from "./jumpToLocator";

type ShowToast = (kind: "success" | "error", message: string) => void;

const timeFormatter = new Intl.DateTimeFormat("zh-CN", {
  month: "numeric",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

function formatTime(ms: number): string {
  if (!Number.isFinite(ms)) return "";
  return timeFormatter.format(new Date(ms));
}

function errorText(error: AppError | Error | null): string {
  if (error === null) return "";
  if ("code" in error) return describeAppError(error).title;
  return error.message;
}

function PanelError({ error }: { error: AppError | Error }) {
  return (
    <p className="notes-panel-error" role="alert">
      操作未完成:{errorText(error)}
    </p>
  );
}

// —— 书签 ——

export function BookmarksPanel({
  documentId,
  items,
  loading,
  error,
  positionLocator,
  showToast,
  onChanged,
  onCreateNote,
}: {
  documentId: string;
  items: BookmarkView[];
  loading: boolean;
  error: AppError | null;
  positionLocator: SourceLocator | null;
  showToast: ShowToast;
  onChanged: () => void;
  onCreateNote: (bookmark: BookmarkView) => void;
}): ReactNode {
  const { client } = useServices();
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<AppError | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [notingId, setNotingId] = useState<string | null>(null);
  const [noteDraft, setNoteDraft] = useState("");

  async function addCurrent() {
    if (positionLocator === null || busy) return;
    setBusy(true);
    setActionError(null);
    try {
      await client.call("bookmark_toggle", {
        documentId,
        anchorId: undefined,
        locator: positionLocator,
        label: locatorLabel(positionLocator) ?? "当前位置",
        expectedState: true,
      });
      onChanged();
      showToast("success", "已收藏当前位置");
    } catch (caught) {
      setActionError(toAppError(caught));
    } finally {
      setBusy(false);
    }
  }

  async function rename(bookmark: BookmarkView) {
    const label = renameDraft.trim();
    setRenamingId(null);
    if (label === "" || label === bookmark.label) return;
    try {
      await client.call("bookmark_rename", {
        documentId,
        bookmarkId: bookmark.id,
        label,
      });
      onChanged();
    } catch (caught) {
      setActionError(toAppError(caught));
    }
  }

  async function saveNote(bookmark: BookmarkView) {
    setNotingId(null);
    try {
      await client.call("bookmark_rename", {
        documentId,
        bookmarkId: bookmark.id,
        label: bookmark.label,
        note: noteDraft.trim(),
      });
      onChanged();
      showToast("success", "书签备注已保存");
    } catch (caught) {
      setActionError(toAppError(caught));
    }
  }

  async function remove(bookmark: BookmarkView) {
    try {
      await client.call("bookmark_toggle", {
        documentId,
        anchorId: bookmark.anchorId ?? undefined,
        locator: bookmark.locator,
        label: bookmark.label,
        expectedState: false,
      });
      onChanged();
    } catch (caught) {
      setActionError(toAppError(caught));
    }
  }

  return (
    <section className="notes-panel-view" aria-label="书签">
      <header className="notes-panel-head">
        <div>
          <h3>
            <Bookmark className="w-4 h-4" aria-hidden />
            书签
          </h3>
          <p>把想回看的位置钉住;也可以从书签直接写一条笔记。</p>
        </div>
      </header>

      <div className="notes-bookmark-add">
        <button
          type="button"
          className="btn small primary"
          disabled={positionLocator === null || busy}
          onClick={() => void addCurrent()}
          title="把当前阅读位置加入书签"
        >
          <Plus className="w-3.5 h-3.5" aria-hidden />
          收藏当前位置
        </button>
        <span className="notes-hint">
          {positionLocator === null
            ? "翻动页面后可收藏"
            : `当前:${locatorLabel(positionLocator) ?? "阅读位置"}`}
        </span>
      </div>

      {error !== null ? <PanelError error={error} /> : null}
      {actionError !== null ? <PanelError error={actionError} /> : null}

      {loading ? <p className="notes-hint center">加载书签…</p> : null}
      {!loading && items.length === 0 ? (
        <div className="notes-panel-empty">
          <Bookmark className="w-6 h-6" aria-hidden />
          <p>还没有书签</p>
          <p className="notes-hint">读到关键处,点「收藏当前位置」;它不会改动原文。</p>
        </div>
      ) : null}

      <ul className="notes-bookmark-list">
        {items.map((bookmark) => {
          const where = locatorLabel(bookmark.locator) ?? "阅读位置";
          return (
            <li key={bookmark.id} className="notes-bookmark-card">
              <span className="notes-bookmark-pin" aria-hidden>
                <MapPin className="w-3.5 h-3.5" />
              </span>
              <div className="notes-bookmark-main">
                {renamingId === bookmark.id ? (
                  <form
                    className="notes-bookmark-rename"
                    onSubmit={(event) => {
                      event.preventDefault();
                      void rename(bookmark);
                    }}
                  >
                    <input
                      autoFocus
                      value={renameDraft}
                      aria-label="书签名称"
                      maxLength={200}
                      onChange={(event) => setRenameDraft(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Escape") {
                          setRenameDraft(bookmark.label || where);
                          setRenamingId(null);
                        }
                      }}
                      onBlur={() => void rename(bookmark)}
                    />
                    <button type="submit" className="icon-btn small" aria-label="保存书签名称">
                      <Check className="w-3 h-3" aria-hidden />
                    </button>
                  </form>
                ) : (
                  <button
                    type="button"
                    className="notes-bookmark-title"
                    title="回跳到原文位置"
                    onClick={() => jumpToLocator(documentId, bookmark.locator)}
                  >
                    {bookmark.label || where}
                  </button>
                )}
                <span className="notes-bookmark-meta">
                  <span className="notes-bookmark-page">{where}</span>
                  <span>{formatTime(bookmark.createdAt)}</span>
                </span>
                {notingId === bookmark.id ? (
                  <form
                    className="notes-bookmark-note-edit"
                    onSubmit={(event) => {
                      event.preventDefault();
                      void saveNote(bookmark);
                    }}
                  >
                    <textarea
                      autoFocus
                      rows={2}
                      value={noteDraft}
                      aria-label="书签备注"
                      placeholder="为什么收藏这里?(可选)"
                      onChange={(event) => setNoteDraft(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Escape") setNotingId(null);
                        if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
                          event.preventDefault();
                          void saveNote(bookmark);
                        }
                      }}
                    />
                    <button type="submit" className="btn tiny">
                      保存备注
                    </button>
                  </form>
                ) : bookmark.note ? (
                  <button
                    type="button"
                    className="notes-bookmark-note"
                    title="编辑备注"
                    onClick={() => {
                      setNotingId(bookmark.id);
                      setNoteDraft(bookmark.note);
                    }}
                  >
                    {bookmark.note}
                  </button>
                ) : null}
              </div>
              <div className="notes-bookmark-actions">
                <button
                  type="button"
                  className="icon-btn small"
                  title="在此写笔记"
                  aria-label="从书签写笔记"
                  onClick={() => onCreateNote(bookmark)}
                >
                  <NotebookPen className="w-3.5 h-3.5" aria-hidden />
                </button>
                <button
                  type="button"
                  className="icon-btn small"
                  title="备注"
                  aria-label="编辑书签备注"
                  onClick={() => {
                    setNotingId(bookmark.id);
                    setNoteDraft(bookmark.note);
                  }}
                >
                  <NotebookPen className="w-3 h-3" aria-hidden />
                </button>
                <button
                  type="button"
                  className="icon-btn small"
                  title="重命名"
                  aria-label="重命名书签"
                  onClick={() => {
                    setRenamingId(bookmark.id);
                    setRenameDraft(bookmark.label || where);
                  }}
                >
                  <Pencil className="w-3 h-3" aria-hidden />
                </button>
                <button
                  type="button"
                  className="icon-btn small danger-ghost"
                  title="移除书签"
                  aria-label="移除书签"
                  onClick={() => void remove(bookmark)}
                >
                  <Trash2 className="w-3 h-3" aria-hidden />
                </button>
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

// —— 待澄清 ——

const STATUS_LABELS: Record<ClarificationView["status"], string> = {
  pending: "待澄清",
  resolved: "已解决",
  deferred: "已暂缓",
};

const STATUS_RANK: Record<ClarificationView["status"], number> = {
  pending: 0,
  deferred: 1,
  resolved: 2,
};

export function ClarificationsPanel({
  documentId,
  items,
  loading,
  error,
  draft,
  onDraftChange,
  source,
  onClearSource,
  onAdd,
  onResolve,
  onDefer,
  onReopen,
  onRemove,
  onAskAi,
  onOpenNote,
  onRemark,
}: {
  documentId: string;
  items: ClarificationView[];
  loading: boolean;
  error: AppError | null;
  draft: string;
  onDraftChange: (value: string) => void;
  source: { quote: string; locator: SourceLocator | null } | null;
  onClearSource: () => void;
  onAdd: () => Promise<void>;
  onResolve: (item: ClarificationView, answer: string) => Promise<void>;
  onDefer: (item: ClarificationView) => Promise<void>;
  onReopen: (item: ClarificationView) => Promise<void>;
  onRemove: (item: ClarificationView) => Promise<void>;
  onAskAi: (item: ClarificationView) => void;
  onOpenNote: (noteId: string) => void;
  onRemark: (item: ClarificationView, remark: string) => Promise<void>;
}): ReactNode {
  const [answeringId, setAnsweringId] = useState<string | null>(null);
  const [answerDraft, setAnswerDraft] = useState("");
  const [busyId, setBusyId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [actionError, setActionError] = useState<AppError | null>(null);
  const [notingId, setNotingId] = useState<string | null>(null);
  const [remarkDraft, setRemarkDraft] = useState("");
  const [remarkBusyId, setRemarkBusyId] = useState<string | null>(null);

  const sorted = [...items].sort((a, b) => {
    if (STATUS_RANK[a.status] !== STATUS_RANK[b.status]) {
      return STATUS_RANK[a.status] - STATUS_RANK[b.status];
    }
    return b.updatedAt - a.updatedAt;
  });

  async function submitAdd() {
    if (draft.trim() === "" || adding) return;
    setAdding(true);
    setActionError(null);
    try {
      await onAdd();
    } catch (caught) {
      setActionError(toAppError(caught));
    } finally {
      setAdding(false);
    }
  }

  async function submitRemark(item: ClarificationView) {
    if (remarkBusyId !== null) return;
    setRemarkBusyId(item.id);
    setActionError(null);
    try {
      await onRemark(item, remarkDraft.trim());
      setNotingId(null);
      setRemarkDraft("");
    } catch (caught) {
      setActionError(toAppError(caught));
    } finally {
      setRemarkBusyId(null);
    }
  }

  async function submitAnswer(item: ClarificationView) {
    if (answerDraft.trim() === "" || busyId !== null) return;
    setBusyId(item.id);
    setActionError(null);
    try {
      await onResolve(item, answerDraft);
      setAnswerDraft("");
      setAnsweringId(null);
    } catch (caught) {
      setActionError(toAppError(caught));
    } finally {
      setBusyId(null);
    }
  }

  return (
    <section className="notes-panel-view" aria-label="待澄清">
      <header className="notes-panel-head">
        <div>
          <h3>
            <CircleHelp className="w-4 h-4" aria-hidden />
            待澄清
          </h3>
          <p>阅读中的疑问先记下来;写答案后会生成一条关联笔记,也可以直接问 AI。</p>
        </div>
      </header>

      <div className="notes-clarify-composer">
        {source !== null ? (
          <div className="notes-clarify-source">
            <span className="notes-clarify-source-label">来自选区</span>
            <span className="notes-clarify-source-text">{source.quote}</span>
            <button
              type="button"
              className="icon-btn small"
              aria-label="移除选区来源"
              title="移除"
              onClick={onClearSource}
            >
              <X className="w-3 h-3" aria-hidden />
            </button>
          </div>
        ) : null}
        <textarea
          id="clarify-input"
          rows={4}
          value={draft}
          placeholder="哪里没读懂?写下来(Enter 记录,Shift+Enter 换行)"
          aria-label="新增待澄清"
          onChange={(event) => onDraftChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              void submitAdd();
            }
          }}
        />
        <div className="notes-clarify-composer-footer">
          <span className="notes-hint">
            {source !== null ? "已带入选区位置" : "也可以先在正文划词,再点「待澄清」"}
          </span>
          <button
            type="button"
            className="btn small primary"
            disabled={draft.trim() === "" || adding}
            onClick={() => void submitAdd()}
          >
            <Plus className="w-3.5 h-3.5" aria-hidden />
            {adding ? "记录中…" : "记下疑问"}
          </button>
        </div>
      </div>

      {error !== null ? <PanelError error={error} /> : null}
      {actionError !== null ? <PanelError error={actionError} /> : null}

      {loading ? <p className="notes-hint center">加载待澄清…</p> : null}
      {!loading && items.length === 0 ? (
        <div className="notes-panel-empty">
          <CircleHelp className="w-6 h-6" aria-hidden />
          <p>还没有待澄清</p>
          <p className="notes-hint">把没读懂的地方记下来,解决后会自动变成一条笔记。</p>
        </div>
      ) : null}

      <ul className="notes-clarify-list">
        {sorted.map((item) => {
          const quote = item.sourceSnapshot.text;
          const linkedNoteId =
            item.resolutionRef?.refType === "note" ? item.resolutionRef.refId : null;
          return (
            <li key={item.id} className={`notes-clarify-card status-${item.status}`}>
              <div className="notes-clarify-card-head">
                <span className={`notes-clarify-status ${item.status}`}>
                  {STATUS_LABELS[item.status]}
                </span>
                <span className="notes-clarify-time">{formatTime(item.updatedAt)}</span>
              </div>
              <p className="notes-clarify-question">{item.questionText}</p>
              {quote !== "" ? (
                <button
                  type="button"
                  className="notes-clarify-quote"
                  title="回跳原文"
                  onClick={() =>
                    jumpToLocator(documentId, item.sourceSnapshot.locator ?? null)
                  }
                >
                  {quote}
                </button>
              ) : null}

              {notingId === item.id ? (
                <div className="notes-clarify-remark-edit">
                  <textarea
                    autoFocus
                    rows={2}
                    value={remarkDraft}
                    aria-label="待澄清备注"
                    placeholder="补充上下文/线索(可选)"
                    onChange={(event) => setRemarkDraft(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") setNotingId(null);
                      if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
                        event.preventDefault();
                        void submitRemark(item);
                      }
                    }}
                  />
                  <div className="notes-clarify-answer-actions">
                    <button
                      type="button"
                      className="btn small primary"
                      disabled={remarkBusyId === item.id}
                      onClick={() => void submitRemark(item)}
                    >
                      {remarkBusyId === item.id ? "保存中…" : "保存备注"}
                    </button>
                    <button
                      type="button"
                      className="btn small ghost"
                      onClick={() => setNotingId(null)}
                    >
                      取消
                    </button>
                  </div>
                </div>
              ) : item.remark !== "" ? (
                <button
                  type="button"
                  className="notes-clarify-remark"
                  title="编辑备注"
                  onClick={() => {
                    setNotingId(item.id);
                    setRemarkDraft(item.remark);
                  }}
                >
                  {item.remark}
                </button>
              ) : null}

              {answeringId === item.id ? (
                <div className="notes-clarify-answer">
                  <textarea
                    autoFocus
                    rows={5}
                    value={answerDraft}
                    placeholder="写下你的答案或结论…(Ctrl+Enter 保存)"
                    aria-label="待澄清答案"
                    onChange={(event) => setAnswerDraft(event.target.value)}
                    onKeyDown={(event) => {
                      if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
                        event.preventDefault();
                        void submitAnswer(item);
                      }
                    }}
                  />
                  <div className="notes-clarify-answer-actions">
                    <button
                      type="button"
                      className="btn small primary"
                      disabled={answerDraft.trim() === "" || busyId !== null}
                      onClick={() => void submitAnswer(item)}
                    >
                      <Check className="w-3 h-3" aria-hidden />
                      保存答案并关联笔记
                    </button>
                    <button
                      type="button"
                      className="btn small ghost"
                      onClick={() => setAnsweringId(null)}
                    >
                      取消
                    </button>
                  </div>
                </div>
              ) : (
                <div className="notes-clarify-actions">
                  {item.status !== "resolved" ? (
                    <>
                      <button
                        type="button"
                        className="btn small"
                        title="写下答案;保存后自动创建关联笔记"
                        onClick={() => {
                          setAnsweringId(item.id);
                          setAnswerDraft("");
                        }}
                      >
                        <NotebookPen className="w-3 h-3" aria-hidden />
                        写答案
                      </button>
                      <button
                        type="button"
                        className="btn small ghost"
                        title="把这个问题交给 AI"
                        onClick={() => onAskAi(item)}
                      >
                        <MessageSquarePlus className="w-3 h-3" aria-hidden />
                        问 AI
                      </button>
                      {item.status === "pending" ? (
                        <button
                          type="button"
                          className="btn small ghost"
                          title="先放一放,之后可以重新打开"
                          onClick={() => void onDefer(item)}
                        >
                          暂缓
                        </button>
                      ) : (
                        <button
                          type="button"
                          className="btn small ghost"
                          onClick={() => void onReopen(item)}
                        >
                          <RotateCcw className="w-3 h-3" aria-hidden />
                          重新打开
                        </button>
                      )}
                    </>
                  ) : linkedNoteId !== null ? (
                    <button
                      type="button"
                      className="btn small"
                      title="打开关联的笔记"
                      onClick={() => onOpenNote(linkedNoteId)}
                    >
                      <NotebookPen className="w-3 h-3" aria-hidden />
                      查看关联笔记
                    </button>
                  ) : null}
                  {notingId !== item.id ? (
                    <button
                      type="button"
                      className="icon-btn small"
                      title="备注"
                      aria-label="编辑待澄清备注"
                      onClick={() => {
                        setNotingId(item.id);
                        setRemarkDraft(item.remark ?? "");
                      }}
                    >
                      <Pencil className="w-3 h-3" aria-hidden />
                    </button>
                  ) : null}
                  <button
                    type="button"
                    className="icon-btn small danger-ghost"
                    title="删除这条待澄清"
                    aria-label="删除待澄清"
                    onClick={() => void onRemove(item)}
                  >
                    <Trash2 className="w-3 h-3" aria-hidden />
                  </button>
                </div>
              )}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
