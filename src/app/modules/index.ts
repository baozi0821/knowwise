// M13 拥有:前端模块清单与事件失效接线(《M13》§5/§6)。模块只声明
// 关注的事件与受影响的查询键;初始化不执行网络或模型调用。按《00》§4
// 依赖顺序排列:核心基础设施在前,可选视觉模块在后。

import type { FrontendModule } from "./registry";

export function buildModuleList(): FrontendModule[] {
  return [
    {
      id: "m13.workspace",
      core: true,
      // 壳层自身:UI 状态已在 store;无额外初始化。
    },
    {
      id: "m02.documents",
      core: true,
      eventTypes: ["document.changed"],
      onEvent: () => {
        // 具体失效经 commandInvalidators 统一声明,这里保留模块事件入口
        // 供 M02 落地自有视图时扩展。
      },
      commandInvalidators: [
        {
          eventTypes: ["document.changed"],
          affectedKeys: () => [["documents"]],
        },
      ],
    },
    {
      id: "m03.reader",
      core: true,
      commandInvalidators: [
        {
          // ocr.progress 只影响页 OCR 状态与解析覆盖,不影响 PDF 渲染本身:
          // 失效整个 ["reader"] 会让阅读区重取渲染计划(解析中易触发 10.9.1 竞态,
          // 表现为识别后正文/页角徽标短暂消失)。因此 OCR 事件只失效状态查询。
          eventTypes: ["document.changed", "parse.progress"],
          affectedKeys: (event) => {
            const keys: string[][] = [];
            if (event.documentId) keys.push(["parse", "status", event.documentId], ["reader"]);
            if (event.documentVersionId) keys.push(["page-ocr", event.documentVersionId]);
            return keys.length > 0 ? keys : [["reader"]];
          },
        },
        {
          eventTypes: ["ocr.progress"],
          affectedKeys: (event) => {
            const keys: string[][] = [];
            if (event.documentId) keys.push(["parse", "status", event.documentId]);
            if (event.documentVersionId) keys.push(["page-ocr", event.documentVersionId]);
            return keys.length > 0 ? keys : [["page-ocr"]];
          },
        },
      ],
    },
    {
      id: "m04.parse-index",
      core: true,
      commandInvalidators: [
        {
          eventTypes: ["parse.progress", "ocr.progress", "task.changed"],
          affectedKeys: (event) => {
            const keys: string[][] = [];
            if (event.documentId) keys.push(["parse", "status", event.documentId]);
            if (event.documentVersionId) keys.push(["page-ocr", event.documentVersionId]);
            return keys.length > 0 ? keys : [["parse"]];
          },
        },
      ],
    },
    {
      id: "m01.tasks",
      core: true,
      commandInvalidators: [
        {
          eventTypes: ["task.changed"],
          affectedKeys: () => [["tasks"]],
        },
      ],
    },
    {
      id: "m06.notes",
      core: true,
      commandInvalidators: [
        {
          eventTypes: ["userdata.changed"],
          affectedKeys: (event) =>
            event.documentId ? [["notes", event.documentId]] : [["notes"]],
        },
      ],
    },
    {
      id: "m09.qa",
      core: true,
      // ai.delta 不进入失效:流式片段只是进度提示,答案以 TaskView 为真相
      // (《02》§4);task.changed 已由 m01.tasks 覆盖任务查询。
      // P3A:写工具确认事件到达后立即重取步骤(同时保留 Agent 面板轮询兜底)。
      eventTypes: [],
      commandInvalidators: [
        {
          eventTypes: ["agent.confirmation"],
          affectedKeys: () => [["agent-steps"]],
        },
      ],
    },
    {
      id: "m10.artifacts",
      core: false,
      commandInvalidators: [
        {
          eventTypes: ["artifact.changed"],
          affectedKeys: (event) =>
            event.documentId ? [["artifacts", event.documentId]] : [["artifacts"]],
        },
      ],
    },
    {
      id: "m11.quiz",
      core: false,
      commandInvalidators: [
        {
          eventTypes: ["artifact.changed"],
          affectedKeys: (event) =>
            event.documentId ? [["artifacts", event.documentId]] : [["artifacts"]],
        },
      ],
    },
    {
      id: "m12.graph",
      core: false,
      commandInvalidators: [
        {
          eventTypes: ["artifact.changed"],
          affectedKeys: (event) =>
            event.documentId ? [["artifacts", event.documentId]] : [["artifacts"]],
        },
      ],
    },
    {
      id: "m08.model-privacy",
      core: false,
      // M08 落地后接入 model.changed/privacy.changed 与设置视图;
      // 当前仅占位注册,保持初始化顺序稳定。
      commandInvalidators: [
        {
          eventTypes: ["model.changed", "privacy.changed"],
          affectedKeys: () => [["model", "status"]],
        },
      ],
    },
  ];
}

/** 查询键前缀必须来自 qk 工厂或其父前缀;此函数供契约测试使用。 */
export function knownKeyPrefixes(): readonly string[] {
  return [
    "shell",
    "documents",
    "document",
    "reader",
    "parse",
    "page-ocr",
    "tasks",
    "task",
    "agent-steps",
    "notes",
    "artifacts",
    "model",
  ];
}
