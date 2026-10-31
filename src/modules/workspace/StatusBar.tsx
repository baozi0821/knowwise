// M13 拥有:底部状态区(《M13》§4/§8)。保存、索引、离线、模型与后台
// 任务摘要;aria-live=polite 且只播报关键变化,不随流式 token 打扰。
// 颜色只作辅助,所有状态都有文本。
import {
  backgroundTaskSummary,
  indexSummary,
  modelStateText,
  offlineText,
  saveStateText,
  type ModelState,
  type SaveState,
} from "./statusText";

export function StatusBar({
  saveState,
  indexText,
  modelState,
  online,
  activeTaskCount,
}: {
  saveState: SaveState;
  indexText: { level: "info" | "warning"; text: string };
  modelState: ModelState;
  online: boolean;
  activeTaskCount: number;
}) {
  const save = saveStateText(saveState);
  const offline = offlineText(!online);
  const tasks = backgroundTaskSummary(activeTaskCount);
  const chunks = [save, indexText.text, modelStateText(modelState), offline, tasks].filter(
    (text) => text.length > 0,
  );
  return (
    <footer id="status-bar" className="status-bar" aria-label="应用状态" tabIndex={-1}>
      {/* live region 用内层 div(role=status 对 div 合法;footer 保持
          contentinfo 隐式角色,叠加 role 会触发 aria-allowed-role)。 */}
      <div role="status" aria-live="polite">
        {chunks.length === 0 ? (
          <span>就绪</span>
        ) : (
          chunks.map((text, i) => (
            <span
              key={i}
              className={`status-chunk status-${i === 1 ? indexText.level : "plain"}`}
            >
              {text}
              {i < chunks.length - 1 ? <span aria-hidden="true"> · </span> : null}
            </span>
          ))
        )}
      </div>
    </footer>
  );
}

/** 由查询侧聚合出状态区输入(纯函数,便于测试)。 */
export function statusBarInput({
  saveState,
  parseSummaryText,
  modelState,
  online,
  activeTaskCount,
}: {
  saveState: SaveState;
  parseSummaryText: { level: "info" | "warning"; text: string };
  modelState: ModelState;
  online: boolean;
  activeTaskCount: number;
}) {
  return {
    saveState,
    indexText: parseSummaryText,
    modelState,
    online,
    activeTaskCount,
  };
}

export { indexSummary };
