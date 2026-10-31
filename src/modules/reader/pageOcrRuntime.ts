// 前端增量 OCR 的运行登记：让进度面板能看到“非 parse 任务”的自动/手动
// 页级识别，并提供停止入口。页状态本身仍以后端 page_ocr 为唯一真相。
import { create } from "zustand";

type OcrProgress = {
  pageNumber: number;
  status: string;
  done: number;
  total: number;
};

type OcrRuntimeState = {
  runs: Record<string, boolean>;
  progress: Record<string, OcrProgress>;
  setRunning: (key: string, running: boolean) => void;
  setProgress: (key: string, progress: OcrProgress | null) => void;
};

export const useOcrRuntime = create<OcrRuntimeState>((set) => ({
  runs: {},
  progress: {},
  setRunning: (key, running) =>
    set((state) => {
      if ((state.runs[key] ?? false) === running) return state;
      const runs = { ...state.runs };
      if (running) runs[key] = true;
      else delete runs[key];
      return { runs };
    }),
  setProgress: (key, progress) =>
    set((state) => {
      const next = { ...state.progress };
      if (progress === null) delete next[key];
      else next[key] = progress;
      return { progress: next };
    }),
}));

const stops = new Map<string, () => void>();

export function ocrRunKey(documentId: string, versionId: string): string {
  return `${documentId}:${versionId}`;
}

/** 登记一次增量 OCR；返回清理函数（结束时必须调用）。 */
export function registerOcrRun(
  documentId: string,
  versionId: string,
  stop: () => void,
): () => void {
  const key = ocrRunKey(documentId, versionId);
  stops.set(key, stop);
  useOcrRuntime.getState().setRunning(key, true);
  return () => {
    if (stops.get(key) === stop) stops.delete(key);
    useOcrRuntime.getState().setRunning(key, false);
    useOcrRuntime.getState().setProgress(key, null);
  };
}

/** 更新当前增量 OCR 的真实页级进度（来自 runPdfOcr 回调）。 */
export function updateOcrProgress(
  documentId: string,
  versionId: string,
  progress: OcrProgress,
): void {
  useOcrRuntime.getState().setProgress(ocrRunKey(documentId, versionId), progress);
}

/** 请求停止当前增量 OCR；返回是否确有运行中的任务。 */
export function cancelOcrRun(documentId: string, versionId: string): boolean {
  const stop = stops.get(ocrRunKey(documentId, versionId));
  if (stop === undefined) return false;
  stop();
  return true;
}

export function isOcrRunActive(documentId: string, versionId: string): boolean {
  return useOcrRuntime.getState().runs[ocrRunKey(documentId, versionId)] === true;
}
