import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  FileText,
  MessageSquare,
  Search,
  Trash2,
  X,
  ExternalLink,
  Clock,
  Download,
  Pin,
  PinOff,
  GitBranch,
} from "lucide-react";
import type { ConversationView } from "../../shared/generated/ConversationView";
import { useServices } from "../../app/services";
import { ConversationExportDialog } from "../context/ConversationExport";
import { useQaStore } from "../qa/qaStore";
import { useWorkspaceUi } from "./uiStore";

type DocSummary = {
  id: string;
  displayName: string;
  format: string;
  currentVersionId: string | null;
};

export function AllConversationsDialog({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const currentDocId = useWorkspaceUi((s) => s.currentDocumentId);
  const openDocument = useWorkspaceUi((s) => s.openDocument);
  const currentConvId = useQaStore((s) => s.currentConversationId);
  const setConversation = useQaStore((s) => s.setConversation);

  const [search, setSearch] = useState("");
  const [exportConvId, setExportConvId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);

  const allConversations = useQuery({
    queryKey: ["conversation", "list", "all"],
    queryFn: () =>
      client.call<{ items: ConversationView[] }>("conversation_list", {
        documentId: "",
        limit: 200,
      }),
    enabled: open,
  });

  const docs = useQuery({
    queryKey: ["document", "list", "all-for-convs"],
    queryFn: () =>
      client.call<{ items: DocSummary[] }>("document_list_recent", {
        limit: 100,
      }),
    enabled: open,
  });

  const docMap = useMemo(() => {
    const map = new Map<string, DocSummary>();
    for (const d of docs.data?.items ?? []) {
      map.set(d.id, d);
    }
    return map;
  }, [docs.data]);

  // Group by document
  const grouped = useMemo(() => {
    const items = allConversations.data?.items ?? [];
    const query = search.trim().toLowerCase();
    const map = new Map<string, ConversationView[]>();

    for (const conv of items) {
      const doc = docMap.get(conv.documentId);
      const docTitle = doc?.displayName ?? "未知文档";
      const matchesSearch =
        query === "" ||
        (conv.title && conv.title.toLowerCase().includes(query)) ||
        docTitle.toLowerCase().includes(query);

      if (!matchesSearch) continue;

      const key = conv.documentId || "global";
      const list = map.get(key) ?? [];
      list.push(conv);
      map.set(key, list);
    }
    for (const list of map.values()) {
      list.sort((a, b) => {
        if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
        return b.updatedAt - a.updatedAt;
      });
    }
    return map;
  }, [allConversations.data, docMap, search]);

  if (!open) return null;

  async function handleDelete(conv: ConversationView, e: React.MouseEvent) {
    e.stopPropagation();
    if (!window.confirm(`确定删除会话「${conv.title || "未命名会话"}」吗？`)) return;
    setDeletingId(conv.id);
    try {
      await client.call("conversation_delete", { conversationId: conv.id });
      if (conv.id === currentConvId) {
        setConversation(null, conv.documentId);
      }
      await queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
    } catch {
      // ignore
    } finally {
      setDeletingId(null);
    }
  }

  async function handlePin(conv: ConversationView, e: React.MouseEvent) {
    e.stopPropagation();
    try {
      await client.call("conversation_pin", {
        conversationId: conv.id,
        pinned: !conv.pinned,
      });
      await queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
    } catch {
      // 置顶失败不清空列表;下次刷新仍可重试。
    }
  }

  async function handleBranch(conv: ConversationView, e: React.MouseEvent) {
    e.stopPropagation();
    try {
      const branched = await client.call<ConversationView>("conversation_branch", {
        conversationId: conv.id,
        idempotencyKey: client.newIdempotencyKey(),
      });
      await queryClient.invalidateQueries({ queryKey: ["conversation", "list"] });
      handleSelect(branched);
    } catch {
      // 分支失败保留原会话与列表。
    }
  }

  function handleSelect(conv: ConversationView) {
    if (conv.documentId && conv.documentId !== currentDocId) {
      const doc = docMap.get(conv.documentId);
      openDocument(conv.documentId, doc?.currentVersionId ?? null);
    }
    setConversation(conv.id, conv.documentId);
    onClose();
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-xs p-4"
      role="dialog"
      aria-modal="true"
    >
      <div className="bg-white dark:bg-slate-900 border border-slate-200 dark:border-slate-800 rounded-xl shadow-2xl w-full max-w-2xl max-h-[85vh] flex flex-col overflow-hidden animate-in fade-in zoom-in-95 duration-150">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-100 dark:border-slate-800">
          <div className="flex items-center gap-2">
            <MessageSquare className="w-5 h-5 text-purple-600 dark:text-purple-400" />
            <h2 className="text-base font-semibold text-slate-800 dark:text-slate-100">
              全文档会话管理中心
            </h2>
            <span className="text-xs bg-purple-50 dark:bg-purple-950/60 text-purple-600 dark:text-purple-400 px-2 py-0.5 rounded-full font-medium">
              共 {allConversations.data?.items?.length ?? 0} 个对话
            </span>
          </div>
          <button
            type="button"
            className="p-1 rounded-lg text-slate-400 hover:text-slate-600 dark:hover:text-slate-200 hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors"
            onClick={onClose}
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Search */}
        <div className="p-3 border-b border-slate-100 dark:border-slate-800 bg-slate-50/50 dark:bg-slate-900/50">
          <div className="relative">
            <Search className="w-4 h-4 text-slate-400 absolute left-3 top-2.5" />
            <input
              type="text"
              className="w-full pl-9 pr-4 py-1.5 text-xs rounded-lg border border-slate-200 dark:border-slate-700 bg-white dark:bg-slate-800 text-slate-900 dark:text-slate-100 placeholder-slate-400 focus:outline-none focus:ring-2 focus:ring-purple-500/20 focus:border-purple-500"
              placeholder="搜索会话名称或关联文档..."
              value={search}
              onChange={(e) => setSearch(e.target.value)}
            />
          </div>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4 space-y-5">
          {grouped.size === 0 ? (
            <div className="text-center py-12 text-slate-400">
              <MessageSquare className="w-8 h-8 mx-auto mb-2 opacity-30" />
              <p className="text-xs">暂无符合条件的会话记录</p>
            </div>
          ) : (
            Array.from(grouped.entries()).map(([docId, convs]) => {
              const doc = docMap.get(docId);
              const docTitle = doc?.displayName ?? (docId === "global" ? "全局独立会话" : "未命名文档");
              const isCurrentDoc = docId === currentDocId;

              return (
                <div key={docId} className="space-y-2">
                  <div className="flex items-center gap-2 text-xs font-semibold text-slate-700 dark:text-slate-300">
                    <FileText className="w-3.5 h-3.5 text-purple-500" />
                    <span>{docTitle}</span>
                    {isCurrentDoc ? (
                      <span className="text-[10px] bg-emerald-50 dark:bg-emerald-950/60 text-emerald-600 dark:text-emerald-400 px-1.5 py-0.2 rounded font-normal">
                        当前阅读中
                      </span>
                    ) : null}
                    <span className="text-[11px] text-slate-400 font-normal ml-auto">
                      {convs.length} 个对话
                    </span>
                  </div>

                  <div className="grid grid-cols-1 sm:grid-cols-2 gap-2 pl-2">
                    {convs.map((conv) => {
                      const isCurrentConv = conv.id === currentConvId;
                      return (
                        <div
                          key={conv.id}
                          className={`group p-3 rounded-lg border transition-all cursor-pointer relative flex flex-col justify-between ${
                            isCurrentConv
                              ? "border-purple-400 bg-purple-50/40 dark:bg-purple-950/20 shadow-xs"
                              : "border-slate-200 dark:border-slate-800 hover:border-purple-300 dark:hover:border-purple-700 bg-white dark:bg-slate-800/60"
                          }`}
                          onClick={() => handleSelect(conv)}
                        >
                          <div>
                            <div className="flex items-start justify-between gap-2">
                              <h3 className="text-xs font-medium text-slate-800 dark:text-slate-200 line-clamp-1 group-hover:text-purple-600 dark:group-hover:text-purple-400 transition-colors">
                                {conv.pinned ? "置顶 · " : ""}
                                {conv.title || "未命名会话"}
                              </h3>
                              {isCurrentConv ? (
                                <span className="text-[9px] bg-purple-600 text-white px-1.5 py-0.2 rounded shrink-0">
                                  活跃
                                </span>
                              ) : null}
                            </div>
                            <div className="flex items-center gap-1.5 text-[10px] text-slate-400 mt-1">
                              <Clock className="w-3 h-3 opacity-70" />
                              <span>{new Date(conv.updatedAt).toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })}</span>
                            </div>
                          </div>

                          <div className="flex items-center justify-end gap-1.5 mt-2.5 pt-2 border-t border-slate-100 dark:border-slate-800/80">
                            <button
                              type="button"
                              className="p-1 rounded text-slate-400 hover:text-purple-600 dark:hover:text-purple-400 hover:bg-slate-100 dark:hover:bg-slate-700 transition-colors"
                              title={conv.pinned ? "取消置顶" : "置顶会话"}
                              aria-label={conv.pinned ? "取消置顶" : "置顶会话"}
                              onClick={(e) => void handlePin(conv, e)}
                            >
                              {conv.pinned ? (
                                <PinOff className="w-3 h-3" />
                              ) : (
                                <Pin className="w-3 h-3" />
                              )}
                            </button>
                            <button
                              type="button"
                              className="p-1 rounded text-slate-400 hover:text-purple-600 dark:hover:text-purple-400 hover:bg-slate-100 dark:hover:bg-slate-700 transition-colors"
                              title="分支为新会话(原会话不变)"
                              aria-label="分支会话"
                              onClick={(e) => void handleBranch(conv, e)}
                            >
                              <GitBranch className="w-3 h-3" />
                            </button>
                            <button
                              type="button"
                              className="p-1 rounded text-slate-400 hover:text-purple-600 dark:hover:text-purple-400 hover:bg-slate-100 dark:hover:bg-slate-700 transition-colors"
                              title="导出会话"
                              onClick={(e) => {
                                e.stopPropagation();
                                setExportConvId(conv.id);
                              }}
                            >
                              <Download className="w-3 h-3" />
                            </button>
                            <button
                              type="button"
                              className="p-1 rounded text-slate-400 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-950/40 transition-colors"
                              title="删除会话"
                              disabled={deletingId === conv.id}
                              onClick={(e) => void handleDelete(conv, e)}
                            >
                              <Trash2 className="w-3 h-3" />
                            </button>
                            <span className="text-[11px] text-purple-600 dark:text-purple-400 font-medium ml-1 flex items-center gap-0.5">
                              进入
                              <ExternalLink className="w-2.5 h-2.5" />
                            </span>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              );
            })
          )}
        </div>

        {/* Footer */}
        <div className="px-5 py-3 border-t border-slate-100 dark:border-slate-800 bg-slate-50/50 dark:bg-slate-900/50 flex items-center justify-between">
          <span className="text-xs text-slate-500">
            点击任意会话即可自动跳转至对应文档与聊天界面
          </span>
          <button
            type="button"
            className="px-3 py-1.5 rounded-lg border border-slate-200 dark:border-slate-700 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800 transition-colors"
            onClick={onClose}
          >
            关闭
          </button>
        </div>
      </div>

      {exportConvId !== null ? (
        <ConversationExportDialog
          conversationId={exportConvId}
          onClose={() => setExportConvId(null)}
        />
      ) : null}
    </div>
  );
}
