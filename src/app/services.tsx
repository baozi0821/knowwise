// M13 拥有:组装后的服务上下文。组件经 useServices 取端口,
// 不直接 import Tauri API 或自建客户端(《M13》§5)。

import { createContext, useContext, type ReactNode } from "react";
import type { CommandClient } from "./api/client";
import type { EventBridge } from "./events/bridge";
import type { ModuleRegistry } from "./modules/registry";

export type AppServices = {
  client: CommandClient;
  bridge: EventBridge;
  registry: ModuleRegistry;
};

export const AppServicesContext = createContext<AppServices | null>(null);

export function AppServicesProvider({
  services,
  children,
}: {
  services: AppServices;
  children: ReactNode;
}) {
  return (
    <AppServicesContext.Provider value={services}>{children}</AppServicesContext.Provider>
  );
}

export function useServices(): AppServices {
  const services = useContext(AppServicesContext);
  if (!services) {
    throw new Error("useServices 必须在 AppServicesProvider 内使用");
  }
  return services;
}
