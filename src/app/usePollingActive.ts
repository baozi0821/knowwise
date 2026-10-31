// 轮询节流:页面不可见(最小化/切到其它应用)或对应面板未显示时,
// 把高频轮询降为低频,而不是完全停止——后台任务仍要有可见终态,
// 收尾逻辑(自动切到资料页、错误提示)也依赖下一次真实轮询结果。
//
// 只做时序控制,不伪造任何状态:数据来源、终态判定都不变。

import { useEffect, useState } from "react";
import { useWorkspaceUi } from "../modules/workspace/uiStore";

/** 页面是否可见(隐藏标签页/最小化时返回 false)。 */
export function useDocumentVisible(): boolean {
  const [visible, setVisible] = useState(() =>
    typeof document === "undefined" ? true : document.visibilityState !== "hidden",
  );
  useEffect(() => {
    if (typeof document === "undefined") return;
    const sync = () => setVisible(document.visibilityState !== "hidden");
    sync();
    document.addEventListener("visibilitychange", sync);
    return () => document.removeEventListener("visibilitychange", sync);
  }, []);
  return visible;
}

/**
 * 右栏「AI 助手」面板此刻是否真的显示:右栏展开、非专注模式、且当前标签是 agent。
 * 面板用 `hidden` 属性隐藏但组件始终挂载,因此需要显式判断。
 */
export function useAgentPaneVisible(): boolean {
  return useWorkspaceUi(
    (s) => s.dockOpen && !s.focusMode && s.dockTab === "agent",
  );
}

/**
 * 轮询节流:前台显示时返回 `activeMs`,页面或面板不可见时返回 `idleMs`。
 * 返回 0 表示停止轮询(调用方自行判断终态)。
 */
export function usePollingGate(): (activeMs: number, idleMs: number) => number {
  const pageVisible = useDocumentVisible();
  const paneVisible = useAgentPaneVisible();
  const throttled = !pageVisible || !paneVisible;
  return (activeMs, idleMs) => (throttled ? idleMs : activeMs);
}
