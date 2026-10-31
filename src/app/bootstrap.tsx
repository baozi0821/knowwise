// M13 拥有:唯一前端组装根(bootstrap)。启动顺序(《M13》§2):
// 1. shell_get_status —— 数据库/设置失败 → 恢复页,不进入工作区;
// 2. 按依赖顺序初始化模块(记录结果,可选模块失败降级,核心失败恢复页);
// 3. 事件桥唯一订阅 domain-event(版本缺口/倒退 → 失效受影响查询);
// 4. 渲染工作区。所有持久用户资产由查询从 Rust 恢复,不读浏览器 storage。
import { Component, type ReactElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { AppError } from "../shared/generated/AppError";
import type { DomainEvent } from "../shared/generated/DomainEvent";
import type { ShellStatusView } from "../shared/generated/ShellStatusView";
import { ApiError, CommandClient, type IpcLogEntry } from "./api/client";
import { tauriInvoke, tauriInvokeStreamed, tauriListen } from "./ipc/transport";
import { EventBridge } from "./events/bridge";
import { installParserRuntime } from "./parserRuntime";
import { ModuleRegistry, type AppContext, type ModuleInitReport } from "./modules/registry";
import { buildModuleList } from "./modules";
import { AppServicesProvider, type AppServices } from "./services";
import { WorkspaceView } from "../modules/workspace/WorkspaceView";
import { RecoveryScreen } from "../modules/workspace/RecoveryScreen";
import { ScreenClipProvider } from "../modules/capture/screenClip";
import { initTheme, initAccent } from "./theme";
import { setStreamSpeed } from "../modules/qa/streamSpeed";
import type { ModelSettingsView } from "../shared/generated/ModelSettingsView";
import "./styles.css";
import "./theme.css";

export type BootstrapOptions = {
  /** 测试注入替身;生产默认 Tauri IPC。 */
  invoke?: typeof tauriInvoke;
  listen?: typeof tauriListen;
  timeoutMs?: number;
  onIpcLog?: (entry: IpcLogEntry) => void;
};

/** 渲染期异常兜底(组装根拥有):不吞错,显示最小恢复提示而非白屏。 */
class RenderBoundary extends Component<{ children: ReactNode }, { message: string | null }> {
  state: { message: string | null } = { message: null };

  static getDerivedStateFromError(error: unknown): { message: string } {
    return { message: error instanceof Error ? error.message : String(error) };
  }

  render() {
    if (this.state.message !== null) {
      return (
        <div role="alert" style={{ padding: "24px" }}>
          界面渲染失败:{this.state.message}。已保存数据不受影响,重启应用可重试。
        </div>
      );
    }
    return this.props.children;
  }
}

export type BootstrapOutcome = {
  registry: ModuleRegistry;
  bridge: EventBridge;
  queryClient: QueryClient;
  reports: ModuleInitReport[];
  /** 核心模块失败时非空:渲染恢复页而非工作区。 */
  fatal?: ApiError;
};

/**
 * 装配服务与模块(不渲染)。返回 outcome 供调用方决定渲染恢复页或工作区。
 * 事件失效:桥的 onVersionGap 与模块 invalidators 都汇入同一 invalidate 函数。
 */
export async function assemble(options: BootstrapOptions = {}): Promise<{
  outcome: BootstrapOutcome;
  services: AppServices;
}> {
  const client = new CommandClient(options.invoke ?? tauriInvoke, {
    timeoutMs: options.timeoutMs,
    onLog: options.onIpcLog,
    // 只有真实 Tauri 环境提供 Channel;测试注入的 invoke 走无事件回退。
    invokeStreamed: options.invoke === undefined ? tauriInvokeStreamed : undefined,
  });
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: {
        retry: 1,
        staleTime: 5_000,
        refetchOnWindowFocus: false,
        // 默认 5 分钟就回收缓存:切走再切回同一文档会重新读字节/重新解码。
        // 桌面本地应用没有网络成本,保留更久可显著减少切文档的重复工作。
        gcTime: 30 * 60_000,
      },
    },
  });
  // 输出速度:启动时读一次设置(失败不阻塞启动,默认标准)。
  void client
    .call<ModelSettingsView>("settings_get", {})
    .then((view) => setStreamSpeed(view.settings.streamSpeed))
    .catch(() => undefined);

  const registry = new ModuleRegistry();

  const invalidate = (keys: readonly unknown[][]) => {
    for (const key of keys) {
      void queryClient.invalidateQueries({ queryKey: key });
    }
  };

  const bridge = new EventBridge({
    onVersionGap: (event: DomainEvent) => {
      // 事件序号缺口/倒退:丢弃局部事件缓存语义 → 失效该事件影响面,
      // 由查询重新拉取真相(《02》§4)。
      for (const module of registry.list()) {
        for (const invalidator of module.commandInvalidators ?? []) {
          if (invalidator.eventTypes.includes(event.type)) {
            invalidate(invalidator.affectedKeys(event));
          }
        }
      }
    },
  });

  for (const module of buildModuleList()) {
    registry.add(module);
  }

  const context: AppContext = {
    eventBridge: bridge,
    registerArtifactRenderer: (registration) => registry.registerRenderer(registration),
  };
  const reports = await registry.initializeAll(context);

  // 模块事件入口与 invalidators 登记进桥(模块为单位去重)。
  for (const module of registry.list()) {
    if (module.eventTypes && module.eventTypes.length > 0 && module.onEvent) {
      bridge.register({
        moduleId: module.id,
        eventTypes: module.eventTypes,
        handle: (event) => module.onEvent?.(event),
      });
    }
  }

  // 解析 Worker:订阅 parser.request,驱动字节读取/解析/批次提交。
  installParserRuntime(bridge, client);

  const failedCore = reports.find((r) => r.core && r.status === "failed");
  const outcome: BootstrapOutcome = {
    registry,
    bridge,
    queryClient,
    reports,
    fatal: failedCore?.error
      ? new ApiError("bootstrap", `mod-${failedCore.id}`, failedCore.error)
      : undefined,
  };
  const services: AppServices = { client, bridge, registry };
  return { outcome, services };
}

/** 启动:先问壳层状态;失败进入恢复页,成功则进入工作区并订阅事件。 */
export async function bootstrap(element: HTMLElement, options: BootstrapOptions = {}) {
  // 主题先于 React 应用:避免浅色闪屏;偏好存 localStorage,不依赖后端。
  initTheme();
  initAccent();
  const { outcome, services } = await assemble(options);

  const render = (tree: ReactElement) => {
    createRoot(element).render(
      <QueryClientProvider client={outcome.queryClient}>
        <AppServicesProvider services={services}>
          {/* 主题在 React 之外应用到 <html>(启动前已执行一次),这里只挂载
              全局原生能力(屏幕框选截图)的宿主。 */}
          <ScreenClipProvider>
            <RenderBoundary>{tree}</RenderBoundary>
          </ScreenClipProvider>
        </AppServicesProvider>
      </QueryClientProvider>,
    );
  };

  let shellFailed = false;
  // 保留后端 failed 状态的错误信封(DB_CORRUPT/迁移失败等),恢复页要按码给指引,
  // 不能一律合成 IPC_TRANSPORT 的通用文案。
  let shellError: AppError | null = null;
  try {
    const status = await services.client.call<{
      state: "booting" | "ready" | "failed";
      error?: unknown;
    }>("shell_get_status");
    shellFailed = status.state === "failed";
    shellError = (status.error as AppError | undefined) ?? null;
  } catch {
    // 壳层状态不可得(如 IPC 未就绪):渲染恢复页,不进入工作区写状态。
    shellFailed = true;
  }

  if (shellFailed || outcome.fatal) {
    const error: AppError = outcome.fatal?.appError ??
      shellError ?? {
        schemaVersion: "error.v1",
        code: "IPC_TRANSPORT",
        messageKey: "error.ipc_transport",
        retryable: true,
        userAction: "retry",
        details: { reason: "shell_status_unavailable" },
        traceId: "bootstrap",
      };
    const status: ShellStatusView = {
      schemaVersion: "shell.status.v1",
      state: "failed",
      error,
    };
    render(<RecoveryScreen status={status} onRetry={() => window.location.reload()} />);
    return outcome;
  }

  await outcome.bridge.start(options.listen ?? tauriListen);
  const moduleFailures = outcome.reports
    .filter((r) => r.status === "failed" && !r.core)
    .map((r) => r.id);
  render(<WorkspaceView moduleFailures={moduleFailures} />);
  return outcome;
}
