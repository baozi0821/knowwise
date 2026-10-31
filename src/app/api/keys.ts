// M13 拥有:查询键工厂。事件失效与组件查询共用同一键源,
// 跨文档切换时键域随 documentId 变化,旧任务不会投到新文档(《M13》§7)。

export const qk = {
  shellStatus: ["shell", "status"] as const,
  recentDocuments: (limit: number) => ["documents", "recent", limit] as const,
  documentDetail: (documentId: string) => ["document", documentId] as const,
  renderPlan: (documentId: string, versionId: string | null) =>
    ["reader", "render-plan", documentId, versionId] as const,
  parseStatus: (documentId: string) => ["parse", "status", documentId] as const,
  readerPosition: (documentId: string) => ["reader", "position", documentId] as const,
  tasks: (scope: { documentId: string | null; activeOnly: boolean }) =>
    ["tasks", scope.documentId, scope.activeOnly] as const,
  task: (taskId: string) => ["task", taskId] as const,
  agentSteps: (taskId: string) => ["agent-steps", taskId] as const,
  notes: (documentId: string, filter: string) => ["notes", documentId, filter] as const,
  artifacts: (documentId: string) => ["artifacts", documentId] as const,
  // M06 追加(只追加,不改既有键):笔记/标注/待澄清/书签按文档分域;
  // ["notes", documentId] 是 settings 面板 notes 键的前缀,变更后整体失效。
  documentNotes: (documentId: string) => ["notes", documentId] as const,
  documentAnnotations: (documentId: string) => ["annotations", documentId] as const,
  documentClarifications: (documentId: string) => ["clarifications", documentId] as const,
  documentBookmarks: (documentId: string) => ["bookmarks", documentId] as const,
} as const;
