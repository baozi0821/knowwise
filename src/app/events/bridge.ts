// M13 拥有:事件桥(《M13》§5、《02》§4)。模块不得私注册 Tauri 监听器;
// 事件桥对 domain-event 通道做唯一订阅,按事件类型分发给模块注册的
// handler,并维护轻量 aggregateVersion 映射——发现倒退或缺口时丢弃局部
// 事件缓存并回调失效(查询是最终事实,事件只是通知)。

import type { DomainEvent } from "../../shared/generated/DomainEvent";
import type { DomainEventType } from "../../shared/generated/DomainEventType";

/** 与 Rust app_shell::shell::DOMAIN_EVENT_CHANNEL 保持一致。 */
export const DOMAIN_EVENT_CHANNEL = "domain-event";

export const DOMAIN_EVENT_TYPES: readonly DomainEventType[] = [
  "document.changed",
  "parse.progress",
  "ocr.progress",
  "task.changed",
  "ai.delta",
  "conversation.changed",
  "userdata.changed",
  "artifact.changed",
  "privacy.changed",
  "model.changed",
  "parser.request",
  "agent.confirmation",
];

export type EventDispatchHandler = (event: DomainEvent) => void;

export type EventHandlerRegistration = {
  moduleId: string;
  eventTypes: readonly DomainEventType[];
  handle: EventDispatchHandler;
};

export type SubscribeFn = (
  channel: string,
  handler: (payload: unknown) => void,
) => Promise<() => void>;

/** 信封校验:不合法载荷直接丢弃,不进入分发。 */
export function isDomainEvent(payload: unknown): payload is DomainEvent {
  if (typeof payload !== "object" || payload === null) return false;
  const candidate = payload as Partial<DomainEvent>;
  return (
    candidate.schemaVersion === "event.v1" &&
    typeof candidate.eventId === "string" &&
    DOMAIN_EVENT_TYPES.includes(candidate.type as DomainEventType) &&
    typeof candidate.occurredAt === "number" &&
    typeof candidate.aggregateId === "string" &&
    typeof candidate.aggregateVersion === "number"
  );
}

export type VersionVerdict = "first" | "in_order" | "stale" | "gap";

/**
 * 版本判定:aggregateVersion 连续递增视为有序;倒退(stale,乱序重放)
 * 与缺口(gap)都要求重查真相;首个事件直接接受。
 */
export function judgeVersion(last: number | undefined, next: number): VersionVerdict {
  if (last === undefined) return "first";
  if (next < last) return "stale";
  if (next === last) return "stale"; // 重复投递,等同倒退处理
  return next === last + 1 ? "in_order" : "gap";
}

export class EventBridge {
  private readonly registrations: EventHandlerRegistration[] = [];
  private readonly versions = new Map<string, number>();
  private readonly onVersionGap: (event: DomainEvent) => void;
  private unsubscribe: (() => void) | null = null;
  private starting: Promise<void> | null = null;

  constructor(options: { onVersionGap?: (event: DomainEvent) => void } = {}) {
    this.onVersionGap = options.onVersionGap ?? (() => {});
  }

  /** 模块注册(由组装根调用);同一模块重复注册抛错。
   *  不同模块关注同一事件类型是合法的(各自失效自己的查询)。 */
  register(registration: EventHandlerRegistration): void {
    if (registration.eventTypes.length === 0) {
      throw new Error(`模块 ${registration.moduleId} 注册了空事件类型列表`);
    }
    if (this.registrations.some((r) => r.moduleId === registration.moduleId)) {
      throw new Error(`模块 ${registration.moduleId} 重复注册事件处理器`);
    }
    this.registrations.push(registration);
  }

  /** 唯一订阅;重复 start 返回进行中的 Promise,不产生第二个监听器。 */
  async start(subscribe: SubscribeFn): Promise<void> {
    if (this.unsubscribe) return;
    this.starting ??= (async () => {
      this.unsubscribe = await subscribe(DOMAIN_EVENT_CHANNEL, (payload) => {
        this.receive(payload);
      });
    })();
    await this.starting;
  }

  stop(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.starting = null;
  }

  /** 供测试与桥内部使用:处理一个原始载荷。 */
  receive(payload: unknown): void {
    if (!isDomainEvent(payload)) return;
    this.dispatch(payload);
  }

  dispatch(event: DomainEvent): void {
    const last = this.versions.get(event.aggregateId);
    const verdict = judgeVersion(last, event.aggregateVersion);
    this.versions.set(event.aggregateId, event.aggregateVersion);
    if (verdict === "stale" || verdict === "gap") {
      // 丢弃局部事件缓存信号:通知失效回调重查;事件本身仍分发,
      // handler 端以查询结果为准,不得把事件当作事实覆盖已保存状态。
      this.onVersionGap(event);
    }
    for (const registration of this.registrations) {
      if (registration.eventTypes.includes(event.type)) {
        registration.handle(event);
      }
    }
  }

  /** 测试辅助:读取版本映射。 */
  versionOf(aggregateId: string): number | undefined {
    return this.versions.get(aggregateId);
  }

  registeredCount(): number {
    return this.registrations.length;
  }
}
