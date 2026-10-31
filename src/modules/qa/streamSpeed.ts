// 流式正文呈现速度:只决定"何时显示已经到达的真实文本",不改变内容与生成。
// 后端设置是权威来源;localStorage 只做启动早期与设置弹窗之间的镜像。
export type StreamSpeed = "slow" | "standard" | "fast";

export const STREAM_SPEED_LABELS: Record<StreamSpeed, string> = {
  slow: "慢",
  standard: "标准",
  fast: "快",
};

type Rate = { base: number; catchup: number; finish: number };

export const STREAM_SPEED_RATES: Record<StreamSpeed, Rate> = {
  slow: { base: 18, catchup: 160, finish: 900 },
  standard: { base: 34, catchup: 240, finish: 1600 },
  fast: { base: 90, catchup: 520, finish: 2600 },
};

const KEY = "knowwise.stream.speed";
const EVENT = "knowwise-stream-speed-changed";
const listeners = new Set<() => void>();

export function normalizeStreamSpeed(value: unknown): StreamSpeed {
  return value === "slow" || value === "fast" || value === "standard" ? value : "standard";
}

let current: StreamSpeed = (() => {
  try {
    return normalizeStreamSpeed(globalThis.localStorage?.getItem(KEY));
  } catch {
    return "standard";
  }
})();

export function getStreamSpeed(): StreamSpeed {
  return current;
}

export function setStreamSpeed(value: unknown, persist = true): void {
  const next = normalizeStreamSpeed(value);
  if (persist) {
    try {
      globalThis.localStorage?.setItem(KEY, next);
    } catch {
      // 存储不可用只影响下次启动的镜像;后端设置仍在。
    }
  }
  if (next === current) return;
  current = next;
  for (const listener of listeners) listener();
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(EVENT, { detail: { speed: next } }));
  }
}

export function subscribeStreamSpeed(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}
