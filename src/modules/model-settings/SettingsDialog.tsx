// M08 拥有:设置对话框(唯一入口,顶栏「设置」打开)。
// 查询:settings_get / model_get_status;变更后失效相关查询;
// 错误经 describeAppError 呈现,不吞错。
// 布局:左侧分区导航 + 右侧内容(宽对话框)。
//
// 隐私分区已按需求移除(默认全部允许):导入文档时自动授予文档级许可,
// 但网关许可、载荷哈希、来源白名单与审计仍然保留,不绕过任何校验。
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { X, Cpu, SlidersHorizontal } from "lucide-react";
import { useServices } from "../../app/services";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import type { ModelSettingsView } from "../../shared/generated/ModelSettingsView";
import type { ModelStatusView } from "../../shared/generated/ModelStatusView";
import { AppSection } from "./AppSection";
import { setStreamSpeed } from "../qa/streamSpeed";
import { ModelsSection } from "./ModelsSection";
import "./settings.css";

export type SettingsSection = "models" | "app";

const TAB_META: Record<SettingsSection, { label: string; icon: typeof Cpu; hint: string }> = {
  models: { label: "模型服务", icon: Cpu, hint: "提供方与参考模型" },
  app: { label: "应用与偏好", icon: SlidersHorizontal, hint: "本地链路与请求边界" },
};

/** 兼容旧入口(顶栏/旧事件可能带 "privacy"):统一落到模型分区。 */
export function normalizeSettingsSection(section: string | undefined): SettingsSection {
  return section === "app" ? "app" : "models";
}

export function SettingsDialog({
  initialSection,
  onClose,
}: {
  initialSection: string;
  onClose: () => void;
}) {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const [section, setSection] = useState<SettingsSection>(
    normalizeSettingsSection(initialSection),
  );
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState<string | null>(null);

  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: () => client.call<ModelSettingsView>("settings_get", {}),
  });
  const status = useQuery({
    queryKey: ["model", "status"],
    queryFn: () => client.call<ModelStatusView>("model_get_status", {}),
  });
  // 后端设置是输出速度的权威来源:打开设置时同步到前端呈现层。
  useEffect(() => {
    if (settings.data !== undefined) {
      setStreamSpeed(settings.data.settings.streamSpeed);
    }
  }, [settings.data]);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  function presentError(err: unknown) {
    if (err instanceof ApiError) {
      const p = describeAppError(err.appError);
      setError(
        `${p.title}${p.actionLabel ? `;可:${p.actionLabel}` : ""}${
          p.detailLines.length ? `(${p.detailLines.join(";")})` : ""
        }`,
      );
    } else {
      setError("操作未完成,请重试");
    }
  }

  async function mutate(action: () => Promise<unknown>) {
    setError(null);
    setOk(null);
    try {
      await action();
      await queryClient.invalidateQueries({ queryKey: ["settings"] });
      await queryClient.invalidateQueries({ queryKey: ["model"] });
      await queryClient.invalidateQueries({ queryKey: ["vision", "status"] });
      setOk("已保存");
    } catch (err) {
      presentError(err);
    }
  }

  return (
    <div className="settings-overlay">
      <div
        role="dialog"
        aria-modal="true"
        aria-label="设置"
        className="settings-dialog"
      >
        <div className="settings-head">
          <h2>设置</h2>
          <button
            type="button"
            className="icon-btn"
            onClick={onClose}
            aria-label="关闭设置"
            title="关闭 (Esc)"
          >
            <X className="w-4 h-4" />
          </button>
        </div>
        <div className="settings-layout">
          <nav className="settings-tabs" role="tablist" aria-label="设置分区">
            {(Object.keys(TAB_META) as SettingsSection[]).map((key) => {
              const meta = TAB_META[key];
              const Icon = meta.icon;
              return (
                <button
                  key={key}
                  type="button"
                  role="tab"
                  aria-selected={section === key}
                  className={`settings-tab${section === key ? " active" : ""}`}
                  onClick={() => {
                    setSection(key);
                    setError(null);
                    setOk(null);
                  }}
                >
                  <Icon className="w-4 h-4" aria-hidden />
                  <span className="settings-tab-text">
                    <span className="settings-tab-label">{meta.label}</span>
                    <span className="settings-tab-hint">{meta.hint}</span>
                  </span>
                </button>
              );
            })}
          </nav>
          <div className="settings-body">
            {error ? (
              <p className="settings-error" role="alert">
                {error}
              </p>
            ) : null}
            {ok ? (
              <p className="settings-ok" role="status">
                {ok}
              </p>
            ) : null}
            {section === "models" ? (
              settings.isError || status.isError ? (
                <p className="settings-error" role="alert">
                  设置加载失败;关闭后重试。
                </p>
              ) : settings.data && status.data ? (
                <ModelsSection
                  client={client}
                  settings={settings.data}
                  status={status.data}
                  onMutated={() => void mutate(async () => undefined)}
                  onError={setError}
                />
              ) : (
                <p role="status">设置加载中…</p>
              )
            ) : null}
            {section === "app" ? (
              settings.data ? (
                <AppSection
                  client={client}
                  settings={settings.data}
                  onMutated={() => void mutate(async () => undefined)}
                  onError={setError}
                />
              ) : (
                <p role="status">设置加载中…</p>
              )
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
}
