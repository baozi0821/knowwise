// 流式正文的平滑呈现:后端按块(几十到几百字)到达,直接拼进 DOM 会
// 一跳一跳地"喷一大段"。这里把目标文本当作水位,按稳定速率逐字放出,
// 积压过多时加速追赶、终态后快速收尾——只改变"何时显示已有的字",
// 不改变内容、顺序,也不伪造进度。
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import {
  STREAM_SPEED_RATES,
  getStreamSpeed,
  subscribeStreamSpeed,
} from "./streamSpeed";

const TICK_MS = 40;
/** 积压超过阈值时允许的追赶速度,避免长答案永远追不上模型。 */
const CATCHUP_AFTER = 600;

function commonPrefix(a: string, b: string): number {
  const max = Math.min(a.length, b.length);
  let i = 0;
  while (i < max && a.charCodeAt(i) === b.charCodeAt(i)) i += 1;
  // 不要把代理对劈开。
  if (i > 0 && i < b.length) {
    const code = b.charCodeAt(i - 1);
    if (code >= 0xd800 && code <= 0xdbff) i -= 1;
  }
  return i;
}

/**
 * 平滑展示 `target`;返回当前应显示的文本。
 * `finished` 为真时以更快速度补完(仍保持连续,不瞬间跳变)。
 */
export function useSmoothText(
  target: string,
  finished: boolean,
  paused = false,
): string {
  const speed = useSyncExternalStore(subscribeStreamSpeed, getStreamSpeed, getStreamSpeed);
  const [displayed, setDisplayed] = useState("");
  const shownRef = useRef("");
  const targetRef = useRef(target);
  const timerRef = useRef(0);
  const finishedRef = useRef(finished);
  const speedRef = useRef(speed);
  targetRef.current = target;
  finishedRef.current = finished;
  speedRef.current = speed;

  useEffect(() => {
    // 目标被重置或缩短(重新生成/answer.reset):回退到公共前缀重新流出。
    if (!target.startsWith(shownRef.current)) {
      const keep = target.slice(0, commonPrefix(shownRef.current, target));
      shownRef.current = keep;
      setDisplayed(keep);
    }
    if (paused) {
      if (timerRef.current !== 0) {
        window.clearInterval(timerRef.current);
        timerRef.current = 0;
      }
      return;
    }
    if (shownRef.current.length >= target.length) return;
    if (timerRef.current !== 0) return;
    timerRef.current = window.setInterval(() => {
      const goal = targetRef.current;
      const shown = shownRef.current;
      if (shown.length >= goal.length) {
        window.clearInterval(timerRef.current);
        timerRef.current = 0;
        return;
      }
      const backlog = goal.length - shown.length;
      const activeRates = STREAM_SPEED_RATES[speedRef.current];
      const cps = finishedRef.current
        ? activeRates.finish
        : backlog > CATCHUP_AFTER
          ? activeRates.catchup
          : activeRates.base;
      const step = Math.max(1, Math.round((cps * TICK_MS) / 1000));
      const next = goal.slice(0, shown.length + step);
      shownRef.current = next;
      setDisplayed(next);
    }, TICK_MS);
  }, [target, speed, paused]);

  useEffect(
    () => () => {
      if (timerRef.current !== 0) window.clearInterval(timerRef.current);
      timerRef.current = 0;
    },
    [],
  );

  return displayed;
}
