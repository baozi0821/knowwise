// M08 拥有:设置面板「模型与提供方」分区。能力摘要 + 提供方卡片
// (远程:Base URL/密钥/启用/删除;Ollama:检测本机模型) + 模型选择
// (探测列表与手动 ID 均可设为生成/嵌入参考) + 「添加 API 提供方」
// 表单(常用 OpenAI 兼容服务预设)。写入经 settings_update /
// remote_secret_* / model_set_reference。
import { useMemo, useState } from "react";
import { Plus, Trash2, RefreshCw, Globe, Cpu, CheckCircle2, XCircle, CircleDashed } from "lucide-react";
import type { ModelDescriptor } from "../../shared/generated/ModelDescriptor";
import type { ModelProviderDescriptor } from "../../shared/generated/ModelProviderDescriptor";
import type { ModelSettingsView } from "../../shared/generated/ModelSettingsView";
import type { ModelStatusView } from "../../shared/generated/ModelStatusView";
import type { ProviderConfigRecord } from "../../shared/generated/ProviderConfigRecord";
import type { CommandClient } from "../../app/api/client";
import {
  CAPABILITY_LABELS,
  DEGRADATION_LABELS,
  PURPOSE_LABELS,
  maskSecretConfigured,
} from "./policy";
import { CapabilityEditor } from "./CapabilityEditor";

/** 常用 OpenAI 兼容服务预设;.baseUrl 填入表单可改。 */
const API_PRESETS: Array<{ name: string; baseUrl: string }> = [
  { name: "DeepSeek", baseUrl: "https://api.deepseek.com/v1" },
  { name: "Moonshot Kimi", baseUrl: "https://api.moonshot.cn/v1" },
  { name: "智谱 GLM", baseUrl: "https://open.bigmodel.cn/api/paas/v4" },
  { name: "通义千问", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1" },
  { name: "硅基流动", baseUrl: "https://api.siliconflow.cn/v1" },
  { name: "OpenAI", baseUrl: "https://api.openai.com/v1" },
];

function providerStatus(p: ModelProviderDescriptor): {
  tone: "ok" | "warn" | "muted";
  label: string;
} {
  if (!p.configured) return { tone: "muted", label: "未配置" };
  if (p.reachable) return { tone: "ok", label: "已连接" };
  return { tone: "warn", label: "已配置·暂不可达" };
}

function ProviderStatusIcon({ tone }: { tone: "ok" | "warn" | "muted" }) {
  if (tone === "ok") return <CheckCircle2 className="w-3.5 h-3.5 text-emerald-500" aria-hidden />;
  if (tone === "warn") return <XCircle className="w-3.5 h-3.5 text-amber-500" aria-hidden />;
  return <CircleDashed className="w-3.5 h-3.5 text-slate-400" aria-hidden />;
}

function ModelRow({
  modelId,
  displayName,
  capabilities,
  isGenerate,
  isFast,
  isQuality,
  isTranslation,
  isEmbed,
  onSet,
}: {
  modelId: string;
  displayName: string;
  capabilities?: string[];
  isGenerate: boolean;
  isFast: boolean;
  isQuality: boolean;
  isTranslation: boolean;
  isEmbed: boolean;
  onSet: (role: "generate" | "fast" | "quality" | "translation" | "embed") => void;
}) {
  return (
    <li className={`model-row${isGenerate || isFast || isQuality || isTranslation || isEmbed ? " referenced" : ""}`}>
      <span className="model-row-name" title={modelId}>
        {displayName}
        {capabilities?.includes("embed") ? <em className="model-cap" aria-hidden>embed</em> : null}
      </span>
      <span className="model-row-actions">
        {isGenerate ? (
          <span className="ref-chip">生成 ✓</span>
        ) : (
          <button type="button" className="btn small" onClick={() => onSet("generate")}>
            设为生成
          </button>
        )}
        {isFast ? (
          <span className="ref-chip">快速 ✓</span>
        ) : (
          <button type="button" className="btn small" onClick={() => onSet("fast")}>
            设为快速
          </button>
        )}
        {isQuality ? (
          <span className="ref-chip">质量 ✓</span>
        ) : (
          <button type="button" className="btn small" onClick={() => onSet("quality")}>
            设为质量
          </button>
        )}
        {isTranslation ? (
          <span className="ref-chip">翻译 ✓</span>
        ) : (
          <button type="button" className="btn small" onClick={() => onSet("translation")}>
            设为翻译
          </button>
        )}
        {capabilities?.includes("embed") ? (
          isEmbed ? (
            <span className="ref-chip">嵌入 ✓</span>
          ) : (
            <button type="button" className="btn small" onClick={() => onSet("embed")}>
              设为嵌入
            </button>
          )
        ) : null}
      </span>
    </li>
  );
}

function ProviderCard({
  client,
  provider,
  config,
  statusRevision,
  generateReference,
  fastReference,
  qualityReference,
  translationReference,
  embedReference,
  onDone,
  onError,
  onRemove,
}: {
  client: CommandClient;
  provider: ModelProviderDescriptor;
  config: ProviderConfigRecord | undefined;
  statusRevision: number;
  generateReference: { providerId: string; modelId: string } | null;
  fastReference: { providerId: string; modelId: string } | null;
  qualityReference: { providerId: string; modelId: string } | null;
  translationReference: { providerId: string; modelId: string } | null;
  embedReference: { providerId: string; modelId: string } | null;
  onDone: () => void;
  onError: (message: string) => void;
  onRemove: (providerId: string) => void;
}) {
  const [secretDraft, setSecretDraft] = useState("");
  const [baseUrlDraft, setBaseUrlDraft] = useState(config?.baseUrl ?? "");
  const [enabledDraft, setEnabledDraft] = useState(config?.enabled ?? true);
  const [localModels, setLocalModels] = useState<ModelDescriptor[] | null>(null);
  const [detecting, setDetecting] = useState(false);
  const [manualModel, setManualModel] = useState("");

  const models = localModels ?? provider.availableModels;
  const status = providerStatus(provider);
  const isOllama = provider.kind === "ollama";

  async function detectLocal() {
    setDetecting(true);
    try {
      const list = await client.call<ModelDescriptor[]>("model_list_local_models", {});
      setLocalModels(list);
    } catch {
      onError("本机模型检测失败(确认 Ollama 已启动且地址正确)");
    } finally {
      setDetecting(false);
    }
  }

  async function setReference(
    role: "generate" | "fast" | "quality" | "translation" | "embed",
    modelId: string,
  ) {
    try {
      await client.call("model_set_reference", {
        role,
        providerId: provider.id,
        modelId,
        expectedRevision: statusRevision,
      });
      onDone();
    } catch {
      const label =
        role === "fast"
          ? "快速"
          : role === "quality"
            ? "质量"
            : role === "translation"
              ? "翻译"
              : PURPOSE_LABELS[role];
      onError(`设置${label}参考模型失败(设置可能已被并发修改,重试)`);
    }
  }

  async function saveSecret() {
    if (secretDraft.trim().length === 0) return;
    try {
      await client.call("remote_secret_set", {
        providerId: provider.id,
        secret: secretDraft.trim(),
        idempotencyKey: client.newIdempotencyKey(),
      });
      setSecretDraft("");
      onDone();
    } catch {
      onError("密钥保存失败(密钥只进系统凭据存储,失败不落盘)");
    }
  }

  async function deleteSecret() {
    try {
      await client.call("remote_secret_delete", {
        providerId: provider.id,
        expectedRevision: statusRevision,
      });
      onDone();
    } catch {
      onError("密钥删除失败,请重试");
    }
  }

  async function saveProviderConfig() {
    if (config === undefined) return;
    try {
      // providers 为全量替换:以设置中的数组为基础仅替换本行。
      const view = await client.call<ModelSettingsView>("settings_get", {});
      const providers = (view.settings.providers ?? []).map((p) =>
        p.id === provider.id ? { ...p, baseUrl: baseUrlDraft, enabled: enabledDraft } : p,
      );
      await client.call("settings_update", {
        patch: {
          providers,
          ollamaBaseUrl: null,
          generateReference: null,
          embedReference: null,
          reduceMotion: null,
          diagnosticsIncludePaths: null,
          diagnosticsIncludeModelDetails: null,
        },
        expectedRevision: view.revision,
      });
      onDone();
    } catch {
      onError("提供方配置保存失败(可能已被并发修改,重试)");
    }
  }

  return (
    <div className="settings-card provider-card">
      <div className="provider-card-head">
        <span className={`provider-kind-icon${isOllama ? " local" : " remote"}`}>
          {isOllama ? (
            <Cpu className="w-4 h-4" aria-hidden />
          ) : (
            <Globe className="w-4 h-4" aria-hidden />
          )}
        </span>
        <div className="provider-card-title">
          <h3>{provider.displayName}</h3>
          <span className={`provider-status tone-${status.tone}`}>
            <ProviderStatusIcon tone={status.tone} />
            {status.label} · {isOllama ? "本地" : "OpenAI 兼容"}
          </span>
        </div>
        {config !== undefined ? (
          <button
            type="button"
            className="btn small danger-ghost"
            title="删除该提供方"
            onClick={() => onRemove(provider.id)}
          >
            <Trash2 className="w-3.5 h-3.5" aria-hidden />
          </button>
        ) : null}
      </div>

      {isOllama ? (
        <div className="settings-row">
          <button
            type="button"
            className="btn small"
            onClick={() => void detectLocal()}
            disabled={detecting}
          >
            <RefreshCw className={`w-3.5 h-3.5${detecting ? " spin" : ""}`} aria-hidden />
            {detecting ? "检测中…" : "检测本机模型"}
          </button>
          <span className="settings-meta">列出本机 Ollama 已拉取的模型</span>
        </div>
      ) : (
        <>
          <div className="settings-row">
            <label className="settings-input grow">
              <span>Base URL</span>
              <input
                type="url"
                value={baseUrlDraft}
                onChange={(e) => setBaseUrlDraft(e.target.value)}
                placeholder="https://api.example.com/v1"
              />
            </label>
            <label className="switch-row">
              <input
                type="checkbox"
                checked={enabledDraft}
                onChange={(e) => setEnabledDraft(e.target.checked)}
              />
              <span>启用</span>
            </label>
            <button type="button" className="btn small" onClick={() => void saveProviderConfig()}>
              保存
            </button>
          </div>
          <div className="settings-row">
            <label className="settings-input grow">
              <span>API Key(只写不读,保存后进系统凭据存储)</span>
              <input
                type="password"
                value={secretDraft}
                onChange={(e) => setSecretDraft(e.target.value)}
                placeholder="sk-…"
                autoComplete="off"
              />
            </label>
            <button
              type="button"
              className="btn small"
              onClick={() => void saveSecret()}
              disabled={secretDraft.trim().length === 0}
            >
              保存密钥
            </button>
            <button type="button" className="btn small ghost" onClick={() => void deleteSecret()}>
              删除密钥
            </button>
          </div>
        </>
      )}

      {models.length > 0 ? (
        <ul className="model-list">
          {models.map((m) => (
            <ModelRow
              key={m.id}
              modelId={m.id}
              displayName={m.displayName ?? m.id}
              capabilities={m.capabilities}
              isGenerate={
                generateReference?.providerId === provider.id &&
                generateReference?.modelId === m.id
              }
              isFast={
                fastReference?.providerId === provider.id && fastReference?.modelId === m.id
              }
              isQuality={
                qualityReference?.providerId === provider.id &&
                qualityReference?.modelId === m.id
              }
              isTranslation={
                translationReference?.providerId === provider.id &&
                translationReference?.modelId === m.id
              }
              isEmbed={
                embedReference?.providerId === provider.id && embedReference?.modelId === m.id
              }
              onSet={(role) => void setReference(role, m.id)}
            />
          ))}
        </ul>
      ) : (
        <p className="settings-meta">
          {isOllama
            ? "暂未列出模型;点击「检测本机模型」。"
            : "该提供方暂未探测到模型;若服务不提供 /models 列表,请在下方手动填写模型 ID。"}
        </p>
      )}

      {/* 2F:预算按真实上下文窗口计算,用户可在此修正自动探测结果。 */}
      {(fastReference?.providerId === provider.id && fastReference?.modelId) ||
      (generateReference?.providerId === provider.id && generateReference?.modelId) ? (
        <CapabilityEditor
          client={client}
          providerId={provider.id}
          modelId={
            (fastReference?.providerId === provider.id ? fastReference?.modelId : null) ??
            generateReference!.modelId
          }
          onError={onError}
        />
      ) : null}

      <div className="settings-row manual-model-row">
        <label className="settings-input grow">
          <span>手动模型 ID(适用于不提供模型列表的服务)</span>
          <input
            type="text"
            value={manualModel}
            onChange={(e) => setManualModel(e.target.value)}
            placeholder="例如 deepseek-chat / glm-4.5 / qwen2.5:7b"
          />
        </label>
        <button
          type="button"
          className="btn small"
          disabled={manualModel.trim().length === 0}
          onClick={() => void setReference("generate", manualModel.trim())}
        >
          设为生成
        </button>
        <button
          type="button"
          className="btn small"
          disabled={manualModel.trim().length === 0}
          onClick={() => void setReference("fast", manualModel.trim())}
        >
          设为快速
        </button>
        <button
          type="button"
          className="btn small"
          disabled={manualModel.trim().length === 0}
          onClick={() => void setReference("quality", manualModel.trim())}
        >
          设为质量
        </button>
        <button
          type="button"
          className="btn small"
          disabled={manualModel.trim().length === 0}
          onClick={() => void setReference("embed", manualModel.trim())}
        >
          设为嵌入
        </button>
      </div>
    </div>
  );
}

/** 添加 API 提供方表单:预设 + 名称/URL/Key;保存后自动探测模型列表。 */
function AddProviderForm({
  client,
  onDone,
  onError,
}: {
  client: CommandClient;
  onDone: () => void;
  onError: (message: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [saving, setSaving] = useState(false);

  const canSubmit =
    name.trim().length > 0 && /^https?:\/\/.+/.test(baseUrl.trim()) && apiKey.trim().length > 0;

  async function submit() {
    if (!canSubmit) return;
    setSaving(true);
    try {
      const view = await client.call<ModelSettingsView>("settings_get", {});
      const id = `custom-${name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-")}-${Date.now()
        .toString(36)
        .slice(-4)}`;
      const providers = [
        ...(view.settings.providers ?? []),
        {
          id,
          kind: "openai_compatible" as const,
          displayName: name.trim(),
          baseUrl: baseUrl.trim().replace(/\/+$/, ""),
          enabled: true,
          // 不写 embedDimensions: 0。后端 Option<i64> 会把 0 当作显式维度并
          // 拒绝(合法范围 1..=8192),导致“添加失败”。省略字段即 None。
        } as ProviderConfigRecord,
      ];
      await client.call("settings_update", {
        patch: {
          providers,
          ollamaBaseUrl: null,
          generateReference: null,
          embedReference: null,
          reduceMotion: null,
          diagnosticsIncludePaths: null,
          diagnosticsIncludeModelDetails: null,
        },
        expectedRevision: view.revision,
      });
      await client.call("remote_secret_set", {
        providerId: id,
        secret: apiKey.trim(),
        idempotencyKey: client.newIdempotencyKey(),
      });
      setName("");
      setBaseUrl("");
      setApiKey("");
      setOpen(false);
      onDone();
    } catch {
      onError("添加失败(可能已被并发修改,请重试)");
    } finally {
      setSaving(false);
    }
  }

  if (!open) {
    return (
      <button type="button" className="btn primary add-provider-btn" onClick={() => setOpen(true)}>
        <Plus className="w-4 h-4" aria-hidden />
        <span>添加 API 提供方</span>
      </button>
    );
  }

  return (
    <div className="settings-card add-provider-card">
      <h3>添加 API 提供方(OpenAI 兼容)</h3>
      <div className="preset-row" role="group" aria-label="常用服务预设">
        {API_PRESETS.map((preset) => (
          <button
            key={preset.name}
            type="button"
            className="preset-chip"
            onClick={() => {
              setName((current) => (current.trim() === "" ? preset.name : current));
              setBaseUrl(preset.baseUrl);
            }}
          >
            {preset.name}
          </button>
        ))}
      </div>
      <div className="settings-row">
        <label className="settings-input grow">
          <span>名称</span>
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="例如 DeepSeek、公司网关"
          />
        </label>
      </div>
      <div className="settings-row">
        <label className="settings-input grow">
          <span>Base URL(以 /v1 等版本路径结尾)</span>
          <input
            type="url"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder="https://api.deepseek.com/v1"
          />
        </label>
      </div>
      <div className="settings-row">
        <label className="settings-input grow">
          <span>API Key</span>
          <input
            type="password"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            placeholder="sk-…"
            autoComplete="off"
          />
        </label>
      </div>
      <div className="settings-row">
        <button
          type="button"
          className="btn primary"
          onClick={() => void submit()}
          disabled={!canSubmit || saving}
        >
          {saving ? "保存中…" : "保存并探测"}
        </button>
        <button type="button" className="btn ghost" onClick={() => setOpen(false)}>
          取消
        </button>
      </div>
    </div>
  );
}

export function ModelsSection({
  client,
  status,
  settings,
  onMutated,
  onError,
}: {
  client: CommandClient;
  status: ModelStatusView;
  settings: ModelSettingsView;
  onMutated: () => void;
  onError: (message: string) => void;
}) {
  async function removeProvider(providerId: string) {
    try {
      const view = await client.call<ModelSettingsView>("settings_get", {});
      const providers = (view.settings.providers ?? []).filter((p) => p.id !== providerId);
      await client.call("settings_update", {
        patch: {
          providers,
          ollamaBaseUrl: null,
          generateReference: null,
          embedReference: null,
          reduceMotion: null,
          diagnosticsIncludePaths: null,
          diagnosticsIncludeModelDetails: null,
        },
        expectedRevision: view.revision,
      });
      try {
        await client.call("remote_secret_delete", {
          providerId,
          expectedRevision: view.revision,
        });
      } catch {
        // 密钥清理失败不阻塞提供方删除。
      }
      onMutated();
    } catch {
      onError("删除提供方失败,请重试");
    }
  }

  const generateRef = status.generateReference;
  const fastRef = status.fastReference;
  const qualityRef = status.qualityReference;
  const translationRef = status.translationReference;
  const embedRef = status.embedReference;

  const referenceSummary = useMemo(() => {
    const label = (
      reference: { providerId: string; modelId: string } | null | undefined,
    ) => (reference ? `${reference.providerId} / ${reference.modelId}` : "跟随生成");
    return [
      `生成:${generateRef ? `${generateRef.providerId} / ${generateRef.modelId}` : "未设置"}`,
      `快速:${label(fastRef)}`,
      `质量:${label(qualityRef)}`,
      `翻译:${translationRef ? `${translationRef.providerId} / ${translationRef.modelId}` : "回退质量"}`,
      `嵌入:${embedRef ? `${embedRef.providerId} / ${embedRef.modelId}` : "未设置"}`,
    ].join(" · ");
  }, [generateRef, fastRef, qualityRef, translationRef, embedRef]);

  return (
    <div className="settings-section">
      <div className="settings-summary-row">
        <span className={`capability-chip cap-${status.capability}`}>
          {CAPABILITY_LABELS[status.capability]}
        </span>
        <span className="settings-meta">{referenceSummary}</span>
      </div>
      {status.reasons.length > 0 ? (
        <p className="settings-meta warn">
          {status.reasons.map((r) => DEGRADATION_LABELS[r]).join("、")}
        </p>
      ) : null}
      {status.secrets.length > 0 ? (
        <p className="settings-meta">
          密钥:
          {status.secrets
            .map((s) => `${s.providerId}(${maskSecretConfigured(s.configured, s.updatedAt)})`)
            .join("、")}
        </p>
      ) : null}

      <AddProviderForm client={client} onDone={onMutated} onError={onError} />

      {status.providers.map((p) => (
        <ProviderCard
          key={p.id}
          client={client}
          provider={p}
          config={settings.settings.providers.find((c) => c.id === p.id)}
          statusRevision={status.settingsRevision}
          generateReference={status.generateReference ?? null}
          fastReference={status.fastReference ?? null}
          qualityReference={status.qualityReference ?? null}
          translationReference={status.translationReference ?? null}
          embedReference={status.embedReference ?? null}
          onDone={onMutated}
          onError={onError}
          onRemove={(id) => void removeProvider(id)}
        />
      ))}
      {status.providers.length === 0 ? (
        <p className="settings-meta">还没有提供方;点击上方「添加 API 提供方」接入外接模型。</p>
      ) : null}
    </div>
  );
}
