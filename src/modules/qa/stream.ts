// 临时流状态(只用于当前运行中的展示;事实来源仍是数据库消息)。
// 事件来自 Tauri Channel(`stream_event.v1`),不落库、不进入导出。

export type StreamEventV1 = {
  schemaVersion: "stream_event.v1";
  seq: number;
  kind:
    | "run.started"
    | "stage.changed"
    | "reasoning.delta"
    | "answer.delta"
    | "answer.reset"
    | "notice"
    | "tool.started"
    | "tool.finished"
    | "heartbeat"
    | "run.finished"
    | "run.failed";
  messageId?: string;
  taskId?: string;
  stage?: string;
  text?: string;
  tool?: string;
  status?: string;
  summary?: string;
  elapsedMs?: number;
  errorCode?: string;
  visibleMessage?: string;
};

export type StreamTool = {
  id: string;
  tool: string;
  status: string;
  summary?: string;
};

export type StreamState = {
  /** running = 事件流仍在进行;finished/failed 后保留最后一次内容。 */
  status: "idle" | "running" | "finished" | "failed";
  messageId: string | null;
  taskId?: string;
  mode?: string;
  stage?: string;
  reasoning: string;
  answer: string;
  tools: StreamTool[];
  /** 看门狗的真实通知(不是模型推理)。 */
  notice?: string;
  errorCode?: string;
  visibleMessage?: string;
  startedAt: number;
  lastEventAt: number;
  /** 首条可见模型答案时间;用于 5s 口径与调试。 */
  firstVisibleAt?: number;
  /** 阶段耗时:只记录真实 stage.changed 事件的到达时间,不推测。 */
  stages?: Array<{ stage: string; startedAt: number; endedAt?: number }>;
  /** 真实 answer.delta 事件计数:用于区分逐块流式与"完成时一次性返回"。 */
  answerDeltas: number;
};

export const EMPTY_STREAM: StreamState = {
  status: "idle",
  messageId: null,
  reasoning: "",
  answer: "",
  tools: [],
  startedAt: 0,
  lastEventAt: 0,
  answerDeltas: 0,
};

export const STAGE_LABELS: Record<string, string> = {
  resolving_scope: "解析提问范围",
  preparing: "准备上下文",
  retrieving: "检索文档",
  planning: "制定研究计划",
  generating: "生成回答",
  tool: "调用工具",
  validating: "校验引用",
  finalizing: "整理回答",
};

export function stageLabel(stage: string | undefined): string | null {
  if (!stage) return null;
  return STAGE_LABELS[stage] ?? stage;
}

function closeStages(
  stages: StreamState["stages"],
  now: number,
): StreamState["stages"] {
  if (stages === undefined || stages.length === 0) return stages;
  const last = stages[stages.length - 1]!;
  if (last.endedAt !== undefined) return stages;
  return [...stages.slice(0, -1), { ...last, endedAt: now }];
}

export function reduceStream(state: StreamState, event: StreamEventV1): StreamState {
  const base: StreamState = {
    ...state,
    lastEventAt: Date.now(),
    taskId: event.taskId ?? state.taskId,
  };
  switch (event.kind) {
    case "run.started":
      return {
        ...base,
        status: "running",
        messageId: event.messageId ?? state.messageId,
        mode: event.status ?? state.mode,
        startedAt: state.status === "running" && state.startedAt > 0 ? state.startedAt : Date.now(),
        reasoning: state.status === "running" ? state.reasoning : "",
        answer: state.status === "running" ? state.answer : "",
        tools: [],
        answerDeltas: state.status === "running" ? state.answerDeltas : 0,
        notice: undefined,
        errorCode: undefined,
        visibleMessage: undefined,
        firstVisibleAt: undefined,
        stages:
          event.stage !== undefined
            ? [{ stage: event.stage, startedAt: Date.now() }]
            : [],
      };
    case "stage.changed": {
      if (event.stage === undefined) return { ...base, status: "running" };
      const now = Date.now();
      const closed = closeStages(state.stages ?? [], now) ?? [];
      return {
        ...base,
        status: "running",
        stage: event.stage,
        stages: [...closed, { stage: event.stage, startedAt: now }],
      };
    }
    case "reasoning.delta":
      return {
        ...base,
        status: "running",
        reasoning: state.reasoning + (event.text ?? ""),
      };
    case "answer.delta": {
      const text = event.text ?? "";
      return {
        ...base,
        status: "running",
        answer: state.answer + text,
        answerDeltas: state.answerDeltas + 1,
        firstVisibleAt:
          state.firstVisibleAt ?? (text.length > 0 ? Date.now() : undefined),
      };
    }
    case "answer.reset":
      // 结构化解析与最终答案不一致:丢弃临时草稿,最终以数据库答案为准。
      return { ...base, status: "running", answer: "", firstVisibleAt: undefined };
    case "notice":
      return { ...base, status: "running", notice: event.visibleMessage ?? state.notice };
    case "tool.started": {
      const id = event.text ?? event.tool ?? `tool-${event.seq}`;
      const tool: StreamTool = {
        id,
        tool: event.tool ?? "工具",
        status: "running",
        summary: event.summary,
      };
      const existing = state.tools.findIndex((item) => item.id === id);
      const tools =
        existing >= 0
          ? state.tools.map((item, index) => (index === existing ? tool : item))
          : [...state.tools, tool];
      return { ...base, status: "running", tools };
    }
    case "tool.finished": {
      const id = event.text ?? event.tool ?? "";
      const tools = state.tools.map((item) =>
        item.id === id
          ? {
              ...item,
              status: event.status ?? "finished",
              summary: event.summary ?? item.summary,
            }
          : item,
      );
      return { ...base, status: "running", tools };
    }
    case "heartbeat":
      return { ...base, status: state.status === "idle" ? "running" : state.status };
    case "run.finished":
      return { ...base, status: "finished", stages: closeStages(state.stages, Date.now()) };
    case "run.failed":
      return {
        ...base,
        status: "failed",
        errorCode: event.errorCode,
        visibleMessage: event.visibleMessage,
        stages: closeStages(state.stages, Date.now()),
      };
    default:
      return base;
  }
}

/** 依据真实事件判断当前展示文案;没有事件时不编造“思考内容”。 */
export function streamHeadline(state: StreamState): string | null {
  if (state.status === "failed") {
    return state.visibleMessage ?? "回答未完成。";
  }
  return stageLabel(state.stage);
}
