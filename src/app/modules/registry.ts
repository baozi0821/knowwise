// M13 拥有:前端模块注册表(《M13》§6)。模块以声明式接入:初始化顺序
// 由组装根固定;id、产物 Renderer kind 重复注册在接入时即失败;
// 可选模块失败记录降级,不让工作区白屏,核心模块失败进入恢复页。
// 禁止在 initialize 中执行网络或模型调用。

import type { ComponentType } from "react";
import type { AppError } from "../../shared/generated/AppError";
import type { DomainEvent } from "../../shared/generated/DomainEvent";
import type { DomainEventType } from "../../shared/generated/DomainEventType";
import type { EventBridge } from "../events/bridge";

/** Artifact Renderer 挂载契约(《M13》§4/§6;编辑器与导出能力可后补)。 */
export type ArtifactRendererProps = {
  artifactId: string;
  versionId: string;
};

export type ArtifactRendererRegistration = {
  kind: string;
  component: ComponentType<ArtifactRendererProps>;
  editor?: ComponentType<ArtifactRendererProps>;
  /** 导出格式 id 列表(由 M10–M12 的 exporter 决定)。 */
  exportFormats?: string[];
};

export type CommandInvalidator = {
  eventTypes: readonly DomainEventType[];
  /** 返回需要失效的查询键(扁平注入 queryClient.invalidateQueries)。 */
  affectedKeys: (event: DomainEvent) => readonly unknown[][];
};

/** 组装上下文:模块初始化只能拿到这些端口,不直接触碰 Tauri API。 */
export type AppContext = {
  eventBridge: EventBridge;
  registerArtifactRenderer: (registration: ArtifactRendererRegistration) => void;
};

export type FrontendModule = {
  id: string;
  /** 核心模块初始化失败 → 恢复页;可选模块失败 → 产物降级标记。 */
  core: boolean;
  initialize?: (context: AppContext) => Promise<void> | void;
  eventTypes?: readonly DomainEventType[];
  onEvent?: (event: DomainEvent) => void;
  commandInvalidators?: CommandInvalidator[];
  artifactRenderers?: ArtifactRendererRegistration[];
};

export type ModuleInitReport = {
  id: string;
  core: boolean;
  status: "ok" | "failed";
  error?: AppError;
};

export class ModuleRegistry {
  private readonly modules: FrontendModule[] = [];
  private readonly renderers = new Map<string, ArtifactRendererRegistration>();
  private readonly failedKinds = new Set<string>();

  add(module: FrontendModule): void {
    if (this.modules.some((m) => m.id === module.id)) {
      throw new Error(`模块 ${module.id} 重复注册`);
    }
    this.modules.push(module);
  }

  list(): readonly FrontendModule[] {
    return this.modules;
  }

  /** 按《00》§4 依赖顺序初始化:调用方按序 add,此处顺序执行并记录结果。 */
  async initializeAll(context: AppContext): Promise<ModuleInitReport[]> {
    const reports: ModuleInitReport[] = [];
    for (const module of this.modules) {
      try {
        // Renderer 先登记(重复 kind 立即失败);initialize 保持无副作用。
        for (const renderer of module.artifactRenderers ?? []) {
          context.registerArtifactRenderer(renderer);
        }
        await module.initialize?.(context);
        reports.push({ id: module.id, core: module.core, status: "ok" });
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        reports.push({
          id: module.id,
          core: module.core,
          status: "failed",
          error: {
            schemaVersion: "error.v1",
            code: "MODULE_INIT_FAILED",
            messageKey: "error.module_init_failed",
            retryable: false,
            userAction: "none",
            details: { reason: message },
            traceId: `mod-${module.id}`,
          },
        });
      }
    }
    return reports;
  }

  /** 未知 kind 的兜底由渲染层(artifactRenderers.tsx)处理,这里只查注册表。 */
  rendererFor(kind: string): ArtifactRendererRegistration | undefined {
    return this.renderers.get(kind);
  }

  registerRenderer(registration: ArtifactRendererRegistration): void {
    if (this.renderers.has(registration.kind)) {
      throw new Error(`产物 Renderer 重复注册:${registration.kind}`);
    }
    this.renderers.set(registration.kind, registration);
  }

  /** 可选视觉模块失败时标记其产物 kind 降级(不白屏,《M13》§6)。 */
  markKindDegraded(kind: string): void {
    this.failedKinds.add(kind);
  }

  degradedKinds(): readonly string[] {
    return [...this.failedKinds];
  }
}
