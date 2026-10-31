// M03 阅读位置:防抖保存与恢复降级链(《M03》§7)。
// 退出/隐藏/滚动停止后 800–1000ms 防抖保存;切换文档前立即 flush;
// 只有命令成功返回后才进入 saved 状态(不伪装已保存)。
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import type { RenderBlock } from "./types";

export type PositionSaveStatus = "idle" | "pending" | "saving" | "saved" | "failed";

export type PositionSaverInput = {
  documentId: string;
  versionId: string;
  locator: SourceLocator;
  /** 辅助恢复:标题路径与锚点文本哈希。 */
  headingPath?: string[];
  anchorTextHash?: string;
};

export type PositionSaver = {
  /** 滚动/位置变化通知;重置防抖计时。 */
  notify(input: PositionSaverInput): void;
  /** 切换文档/退出前立即保存未落盘的位置。 */
  flush(): Promise<void>;
  /** 放弃未保存位置(文档关闭失败等)。 */
  discard(): void;
  status(): PositionSaveStatus;
};

export const POSITION_DEBOUNCE_MS = 900;

export function createPositionSaver(
  save: (input: PositionSaverInput) => Promise<unknown>,
  opts: { debounceMs?: number } = {},
): PositionSaver {
  const debounceMs = opts.debounceMs ?? POSITION_DEBOUNCE_MS;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pendingInput: PositionSaverInput | null = null;
  let state: PositionSaveStatus = "idle";

  const doSave = async (input: PositionSaverInput) => {
    state = "saving";
    try {
      await save(input);
      state = "saved";
    } catch {
      state = "failed";
    }
  };

  return {
    notify(input) {
      pendingInput = input;
      state = "pending";
      if (timer !== null) clearTimeout(timer);
      // 防抖窗口 800–1000ms:连续滚动只保留最后一次位置。
      timer = setTimeout(() => {
        timer = null;
        if (pendingInput) {
          const snapshot = pendingInput;
          pendingInput = null;
          void doSave(snapshot);
        }
      }, debounceMs);
    },
    async flush() {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      if (pendingInput) {
        const snapshot = pendingInput;
        pendingInput = null;
        await doSave(snapshot);
      }
    },
    discard() {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      pendingInput = null;
      state = "idle";
    },
    status() {
      return state;
    },
  };
}

export type RestoreOutcome = {
  block: RenderBlock | null;
  /** 降级原因:null=精确恢复;"heading"=标题路径;"text_hash"=文本哈希;"none"。 */
  degraded: null | "heading" | "text_hash" | "none";
};

/**
 * 恢复降级链(《M03》§7):精确 Locator → 文本哈希/相邻块候选 →
 * 最近有效标题。失败时回退并提示,不假装定位成功。
 */
export function restorePosition(
  blocks: RenderBlock[],
  saved: {
    locator: SourceLocator;
    headingPath?: string[];
    anchorTextHash?: string;
  },
): RestoreOutcome {
  // 1) 精确:markdown 偏移落在块内。
  if (saved.locator.kind === "markdown") {
    const exact = blocks.find(
      (b) =>
        saved.locator.kind === "markdown" &&
        b.locator.kind === "markdown" &&
        b.locator.startOffset <= saved.locator.startOffset &&
        b.locator.endOffset >= saved.locator.startOffset,
    );
    if (exact) return { block: exact, degraded: null };
  }
  if (saved.locator.kind === "docx") {
    const exact = blocks.find(
      (b) =>
        b.locator.kind === "docx" &&
        saved.locator.kind === "docx" &&
        b.locator.bodyBlockStart === saved.locator.bodyBlockStart,
    );
    if (exact) return { block: exact, degraded: null };
  }
  if (saved.locator.kind === "pdf") {
    // PDF 页级恢复由页面虚拟化处理;此处给不了块级就报降级。
    const firstPage = blocks.find(
      (b) => b.locator.kind === "pdf" && saved.locator.kind === "pdf",
    );
    if (firstPage) return { block: firstPage, degraded: "heading" };
  }
  // 2) 标题路径匹配(重复标题按序号区分)。
  if (saved.headingPath && saved.headingPath.length > 0) {
    const target = saved.headingPath.join("/");
    const byHeading = blocks.find(
      (b) => b.headingPath.join("/") === target,
    );
    if (byHeading) return { block: byHeading, degraded: "heading" };
  }
  // 3) 文本哈希(相邻块候选)。
  if (saved.anchorTextHash) {
    const byHash = blocks.find(
      (b) => hashText(b.text) === saved.anchorTextHash,
    );
    if (byHash) return { block: byHash, degraded: "text_hash" };
  }
  return { block: null, degraded: "none" };
}

// —— Markdown / DOCX 的块级位置(M03 §5):滚动时取视口顶部最近块 ——

/** 滚动位置候选块:几何 + 渲染文本偏移 + 标题路径(DOM 几何由视图采集)。 */
export type PositionBlockCandidate = {
  /** 块首字符在渲染文本中的偏移(UTF-16 code unit,与标注 offset 同源)。 */
  offset: number;
  /** 块末字符偏移(不含)。 */
  endOffset: number;
  /** 块所属标题路径(最外层在前)。 */
  headingPath: string[];
  /** 块相对滚动容器视口顶部的上沿(px,负数=已滚出上方)。 */
  top: number;
  /** 块相对滚动容器视口顶部的下沿(px)。 */
  bottom: number;
  /** 块级标签名(小写;写入 mdastNodeType 便于排障)。 */
  blockType: string;
};

/**
 * 视角顶部位置块:取与视口顶部线相交、或最靠近视口顶部的第一个块。
 * 纯函数(便于单测);blocks 顺序不敏感。
 */
export function choosePositionBlock(
  blocks: readonly PositionBlockCandidate[],
  viewportTop = 0,
): PositionBlockCandidate | null {
  if (blocks.length === 0) return null;
  const sorted = [...blocks].sort((a, b) => a.top - b.top);
  // 视口内最上方块:块的阅读区域尚未完全滚出顶部。
  const firstVisible = sorted.find((b) => b.bottom > viewportTop);
  if (firstVisible !== undefined) return firstVisible;
  // 已滚到文档末尾:保留最后一个块,避免恢复时回到文首。
  return sorted[sorted.length - 1] ?? null;
}

/** 候选块 → 持久化 locator(markdown 用字符偏移,docx 用正文块序)。 */
export function locatorFromPositionBlock(
  format: "markdown" | "docx",
  block: PositionBlockCandidate,
): SourceLocator {
  const startOffset = Math.max(0, block.offset);
  // 空块无法构造有效 Range;至少保留 1 个字符区间,恢复时再按标题降级。
  const endOffset = Math.max(startOffset + 1, block.endOffset);
  if (format === "markdown") {
    return {
      kind: "markdown",
      startOffset,
      endOffset,
      mdastNodeType: block.blockType || "paragraph",
      headingPath: block.headingPath,
      locatorVersion: 1,
    };
  }
  return {
    kind: "docx",
    bodyBlockStart: startOffset,
    bodyBlockEnd: endOffset,
    paragraphIds: [],
    headingPath: block.headingPath,
    locatorVersion: 1,
  };
}

/** 恢复目标:精确偏移 + 标题路径(格式不匹配 → null,不假装定位成功)。 */
export type RestoreTextTarget = {
  startOffset: number;
  endOffset: number;
  headingPath: string[];
};

/** 从已保存 locator 提取 MD/DOCX 恢复目标(恢复降级链的入口)。 */
export function restoreTargetFromLocator(
  locator: SourceLocator | null | undefined,
  format: "markdown" | "docx",
): RestoreTextTarget | null {
  if (locator === null || locator === undefined) return null;
  if (format === "markdown" && locator.kind === "markdown") {
    return {
      startOffset: locator.startOffset,
      endOffset: locator.endOffset,
      headingPath: locator.headingPath,
    };
  }
  if (format === "docx" && locator.kind === "docx") {
    return {
      startOffset: locator.bodyBlockStart,
      endOffset: locator.bodyBlockEnd,
      headingPath: locator.headingPath,
    };
  }
  return null;
}

/** 简易稳定哈希(恢复候选比对用,非密码学)。 */
export function hashText(text: string): string {
  let h1 = 0x811c9dc5;
  let h2 = 0x01000193;
  for (let i = 0; i < text.length; i += 1) {
    const c = text.charCodeAt(i);
    h1 = (h1 ^ c) * 16777619 >>> 0;
    h2 = (h2 + c * 31) >>> 0;
  }
  return `${h1.toString(16)}-${h2.toString(16)}`;
}
