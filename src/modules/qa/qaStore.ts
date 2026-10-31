// M09 拥有:问答面板 UI 状态。会话选择跨面板开关保持(组件卸载不丢)。
import { create } from "zustand";

type QaStore = {
  currentConversationId: string | null;
  documentConversations: Record<string, string | null>;
  setConversation: (id: string | null, documentId?: string | null) => void;
  getConversationForDocument: (documentId: string) => string | null;
};

export const useQaStore = create<QaStore>((set, get) => ({
  currentConversationId: null,
  documentConversations: {},
  setConversation: (id, documentId) => {
    set((state) => ({
      currentConversationId: id,
      documentConversations: documentId
        ? { ...state.documentConversations, [documentId]: id }
        : state.documentConversations,
    }));
  },
  // 未记录过的文档不得继承上一篇文档的会话(跨文档消息/快照会被后端拒绝)。
  getConversationForDocument: (documentId) => {
    return get().documentConversations[documentId] ?? null;
  },
}));
