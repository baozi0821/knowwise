// M06 自动保存状态机(《M06》§4)。
// 关键契约:
// - 停止输入 500ms 后触发 note_save;切页/失焦/关闭抽屉/切文档/退出前立即 flush。
// - 只有 Rust 事务成功返回后才显示"已保存";崩溃时最多丢失尚未成功保存的
//   当前编辑,绝不把已显示"已保存"的数据丢失。
// - 保存冲突(CONFLICT_REVISION_MISMATCH)保留本地编辑并进入 conflict 状态,
//   不自动覆盖。
import type { NoteStatus } from "../../shared/generated/NoteStatus";

export type AutosaveStatus =
  | "idle"
  | "unsaved"
  | "saving"
  | "saved"
  | "error"
  | "conflict";

/** 保存失败的机器可读原因(与 AppError.code 对齐)。 */
export type AutosaveFailure = { code: string; retryable: boolean };

export type NoteDraft = {
  /** 普通笔记:note_save 的 noteId。 */
  noteId?: string;
  /** 标注:annotation_update 的 annotationId(与 noteId 二选一)。 */
  annotationId?: string;
  /** 面板侧目标条目 key:切条目后仍能把迟到的保存送对目标。 */
  targetKey?: string;
  body: string;
  tags: string[];
  /** 笔记状态(标注没有状态,忽略)。 */
  status?: NoteStatus;
  expectedRevision: number;
};

export interface SaveOutcome {
  noteId: string;
  revision: number;
}

export interface AutosaveDeps {
  /** 实际执行 note_save 的函数(注入 invoke 适配层)。 */
  save: (draft: NoteDraft, idempotencyKey: string) => Promise<SaveOutcome>;
  /** 停止输入后的防抖毫秒数;默认 500ms。 */
  delayMs?: number;
  /** 幂等键生成器:同一逻辑保存重复送达必须同键。 */
  newIdempotencyKey?: () => string;
}

export interface AutosaveController {
  /** 编辑输入:标记未保存并(重新)安排防抖保存。 */
  update(draft: NoteDraft): void;
  /** 立即保存(失焦/切页/退出前);进行中的保存会等待完成后再次补存。 */
  flushNow(): Promise<void>;
  /** 当前状态。 */
  status(): AutosaveStatus;
  /** 最近一次失败原因(状态 error/conflict 时有效)。 */
  lastFailure(): AutosaveFailure | null;
  /** 最近一次成功保存的结果(状态 saved 时有效)。 */
  lastSaved(): SaveOutcome | null;
  /** 当前未保存的最新草稿(崩溃恢复提示用)。 */
  pendingDraft(): NoteDraft | null;
  /** 丢弃未保存草稿(冲突后放弃/另存副本时使用,不再重试旧内容)。 */
  discard(): void;
  /** 清理定时器。 */
  dispose(): void;
}

const DEFAULT_DELAY_MS = 500;

export function createAutosaveController(deps: AutosaveDeps): AutosaveController {
  const delayMs = deps.delayMs ?? DEFAULT_DELAY_MS;
  const newKey = deps.newIdempotencyKey ?? (() => `autosave-${Date.now()}-${counter()}`);
  let keyCounter = 0;
  function counter(): number {
    keyCounter += 1;
    return keyCounter;
  }

  let status: AutosaveStatus = "idle";
  let failure: AutosaveFailure | null = null;
  let saved: SaveOutcome | null = null;
  let draft: NoteDraft | null = null;
  let draftKey: string | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let saving = false;
  /** 保存期间是否有新输入:补存只处理新输入,失败后不自动重试。 */
  let dirtyDuringSave = false;
  let flushWaiters: Array<() => void> = [];

  function clearTimer() {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  }

  async function runSave(): Promise<void> {
    if (saving) return;
    const current = draft;
    const currentKey = draftKey;
    if (!current || !currentKey) return;
    saving = true;
    dirtyDuringSave = false;
    status = "saving";
    try {
      const outcome = await deps.save(current, currentKey);
      // 仅当本次保存仍是对应草稿的保存时提交状态(丢弃迟到结果之外的竞态)。
      if (draft === current) {
        saved = outcome;
        status = "saved";
        failure = null;
        draft = null;
        draftKey = null;
      }
    } catch (e) {
      const code = readErrorCode(e);
      if (draft === current) {
        // 冲突:保留本地编辑内容,不自动覆盖(由 UI 提供查看差异/另存副本)。
        status = code === "CONFLICT_REVISION_MISMATCH" ? "conflict" : "error";
        failure = { code, retryable: code !== "CONFLICT_REVISION_MISMATCH" };
      }
    } finally {
      saving = false;
      // flushNow 等待者解除。
      const waiters = flushWaiters;
      flushWaiters = [];
      for (const w of waiters) w();
      // 保存期间又有新输入 ⇒ 立即补存;保存失败本身不自动重试。
      if (dirtyDuringSave && draft && status !== "conflict") {
        await runSave();
      }
    }
  }

  return {
    update(next: NoteDraft) {
      draft = next;
      // 每次编辑产生新的逻辑保存(新幂等键)。
      draftKey = newKey();
      status = "unsaved";
      failure = null;
      if (saving) {
        // 保存进行中:标记补存,由保存完成后的 finally 触发。
        dirtyDuringSave = true;
        return;
      }
      clearTimer();
      timer = setTimeout(() => {
        timer = null;
        void runSave();
      }, delayMs);
    },
    async flushNow() {
      clearTimer();
      if (!draft) return;
      if (saving) {
        await new Promise<void>((resolve) => {
          flushWaiters.push(resolve);
        });
        // 等待完成后若仍有未保存草稿则继续保存。
        if (!draft || saving) return;
      }
      await runSave();
    },
    status: () => status,
    lastFailure: () => failure,
    lastSaved: () => saved,
    pendingDraft: () => draft,
    discard() {
      clearTimer();
      draft = null;
      draftKey = null;
      dirtyDuringSave = false;
      failure = null;
      status = "idle";
    },
    dispose() {
      clearTimer();
      flushWaiters = [];
    },
  };
}

function readErrorCode(e: unknown): string {
  if (e && typeof e === "object") {
    if ("code" in e && typeof e.code === "string") return e.code;
    // ApiError:错误信封在 appError 上(冲突必须进入 conflict 状态,不能当普通失败)。
    const appError = (e as { appError?: { code?: unknown } }).appError;
    if (appError && typeof appError.code === "string") return appError.code;
  }
  return "INTERNAL";
}
