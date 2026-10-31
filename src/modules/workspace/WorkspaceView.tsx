// M13 拥有:桌面工作区组装视图(《M13》§4)。顶部功能栏 + 文档标签栏 +
// 左侧文档库 + 右侧全高辅助面板(Dock:AI 助手 / 笔记 双标签,可拖宽);
// 专注模式隐藏右栏与抽屉但保留快捷键与草稿。窄窗口(≤1100px)右栏仍
// 以覆盖方式呈现,阅读区为主区域。拖拽文件到窗口即导入。
import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { ParseStatusView } from "../../shared/generated/ParseStatusView";
import type { ShellStatusView } from "../../shared/generated/ShellStatusView";
import type { ModelStatusView } from "../../shared/generated/ModelStatusView";
import type { TaskView } from "../../shared/generated/TaskView";
import type { MessageView } from "../../shared/generated/MessageView";
import type { DocumentDetail } from "../../shared/generated/DocumentDetail";
import { useServices } from "../../app/services";
import { qk } from "../../app/api/keys";
import { dialogPickDocument } from "../../app/ipc/transport";
import { applyTheme, loadTheme, resolveTheme, saveTheme, toggleTheme, type ThemeMode } from "../../app/theme";
import {
  applyAccent,
  loadAccent,
  saveAccent,
  type AccentPreset,
} from "../../app/theme";
import { AgentRegion } from "./AgentRegion";
import { NotesDock } from "../notes/NotesDock";
import { ArtifactsPanel } from "../artifacts/ArtifactsPanel";
import { DocumentSidebar } from "./DocumentSidebar";
import { ReaderRegion } from "./ReaderRegion";
import { ArtifactReadingPane } from "../artifacts/ArtifactReadingPane";
import { StatusBar } from "./StatusBar";
import { TabStrip } from "./TabStrip";
import { PanelResizer } from "./PanelResizer";
import { CompareSplitter } from "./CompareSplitter";
import { TopBar } from "./TopBar";
import { ShortcutHelp } from "./ShortcutHelp";
import { indexSummary, type ModelState } from "./statusText";
import { isTextInput, matchShortcut } from "./shortcuts";
import { useWorkspaceUi } from "./uiStore";
import {
  SettingsDialog,
  normalizeSettingsSection,
  type SettingsSection,
} from "../model-settings/SettingsDialog";

/** 支持即拖即开的文档扩展名(与导入侧能力一致)。 */
const DROP_EXTENSIONS = [".pdf", ".md", ".markdown", ".docx"] as const;

function isSupportedDropPath(path: string): boolean {
  const lower = path.toLowerCase();
  return DROP_EXTENSIONS.some((ext) => lower.endsWith(ext));
}

/** 在线状态订阅(SEC-01:离线时阅读/搜索/笔记可用,AI 明确不可用)。 */
function useOnline(): boolean {
  const [online, setOnline] = useState(() =>
    typeof navigator === "undefined" ? true : navigator.onLine,
  );
  useEffect(() => {
    const goOnline = () => setOnline(true);
    const goOffline = () => setOnline(false);
    window.addEventListener("online", goOnline);
    window.addEventListener("offline", goOffline);
    return () => {
      window.removeEventListener("online", goOnline);
      window.removeEventListener("offline", goOffline);
    };
  }, []);
  return online;
}

export function WorkspaceView({ moduleFailures }: { moduleFailures: string[] }) {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const documentId = useWorkspaceUi((s) => s.currentDocumentId);
  const versionId = useWorkspaceUi((s) => s.currentVersionId);
  const dockOpen = useWorkspaceUi((s) => s.dockOpen);
  const dockTab = useWorkspaceUi((s) => s.dockTab);
  const setDockTab = useWorkspaceUi((s) => s.setDockTab);
  const toggleDockTab = useWorkspaceUi((s) => s.toggleDockTab);
  const setDockOpen = useWorkspaceUi((s) => s.setDockOpen);
  const sidebarWidth = useWorkspaceUi((s) => s.sidebarWidth);
  const setSidebarWidth = useWorkspaceUi((s) => s.setSidebarWidth);
  const dockWidth = useWorkspaceUi((s) => s.dockWidth);
  const setDockWidth = useWorkspaceUi((s) => s.setDockWidth);
  const sidebarOpen = useWorkspaceUi((s) => s.sidebarOpen);
  const setSidebarOpen = useWorkspaceUi((s) => s.setSidebarOpen);
  const focusMode = useWorkspaceUi((s) => s.focusMode);
  const openDocument = useWorkspaceUi((s) => s.openDocument);
  const closeDocument = useWorkspaceUi((s) => s.closeDocument);
  const cycleTab = useWorkspaceUi((s) => s.cycleTab);
  const setFocusMode = useWorkspaceUi((s) => s.setFocusMode);
  const artifactReading = useWorkspaceUi((s) => s.artifactReading);
  const compareTarget = useWorkspaceUi((s) => s.compareTarget);
  const setCompareTarget = useWorkspaceUi((s) => s.setCompareTarget);
  const compareRatio = useWorkspaceUi((s) => s.compareRatio);
  const setCompareRatio = useWorkspaceUi((s) => s.setCompareRatio);
  const compareRef = useRef<HTMLDivElement>(null);
  // 右栏面板懒挂载:首次切到才挂载,之后保留(切回不重建、查询缓存不丢)。
  // 三个面板同时挂载时,切换文档会让 12+ 个查询一起激活(每篇文档都重发),
  // 未看过的面板不该为当前阅读付这笔开销。
  const [mountedDockTabs, setMountedDockTabs] = useState<ReadonlySet<string>>(
    () => new Set([dockTab]),
  );
  useEffect(() => {
    setMountedDockTabs((previous) =>
      previous.has(dockTab) ? previous : new Set([...previous, dockTab]),
    );
  }, [dockTab]);
  const [settingsSection, setSettingsSection] = useState<SettingsSection | null>(null);
  const [shortcutHelpOpen, setShortcutHelpOpen] = useState(false);
  const [themeMode, setThemeMode] = useState<ThemeMode>(() => loadTheme());
  const [accentPreset, setAccentPreset] = useState<AccentPreset>(() => loadAccent());
  const [notice, setNotice] = useState<string | null>(null);
  const online = useOnline();

  // 主题:状态即真相;应用 + 落盘(跟随系统时监听系统变化)。
  useEffect(() => {
    applyTheme(themeMode);
    saveTheme(themeMode);
  }, [themeMode]);

  // 强调色:顶栏与设置面板都可能改;状态变化即应用并落盘。
  useEffect(() => {
    applyAccent(accentPreset);
    saveAccent(accentPreset);
  }, [accentPreset]);
  useEffect(() => {
    const onAccent = (event: Event) => {
      const detail = (event as CustomEvent<AccentPreset>).detail;
      if (detail !== undefined) setAccentPreset(detail);
      else setAccentPreset(loadAccent());
    };
    window.addEventListener("knowwise-accent-changed", onAccent);
    return () => window.removeEventListener("knowwise-accent-changed", onAccent);
  }, []);
  useEffect(() => {
    if (globalThis.matchMedia === undefined) return;
    const query = globalThis.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => {
      if (loadTheme() === "system") applyTheme("system");
    };
    query.addEventListener?.("change", onChange);
    return () => query.removeEventListener?.("change", onChange);
  }, []);
  useEffect(() => {
    const onOpen = (event: Event) => {
      // 事件可能先于资料面板挂载到达:统一转存到 uiStore,由面板挂载后消费。
      const detail = (
        event as CustomEvent<{
          artifactId?: string;
          artifactVersionId?: string;
          read?: boolean;
        }>
      ).detail;
      useWorkspaceUi.getState().setPendingArtifactOpen({
        artifactId: detail?.artifactId ?? null,
        versionId: detail?.artifactVersionId ?? null,
        read: detail?.read === true,
      });
      setDockTab("artifacts");
    };
    window.addEventListener("knowwise-open-artifacts", onOpen);
    return () => window.removeEventListener("knowwise-open-artifacts", onOpen);
  }, [setDockTab]);
  // 任务中心「查看结果」:定位消息所在文档/会话,再交给对话区滚动高亮。
  useEffect(() => {
    const onOpenMessage = (event: Event) => {
      const detail = (event as CustomEvent<{ messageId?: string }>).detail;
      const messageId = detail?.messageId;
      if (!messageId) return;
      void (async () => {
        try {
          const message = await client.call<MessageView>("message_get", { messageId });
          const current = useWorkspaceUi.getState();
          if (current.currentDocumentId !== message.documentId) {
            const tab = current.tabs.find((item) => item.documentId === message.documentId);
            const document = await client.call<DocumentDetail>("document_get", {
              documentId: message.documentId,
            });
            openDocument(
              message.documentId,
              tab?.versionId ?? document.currentVersionId ?? null,
            );
          }
          setDockTab("agent");
          window.dispatchEvent(
            new CustomEvent("knowwise-reveal-message", {
              detail: {
                messageId,
                conversationId: message.conversationId,
                documentId: message.documentId,
              },
            }),
          );
        } catch {
          setNotice("无法定位该结果消息;它可能已被删除,或文档当前不可用。");
        }
      })();
    };
    window.addEventListener("knowwise-open-message", onOpenMessage);
    return () => window.removeEventListener("knowwise-open-message", onOpenMessage);
  }, [client, openDocument, setDockTab]);
  useEffect(() => {
    const onOpenSettings = (event: Event) => {
      const detail = (event as CustomEvent<string | undefined>).detail;
      setSettingsSection(normalizeSettingsSection(detail));
    };
    window.addEventListener("knowwise-open-settings", onOpenSettings);
    return () => window.removeEventListener("knowwise-open-settings", onOpenSettings);
  }, []);

  const resolvedTheme = resolveTheme(themeMode);

  // 启动恢复(《M13》§2):仅首屏自动打开最近文档一次;
  // 用户主动关闭文档不触发重开,阅读位置由 M03 挂载点恢复。
  const autoRestoreDone = useRef(false);
  const recentForRestore = useQuery({
    queryKey: qk.recentDocuments(1),
    queryFn: () =>
      client.call<{ items: Array<{ id: string; currentVersionId: string | null }> }>(
        "document_list_recent",
        { limit: 1 },
      ),
    enabled: !autoRestoreDone.current && documentId === null,
    staleTime: 5_000,
  });
  useEffect(() => {
    if (autoRestoreDone.current) return;
    const last = recentForRestore.data?.items?.[0];
    if (last) {
      autoRestoreDone.current = true;
      openDocument(last.id, last.currentVersionId);
    }
  }, [recentForRestore.data, openDocument]);

  const parseStatus = useQuery({
    queryKey: documentId ? qk.parseStatus(documentId) : ["parse", "none"],
    queryFn: () => client.call<ParseStatusView>("parse_get_status", { documentId }),
    enabled: documentId !== null,
    refetchInterval: (query) =>
      query.state.data?.parseState === "in_progress" ? 1_500 : false,
  });
  // 从未解析过的文档打开即补一次 parse_start(parser.request 由解析 Worker 消费)。
  // 失败/中断/取消的文档不自动重跑全文识别:扫描件可由「识别指定页」按需识别,
  // 避免每次打开都重启整本 OCR(用户明确要求不等全文)。
  const parseAutoStart = useRef<string | null>(null);
  useEffect(() => {
    if (documentId === null || versionId === null || parseStatus.data === undefined) return;
    if (parseStatus.data.parseState !== "not_started") return;
    const key = `${documentId}:${versionId}`;
    if (parseAutoStart.current === key) return;
    parseAutoStart.current = key;
    void client
      .call("parse_start", {
        documentId,
        versionId,
        reason: "auto_open",
        idempotencyKey: client.newIdempotencyKey(),
      })
      .then(() => queryClient.invalidateQueries({ queryKey: qk.parseStatus(documentId) }))
      .catch(() => {
        // 解析启动失败由文档状态/任务中心呈现;不阻塞阅读。
      });
  }, [client, documentId, parseStatus.data, queryClient, versionId]);
  const activeTasks = useQuery({
    queryKey: qk.tasks({ documentId: null, activeOnly: true }),
    queryFn: () =>
      client.call<{ items: TaskView[] }>("task_list", { activeOnly: true, limit: 50 }),
    refetchInterval: 10_000,
  });
  const shellStatus = useQuery({
    queryKey: qk.shellStatus,
    queryFn: () => client.call<ShellStatusView>("shell_get_status"),
  });
  // M08 接线:模型能力如实呈现(就绪/不可用/未知),不阻塞阅读(《M13》§2)。
  const modelStatus = useQuery({
    queryKey: ["model", "status"],
    queryFn: () => client.call<ModelStatusView>("model_get_status", {}),
  });
  const modelState: ModelState =
    modelStatus.data === undefined
      ? "unknown"
      : modelStatus.data.capability === "ready"
        ? "ready"
        : modelStatus.data.capability === "unavailable" ||
            modelStatus.data.capability === "misconfigured"
          ? "unavailable"
          : "ready";
  const activeCount = (activeTasks.data?.items ?? []).length;

  const importPath = useCallback(
    async (path: string) => {
      const result = await client.call<{ documentId: string; versionId: string }>(
        "document_import",
        {
          path,
          source: "picker",
          idempotencyKey: client.newIdempotencyKey(),
        },
      );
      openDocument(result.documentId, result.versionId);
    },
    [client, openDocument],
  );

  const handleOpenFromDialog = useCallback(async () => {
    try {
      const path = await dialogPickDocument();
      if (path === null) return;
      await importPath(path);
    } catch {
      // 导入失败已在侧栏入口呈现完整反馈;快捷键路径安静失败,
      // 不改变任何已保存状态,下次打开仍可重试。
    }
  }, [importPath]);

  // 拖拽导入:从资源管理器/浏览器把文件拖进窗口即开(Tauri 原生拖放事件;
  // 浏览器环境没有该 API,静默降级)。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let canceled = false;
    void (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const stop = await getCurrentWebview().onDragDropEvent((event) => {
          if (event.payload.type !== "drop") return;
          const paths = event.payload.paths.filter(isSupportedDropPath);
          if (paths.length === 0) {
            setNotice("只支持拖入 PDF / Markdown / DOCX 文件");
            return;
          }
          void (async () => {
            try {
              await importPath(paths[0]!);
              setNotice(paths.length > 1 ? `已打开 ${paths[0]} 等 ${paths.length} 个文件` : null);
            } catch {
              setNotice("拖入的文件未能导入;可点「导入文档」重试");
            }
          })();
        });
        if (canceled) stop();
        else unlisten = stop;
      } catch {
        // 浏览器/测试环境:无原生拖放。
      }
    })();
    return () => {
      canceled = true;
      unlisten?.();
    };
  }, [importPath]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      const action = matchShortcut(event, { inTextInput: isTextInput(event.target) });
      switch (action) {
        case "open-document":
          event.preventDefault();
          void handleOpenFromDialog();
          break;
        case "focus-search":
          event.preventDefault();
          // Acrobat/Typora 习惯:Ctrl+F 是文档内搜索。
          window.dispatchEvent(new CustomEvent("knowwise-open-search"));
          break;
        case "toggle-focus-mode":
          event.preventDefault();
          setFocusMode(!useWorkspaceUi.getState().focusMode);
          break;
        case "toggle-agent":
          event.preventDefault();
          toggleDockTab("agent");
          break;
        case "toggle-notes":
          event.preventDefault();
          toggleDockTab("notes");
          break;
        case "toggle-shortcut-help":
          event.preventDefault();
          setShortcutHelpOpen(true);
          break;
        case "toggle-sidebar":
          event.preventDefault();
          setSidebarOpen(!useWorkspaceUi.getState().sidebarOpen);
          break;
        case "next-tab":
          event.preventDefault();
          cycleTab(1);
          break;
        case "prev-tab":
          event.preventDefault();
          cycleTab(-1);
          break;
        case "close-tab":
          event.preventDefault();
          closeDocument();
          break;
        case "go-reader":
          event.preventDefault();
          document.getElementById("main-content")?.focus();
          break;
        case "go-agent-input":
          event.preventDefault();
          setDockTab("agent");
          requestAnimationFrame(() => document.getElementById("agent-input")?.focus());
          break;
        case "go-status":
          event.preventDefault();
          document.getElementById("status-bar")?.focus();
          break;
        default:
          break;
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    handleOpenFromDialog,
    setFocusMode,
    setSidebarOpen,
    toggleDockTab,
    setDockTab,
    cycleTab,
    closeDocument,
  ]);

  const showDock = dockOpen && !focusMode;

  return (
    <div className={`app-shell${focusMode ? " focus-mode" : ""}`}>
      <a className="skip-link" href="#main-content">
        跳转到主内容(阅读器)
      </a>
      <a className="skip-link" href="#agent-input">
        跳转到 Agent 输入
      </a>
      <a className="skip-link" href="#status-bar">
        跳转到状态区
      </a>

      <TopBar
        focusMode={focusMode}
        dockOpen={dockOpen}
        dockTab={dockTab}
        resolvedTheme={resolvedTheme}
        themeMode={themeMode}
        accentPreset={accentPreset}
        modelState={modelState}
        onSelectDockTab={(tab) => toggleDockTab(tab)}
        onToggleFocusMode={() => setFocusMode(!focusMode)}
        onToggleTheme={() => setThemeMode(toggleTheme(themeMode))}
        onSelectAccent={setAccentPreset}
        onOpenShortcuts={() => setShortcutHelpOpen(true)}
        onOpenSettings={() => setSettingsSection("models")}
        onOpenArtifacts={() => setDockTab("artifacts")}
      />

      {moduleFailures.length > 0 ? (
        <p className="module-degraded" role="status">
          部分模块未加载({moduleFailures.join("、")}),对应功能暂不可用。
        </p>
      ) : null}
      {shellStatus.data?.state === "failed" ? (
        <p className="module-degraded" role="alert">
          应用处于恢复模式,业务数据未写入。
        </p>
      ) : null}

      {!focusMode ? (
        <TabStrip onOpenDocument={() => void handleOpenFromDialog()} />
      ) : null}

      <div className="workspace-body">
        {!focusMode && sidebarOpen ? (
          <>
            <DocumentSidebar
              currentDocumentId={documentId}
              onOpenDocument={openDocument}
              pickDocument={dialogPickDocument}
              width={sidebarWidth}
              onClose={() => setSidebarOpen(false)}
            />
            <PanelResizer
              side="left"
              onResize={setSidebarWidth}
              label="调整文档库宽度"
              value={sidebarWidth}
              min={180}
              max={420}
              onReset={() => setSidebarWidth(240)}
            />
          </>
        ) : !focusMode ? (
          <button
            type="button"
            className="sidebar-collapsed-rail"
            title="展开文档库 (Ctrl+B)"
            aria-label="展开文档库"
            onClick={() => setSidebarOpen(true)}
          >
            ›
          </button>
        ) : null}
        <main
          id="main-content"
          className="main-region"
          tabIndex={-1}
          aria-label={artifactReading ? "资料阅览区" : "阅读区"}
        >
          {artifactReading ? (
            <ArtifactReadingPane reading={artifactReading} />
          ) : compareTarget !== null && compareTarget.documentId !== documentId ? (
            <div
              className="reader-compare"
              aria-label="双文档对照"
              ref={compareRef}
              style={{ "--compare-ratio": String(compareRatio) } as CSSProperties}
            >
              <section className="reader-compare-pane" aria-label="左侧文档">
                <ReaderRegion documentId={documentId} versionId={versionId} />
              </section>
              <CompareSplitter
                containerRef={compareRef}
                ratio={compareRatio}
                onChange={setCompareRatio}
                onReset={() => setCompareRatio(0.5)}
              />
              <section className="reader-compare-pane" aria-label="右侧文档">
                <button
                  type="button"
                  className="reader-compare-exit"
                  onClick={() => setCompareTarget(null)}
                  aria-label="退出双文档对照"
                >
                  退出对照
                </button>
                <ReaderRegion
                  documentId={compareTarget.documentId}
                  versionId={compareTarget.versionId}
                />
              </section>
            </div>
          ) : (
            <ReaderRegion documentId={documentId} versionId={versionId} />
          )}
        </main>
        {showDock ? (
          <>
            <PanelResizer
              side="right"
              onResize={setDockWidth}
              label="调整右侧面板宽度"
              value={dockWidth}
              min={300}
              max={900}
              onReset={() => setDockWidth(420)}
            />
            <aside
              className="dock-panel"
              aria-label="右侧面板(AI 助手、笔记与资料)"
              style={{ width: dockWidth }}
            >
              <div className="dock-head" role="tablist" aria-label="右侧面板标签">
                <button
                  type="button"
                  role="tab"
                  id="dock-tab-agent"
                  aria-selected={dockTab === "agent"}
                  aria-controls="dock-panel-agent"
                  className={`dock-tab${dockTab === "agent" ? " active" : ""}`}
                  onClick={() => setDockTab("agent")}
                >
                  AI 助手
                </button>
                <button
                  type="button"
                  role="tab"
                  id="dock-tab-notes"
                  aria-selected={dockTab === "notes"}
                  aria-controls="dock-panel-notes"
                  className={`dock-tab${dockTab === "notes" ? " active" : ""}`}
                  onClick={() => setDockTab("notes")}
                >
                  笔记
                </button>
                <button
                  type="button"
                  role="tab"
                  id="dock-tab-artifacts"
                  aria-selected={dockTab === "artifacts"}
                  aria-controls="dock-panel-artifacts"
                  className={`dock-tab${dockTab === "artifacts" ? " active" : ""}`}
                  onClick={() => setDockTab("artifacts")}
                >
                  资料
                </button>
                <span className="dock-head-spacer" />
                <button
                  type="button"
                  className="icon-btn small"
                  aria-label="收起右侧面板"
                  title="收起右侧面板"
                  onClick={() => setDockOpen(false)}
                >
                  »
                </button>
              </div>
              <div
                className="dock-body"
                id="dock-panel-agent"
                role="tabpanel"
                aria-labelledby="dock-tab-agent"
                hidden={dockTab !== "agent"}
              >
                {mountedDockTabs.has("agent") ? <AgentRegion active={dockTab === "agent"} /> : null}
              </div>
              <div
                className="dock-body"
                id="dock-panel-notes"
                role="tabpanel"
                aria-labelledby="dock-tab-notes"
                hidden={dockTab !== "notes"}
              >
                {mountedDockTabs.has("notes") ? (
                  <NotesDock
                    documentId={documentId}
                    versionId={versionId}
                    active={dockTab === "notes"}
                  />
                ) : null}
              </div>
              <div
                className="dock-body dock-body-artifacts"
                id="dock-panel-artifacts"
                role="tabpanel"
                aria-labelledby="dock-tab-artifacts"
                hidden={dockTab !== "artifacts"}
              >
                {mountedDockTabs.has("artifacts") ? (
                  documentId === null ? (
                    <p className="notes-hint center">先打开文档,再查看本文档产生的资料。</p>
                  ) : (
                    <ArtifactsPanel
                      documentId={documentId}
                      versionId={versionId}
                      isDialog
                      active={dockTab === "artifacts"}
                    />
                  )
                ) : null}
              </div>
            </aside>
          </>
        ) : !focusMode ? (
          <button
            type="button"
            className="dock-collapsed-rail"
            title="展开右侧面板 (Alt+Shift+A)"
            aria-label="展开右侧面板"
            onClick={() => setDockOpen(true)}
          >
            ‹
          </button>
        ) : null}
      </div>

      {notice !== null ? (
        <p className="workspace-toast" role="status" onAnimationEnd={() => setNotice(null)}>
          {notice}
        </p>
      ) : null}

      <StatusBar
        saveState="idle"
        indexText={indexSummary(parseStatus.data)}
        modelState={modelState}
        online={online}
        activeTaskCount={activeCount}
      />

      {settingsSection !== null ? (
        <SettingsDialog
          initialSection={settingsSection ?? "models"}
          onClose={() => setSettingsSection(null)}
        />
      ) : null}
      {shortcutHelpOpen ? <ShortcutHelp onClose={() => setShortcutHelpOpen(false)} /> : null}
    </div>
  );
}
