// 笔记编辑器撤销/重做栈(纯函数,单测见 undoStack.test.ts)。
// 编辑器是受控组件:Ctrl+Z / Ctrl+Shift+Z 走这份栈,而不是浏览器原生撤销,
// 这样工具栏插入、截图插入、手绘插入都能被同一套历史覆盖。
export type UndoStack = {
  /** 记录一个新状态(与当前状态相同则忽略)。 */
  push: (value: string) => void;
  /** 后退一步并返回该状态;无历史返回 null。 */
  undo: () => string | null;
  /** 前进一步并返回该状态;无重做返回 null。 */
  redo: () => string | null;
  /** 当前状态。 */
  current: () => string;
  canUndo: () => boolean;
  canRedo: () => boolean;
  /** 历史状态数量(测试用)。 */
  size: () => number;
  /** 重置整段历史(切换条目时用)。 */
  reset: (value: string) => void;
};

/**
 * 创建撤销栈:
 * - `push` 截断 redo 分支后追加,超过 `limit` 丢弃最旧状态;
 * - `limit` 至少为 2,保证「当前 + 上一步」可用。
 */
export function createUndoStack(initial: string, limit = 200): UndoStack {
  const capacity = Math.max(2, Math.floor(limit));
  let entries: string[] = [initial];
  let index = 0;

  return {
    push(value: string) {
      if (value === entries[index]) return;
      entries = entries.slice(0, index + 1);
      entries.push(value);
      if (entries.length > capacity) entries = entries.slice(entries.length - capacity);
      index = entries.length - 1;
    },
    undo() {
      if (index <= 0) return null;
      index -= 1;
      return entries[index]!;
    },
    redo() {
      if (index >= entries.length - 1) return null;
      index += 1;
      return entries[index]!;
    },
    current: () => entries[index]!,
    canUndo: () => index > 0,
    canRedo: () => index < entries.length - 1,
    size: () => entries.length,
    reset(value: string) {
      entries = [value];
      index = 0;
    },
  };
}
