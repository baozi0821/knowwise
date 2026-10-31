// M13 拥有:应用入口。唯一职责是找到挂载点并调用组装根;
// 任何初始化都不得绕过 bootstrap.tsx(《M13》§1)。
import { bootstrap } from "./bootstrap";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("缺少 #root 挂载点");
}

void bootstrap(root).catch((error: unknown) => {
  // 组装级失败(不应发生):显示最小恢复提示,不吞错。
  root.replaceChildren();
  const message = document.createElement("div");
  message.setAttribute("role", "alert");
  message.style.padding = "24px";
  message.textContent = `应用未能启动:${
    error instanceof Error ? error.message : String(error)
  }。已保存数据不受影响,请重试。`;
  root.append(message);
});
