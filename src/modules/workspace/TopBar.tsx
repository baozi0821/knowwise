// 顶部功能栏:Logo + 应用名;右侧为功能选项(右栏标签、专注、主题、
// 快捷键帮助、隐私、设置)。右栏标签即「AI 助手 / 笔记」切换入口,
// 两个标签共享同一条可拖宽的右栏(整列全高)。
import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  MessageSquare,
  Eye,
  EyeOff,
  Settings,
  NotebookPen,
  Moon,
  Sun,
  Keyboard,
  Palette,
  Check,
  Sparkles,
  ListTodo,
} from "lucide-react";
import { useServices } from "../../app/services";
import { useQuery } from "@tanstack/react-query";
import { TaskCenterPanel } from "./TaskCenterPanel";
import type { TaskView } from "../../shared/generated/TaskView";
import type { DockTab, } from "./uiStore";
import type { AccentPreset, ThemeMode } from "../../app/theme";
import { ACCENT_PRESETS } from "../../app/theme";

function KnowwiseMark(): ReactNode {
  return (
    <svg className="app-mark" viewBox="0 0 32 32" aria-hidden="true">
      <defs>
        <linearGradient id="kw-mark-bg" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#6366f1" />
          <stop offset="1" stopColor="#8b5cf6" />
        </linearGradient>
      </defs>
      <rect x="1" y="1" width="30" height="30" rx="8" fill="url(#kw-mark-bg)" />
      <path
        d="M10 9.5h3.1v5.1l4.9-5.1h3.8l-5.4 5.7 5.7 6.3h-3.9l-5.1-5.9v5.9H10z"
        fill="#fff"
      />
      <circle cx="24.5" cy="9.8" r="1.8" fill="#fbbf24" />
    </svg>
  );
}

export interface TopBarProps {
  focusMode: boolean;
  dockOpen: boolean;
  dockTab: DockTab;
  /** 系统主题的当前生效值(用于显示月亮/太阳图标)。 */
  resolvedTheme: "light" | "dark";
  themeMode: ThemeMode;
  accentPreset: AccentPreset;
  modelState: "unknown" | "ready" | "unavailable";
  onSelectDockTab: (tab: DockTab) => void;
  onToggleFocusMode: () => void;
  onToggleTheme: () => void;
  onSelectAccent: (preset: AccentPreset) => void;
  onOpenShortcuts: () => void;
  onOpenSettings: () => void;
  onOpenArtifacts?: () => void;
}

export function TopBar({
  focusMode,
  dockOpen,
  dockTab,
  resolvedTheme,
  themeMode,
  accentPreset,
  modelState,
  onSelectDockTab,
  onToggleFocusMode,
  onToggleTheme,
  onSelectAccent,
  onOpenShortcuts,
  onOpenSettings,
  onOpenArtifacts,
}: TopBarProps): ReactNode {
  const agentActive = dockOpen && dockTab === "agent";
  const notesActive = dockOpen && dockTab === "notes";
  const artifactsActive = dockOpen && dockTab === "artifacts";
  const [accentOpen, setAccentOpen] = useState(false);
  const accentRef = useRef<HTMLDivElement | null>(null);

  const { client } = useServices();
  const [taskCenterOpen, setTaskCenterOpen] = useState(false);
  const taskCenterTriggerRef = useRef<HTMLButtonElement | null>(null);
  const taskCenterRef = useRef<HTMLDivElement | null>(null);

  const tasks = useQuery({
    // 顶栏角标只要最近 30 条;任务中心用 limit 50,不能共用同一缓存键,
    // 否则先打开的 30 条会被另一处当成完整结果(标题回退、旧任务不可达)。
    queryKey: ["tasks", "topbar", 30],
    queryFn: () => client.call<{ items: TaskView[] }>("task_list", { activeOnly: false, limit: 30 }),
    // 顶栏角标只在有活跃任务时需要跟到终态;空闲时降频,避免常驻 3s IPC。
    refetchInterval: (query) =>
      (query.state.data?.items ?? []).some(
        (t) => t.status === "running" || t.status === "queued",
      )
        ? 3000
        : 30_000,
  });

  const hasRunningTasks = (tasks.data?.items ?? []).some(
    (t) => t.status === "running" || t.status === "queued",
  );

  useEffect(() => {
    if (!taskCenterOpen) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (target === null) return;
      // 面板已 portal 到 body,不在触发器 DOM 子树内;两个 ref 都要检查,
      // 否则点击触发器会先关闭再被 click 重新打开。
      if (taskCenterRef.current?.contains(target) === true) return;
      if (taskCenterTriggerRef.current?.contains(target) === true) return;
      setTaskCenterOpen(false);
    };
    window.addEventListener("mousedown", onPointerDown);
    return () => window.removeEventListener("mousedown", onPointerDown);
  }, [taskCenterOpen]);

  useEffect(() => {
    if (!accentOpen) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (target !== null && accentRef.current?.contains(target) === true) return;
      setAccentOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setAccentOpen(false);
    };
    window.addEventListener("mousedown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [accentOpen]);

  return (
    <header className="top-bar" role="banner">
      <div className="top-bar-brand">
        <KnowwiseMark />
        <span className="app-name">Knowwise</span>
      </div>
      {/* 中段:功能选项(阅读/笔记的同域动作占位,窄窗口自动隐藏) */}
      <div className="top-bar-center" aria-hidden="true">
        <span className="top-bar-hint">
          {notesActive ? "笔记空间" : artifactsActive ? "资料" : "阅读与提问"}
        </span>
      </div>
      <div className="top-bar-actions">
        <button
          type="button"
          className={`top-icon-btn${agentActive ? " active" : ""}`}
          onClick={() => onSelectDockTab("agent")}
          aria-pressed={agentActive}
          title="AI 助手 (Alt+Shift+A)"
          aria-label="AI 助手"
        >
          <MessageSquare className="w-4 h-4" />
        </button>
        <button
          type="button"
          className={`top-icon-btn${notesActive ? " active" : ""}`}
          onClick={() => onSelectDockTab("notes")}
          aria-pressed={notesActive}
          title="笔记空间 (Alt+Shift+N)"
          aria-label="笔记空间"
        >
          <NotebookPen className="w-4 h-4" />
        </button>
        <button
          type="button"
          className={`top-icon-btn${artifactsActive ? " active" : ""}`}
          onClick={onOpenArtifacts}
          aria-pressed={artifactsActive}
          title="资料 (Alt+Shift+P)"
          aria-label="资料"
        >
          <Sparkles className="w-4 h-4" />
        </button>

        <div className="task-center-wrapper relative">
          <button
            ref={taskCenterTriggerRef}
            type="button"
            className={`top-icon-btn${taskCenterOpen ? " active" : ""}`}
            onClick={() => setTaskCenterOpen((open) => !open)}
            title="任务中心 (解析 / 问答 / 研究 / 生成 / 导出)"
            aria-label="任务中心"
            aria-expanded={taskCenterOpen}
          >
            <ListTodo className="w-4 h-4" />
            {hasRunningTasks ? (
              <span
                className="accent-picker-dot animate-pulse"
                style={{ backgroundColor: "#6366f1" }}
                aria-hidden="true"
              />
            ) : null}
          </button>
          {/* 任务中心是整窗浮层:必须脱离 .top-bar 的层叠上下文,
              否则会被窄窗口的 .dock-panel(z-index 100)整体盖住。 */}
          {taskCenterOpen
            ? createPortal(
                <div ref={taskCenterRef}>
                  <TaskCenterPanel open onClose={() => setTaskCenterOpen(false)} />
                </div>,
                document.body,
              )
            : null}
        </div>
        <span className="top-bar-divider" aria-hidden />
        <button
          type="button"
          className={`top-icon-btn${focusMode ? " active" : ""}`}
          onClick={onToggleFocusMode}
          aria-pressed={focusMode}
          title="专注模式 (Alt+Shift+F)"
          aria-label="切换专注模式"
        >
          {focusMode ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
        </button>
        <div className="accent-picker" ref={accentRef}>
          <button
            type="button"
            className={`top-icon-btn${accentOpen ? " active" : ""}`}
            onClick={() => setAccentOpen((open) => !open)}
            aria-haspopup="menu"
            aria-expanded={accentOpen}
            title="强调色"
            aria-label="选择强调色"
          >
            <Palette className="w-4 h-4" />
            <span
              className="accent-picker-dot"
              data-accent-swatch={accentPreset}
              aria-hidden="true"
            />
          </button>
          {accentOpen ? (
            <div className="accent-popover" role="menu" aria-label="强调色">
              <p className="accent-popover-title">强调色</p>
              <div className="accent-grid">
                {ACCENT_PRESETS.map((preset) => (
                  <button
                    key={preset.id}
                    type="button"
                    role="menuitemradio"
                    aria-checked={accentPreset === preset.id}
                    className={`accent-swatch${accentPreset === preset.id ? " current" : ""}`}
                    title={`${preset.label} · ${preset.hint}`}
                    onClick={() => {
                      onSelectAccent(preset.id);
                      setAccentOpen(false);
                    }}
                  >
                    <span
                      className="accent-swatch-dot"
                      style={{ backgroundColor: preset.light }}
                      aria-hidden="true"
                    />
                    <span className="accent-swatch-label">{preset.label}</span>
                    {accentPreset === preset.id ? (
                      <Check className="w-3 h-3" aria-hidden="true" />
                    ) : null}
                  </button>
                ))}
              </div>
            </div>
          ) : null}
        </div>
        <button
          type="button"
          className="top-icon-btn"
          onClick={onToggleTheme}
          title={
            themeMode === "system"
              ? "主题:跟随系统(点击切换深色)"
              : resolvedTheme === "dark"
                ? "切换到浅色主题"
                : "切换到深色主题"
          }
          aria-label="切换主题"
        >
          {resolvedTheme === "dark" ? <Sun className="w-4 h-4" /> : <Moon className="w-4 h-4" />}
        </button>
        <button
          type="button"
          className="top-icon-btn"
          onClick={onOpenShortcuts}
          title="快捷键帮助 (F1)"
          aria-label="快捷键帮助"
        >
          <Keyboard className="w-4 h-4" />
        </button>
        <span className="top-bar-divider" aria-hidden />
        <button
          type="button"
          className="top-icon-btn"
          onClick={onOpenSettings}
          title={modelState === "unavailable" ? "设置(模型未就绪,点击配置)" : "设置"}
          aria-label="设置"
        >
          <Settings className="w-4 h-4" />
          {modelState === "unavailable" ? <span className="notify-dot" aria-hidden /> : null}
        </button>
      </div>
    </header>
  );
}
