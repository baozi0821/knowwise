// M13 拥有:统一命令客户端(《M13》§5)。集中处理 requestId 生成、超时、
// 取消、离线提示与 AppError 归一化;组件不得自行拼 invoke。
//
// 事实声明:超时/取消只停止前端等待,不伪造服务端状态——后台任务以
// TaskView 为真相,超时后仍由任务查询如实呈现(《M13》§7)。

import type { AppError } from "../../shared/generated/AppError";
import type { InvokeFn, StreamInvokeFn } from "../ipc/transport";
import { synthAppError } from "./errors";

export const DEFAULT_TIMEOUT_MS = 30_000;

export class ApiError extends Error {
  readonly appError: AppError;
  readonly command: string;
  readonly requestId: string;

  constructor(command: string, requestId: string, appError: AppError) {
    super(`[${appError.code}] ${command} (${requestId})`);
    this.name = "ApiError";
    this.appError = appError;
    this.command = command;
    this.requestId = requestId;
  }
}

/** 结构化 IPC 日志(白名单字段;不含请求正文与提示词)。 */
export type IpcLogEntry = {
  command: string;
  requestId: string;
  ok: boolean;
  errorCode?: string;
  durationMs: number;
};

export type CallOptions = {
  timeoutMs?: number;
  /** 取消只终止前端等待;幂等键保证服务端不重复写入。 */
  signal?: AbortSignal;
};

function randomId(prefix: string): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === "function") {
    return c.randomUUID();
  }
  return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * 归一化错误:Tauri 侧 Result Err 会序列化为 AppError 对象;字符串或
 * 其他形态(状态未托管、IPC 断开)包装为稳定信封,绝不把原始文本
 * 当作用户可见事实。
 */
export function toAppError(raw: unknown): AppError {
  // ApiError 把信封放在 appError 上;不拆开会被误判成 IPC_TRANSPORT,
  // 冲突等业务错误就显示成“与本地服务通信失败”。
  if (raw instanceof ApiError) return raw.appError;
  if (typeof raw === "object" && raw !== null) {
    const candidate = raw as Partial<AppError> & { details?: unknown };
    if (
      candidate.schemaVersion === "error.v1" &&
      typeof candidate.code === "string" &&
      typeof candidate.messageKey === "string"
    ) {
      return {
        schemaVersion: "error.v1",
        code: candidate.code,
        messageKey: candidate.messageKey,
        retryable: candidate.retryable === true,
        userAction: candidate.userAction ?? "none",
        details:
          candidate.details && typeof candidate.details === "object"
            ? (candidate.details as AppError["details"])
            : null,
        traceId: typeof candidate.traceId === "string" ? candidate.traceId : "unknown",
      };
    }
  }
  const hint =
    typeof raw === "string" && raw.length <= 120
      ? raw
      : typeof raw === "object" && raw !== null
        ? Object.prototype.toString.call(raw)
        : "unknown";
  return synthAppError("IPC_TRANSPORT", hint);
}

export class CommandClient {
  private readonly invoke: InvokeFn;
  private readonly invokeStreamed: StreamInvokeFn;
  /** 是否具备真实 Tauri Channel(测试/浏览器回退时为 false)。 */
  readonly supportsStreaming: boolean;
  private readonly timeoutMs: number;
  private readonly onLog: (entry: IpcLogEntry) => void;

  constructor(
    invoke: InvokeFn,
    options: {
      timeoutMs?: number;
      onLog?: (entry: IpcLogEntry) => void;
      /** 测试/纯浏览器环境未注入时退回普通 invoke(不产生流事件)。 */
      invokeStreamed?: StreamInvokeFn;
    } = {},
  ) {
    this.invoke = invoke;
    this.supportsStreaming = options.invokeStreamed !== undefined;
    this.invokeStreamed =
      options.invokeStreamed ??
      ((command, request, onEvent) =>
        this.invoke(command, { request, onEvent }));
    this.timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    this.onLog = options.onLog ?? (() => {});
  }

  newRequestId(): string {
    return randomId("req");
  }

  /** 幂等键:变更类命令必须携带(《02》§1)。 */
  newIdempotencyKey(): string {
    return randomId("idem");
  }

  /**
   * 平铺参数命令(artifacts/quiz 等里程碑命令直接取字段,无 request 封装)。
   * 仅限后端命令签名无 `request` 形参的命令;新命令一律走 call。
   */
  async callFlat<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
    const requestId = this.newRequestId();
    const startedAt = Date.now();
    try {
      const raw = await this.invoke(command, args);
      this.onLog({ command, requestId, ok: true, durationMs: Date.now() - startedAt });
      return raw as T;
    } catch (error) {
      const appError = toAppError(error);
      this.onLog({
        command,
        requestId,
        ok: false,
        errorCode: appError.code,
        durationMs: Date.now() - startedAt,
      });
      throw new ApiError(command, requestId, appError);
    }
  }

  /**
   * 带 Channel 的命令调用:事件持续到达,Promise 在服务端返回 TaskView 时结束。
   * 超时不套在事件流上——任务真相由消息/任务轮询确认。
   */
  async callStreamed<T>(
    command: string,
    request: object | undefined,
    onEvent: (event: unknown) => void,
  ): Promise<T> {
    const requestId = this.newRequestId();
    const startedAt = Date.now();
    try {
      const raw = await this.invokeStreamed(command, request, onEvent);
      this.onLog({ command, requestId, ok: true, durationMs: Date.now() - startedAt });
      return raw as T;
    } catch (error) {
      const appError = toAppError(error);
      this.onLog({
        command,
        requestId,
        ok: false,
        errorCode: appError.code,
        durationMs: Date.now() - startedAt,
      });
      throw new ApiError(command, requestId, appError);
    }
  }

  async call<T>(command: string, request?: object, options: CallOptions = {}): Promise<T> {    const requestId = this.newRequestId();
    const startedAt = Date.now();
    const timeoutMs = options.timeoutMs ?? this.timeoutMs;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timerPromise = new Promise<never>((_, reject) => {
      timer = setTimeout(() => {
        reject(new ApiError(command, requestId, synthAppError("IPC_TIMEOUT")));
      }, timeoutMs);
    });
    const signalPromise = options.signal
      ? new Promise<never>((_, reject) => {
          const onAbort = () => {
            reject(
              new ApiError(
                command,
                requestId,
                synthAppError("IPC_TIMEOUT", "aborted_by_user"),
              ),
            );
          };
          if (options.signal!.aborted) {
            onAbort();
          } else {
            options.signal!.addEventListener("abort", onAbort, { once: true });
          }
        })
      : null;

    try {
      const raw = await Promise.race([
        this.invoke(command, request === undefined ? undefined : { request }),
        timerPromise,
        ...(signalPromise ? [signalPromise] : []),
      ]);
      this.onLog({
        command,
        requestId,
        ok: true,
        durationMs: Date.now() - startedAt,
      });
      return raw as T;
    } catch (error) {
      const appError = toAppError(error instanceof ApiError ? error.appError : error);
      this.onLog({
        command,
        requestId,
        ok: false,
        errorCode: appError.code,
        durationMs: Date.now() - startedAt,
      });
      throw new ApiError(command, requestId, appError);
    } finally {
      if (timer !== undefined) clearTimeout(timer);
    }
  }
}
