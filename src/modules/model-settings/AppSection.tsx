// M08 拥有:设置面板「应用与偏好」分区。编辑 ModelSettingsV1 的应用层
// 字段(本地 Ollama 地址、并发/超时/响应上限、减少动态、诊断默认勾选);
// 保存走 settings_update(乐观并发修订号)。
import { useRef, useState } from "react";
import type { ModelSettingsView } from "../../shared/generated/ModelSettingsView";
import { ApiError } from "../../app/api/client";
import type { CommandClient } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";
import {
  ACCENT_PRESETS,
  applyAccent,
  applyTheme,
  loadAccent,
  loadTheme,
  notifyAccentChanged,
  saveAccent,
  saveTheme,
  type AccentPreset,
  type ThemeMode,
} from "../../app/theme";
import { setStreamSpeed } from "../qa/streamSpeed";
import { PrivacySection } from "./PrivacySection";

type Draft = {
  ollamaBaseUrl: string;
  maxConcurrentRequests: string;
  requestTimeoutMs: string;
  maxResponseBytes: string;
  reduceMotion: boolean;
  allowRemoteVision: boolean;
  diagnosticsIncludePaths: boolean;
  diagnosticsIncludeModelDetails: boolean;
  defaultMode: "fast" | "quality";
  defaultPageWindow: string;
  showRawReasoning: boolean;
  saveReasoning: boolean;
  ocrLanguages: string;
  autoOcr: boolean;
  translationTargetLanguage: string;
  showCostEstimate: boolean;
  allowExternalTranslationEngine: boolean;
  translationGlossary: string;
  streamSpeed: "slow" | "standard" | "fast";
};

function toDraft(view: ModelSettingsView): Draft {
  const s = view.settings;
  return {
    ollamaBaseUrl: s.ollamaBaseUrl,
    maxConcurrentRequests: String(s.maxConcurrentRequests),
    requestTimeoutMs: String(s.requestTimeoutMs),
    maxResponseBytes: String(s.maxResponseBytes),
    reduceMotion: s.reduceMotion,
    allowRemoteVision: s.allowRemoteVision,
    diagnosticsIncludePaths: s.diagnosticsIncludePaths,
    diagnosticsIncludeModelDetails: s.diagnosticsIncludeModelDetails,
    defaultMode: s.defaultMode === "quality" ? "quality" : "fast",
    defaultPageWindow: String(s.defaultPageWindow),
    showRawReasoning: s.showRawReasoning,
    saveReasoning: s.saveReasoning,
    ocrLanguages: s.ocrLanguages,
    autoOcr: s.autoOcr !== false,
    translationTargetLanguage: s.translationTargetLanguage,
    showCostEstimate: s.showCostEstimate,
    allowExternalTranslationEngine: s.allowExternalTranslationEngine,
    translationGlossary: s.translationGlossary
      .map((entry) => `${entry.source}=${entry.target}`)
      .join("\n"),
    streamSpeed:
      s.streamSpeed === "slow" || s.streamSpeed === "fast" ? s.streamSpeed : "standard",
  };
}

export function AppSection({
  client,
  settings,
  onMutated,
  onError,
}: {
  client: CommandClient;
  settings: ModelSettingsView;
  onMutated: () => void;
  onError: (message: string) => void;
}) {
  const [draft, setDraft] = useState<Draft>(() => toDraft(settings));
  const [saving, setSaving] = useState(false);
  const glossaryFileRef = useRef<HTMLInputElement | null>(null);

  /** 术语表导入:接受 source=target 文本或两列 CSV,统一为每行 source=target。 */
  async function importGlossary(file: File): Promise<void> {
    try {
      const raw = await file.text();
      const lines = raw
        .replace(/^\uFEFF/, "")
        .split(/\r?\n/)
        .map((line) => line.trim())
        .filter((line) => line.length > 0 && !/^source\s*,\s*target$/i.test(line));
      const normalized = lines
        .map((line) => {
          if (line.includes("=")) return line;
          const comma = line.indexOf(",");
          if (comma <= 0) return null;
          const source = line.slice(0, comma).trim().replace(/^"|"$/g, "");
          const target = line.slice(comma + 1).trim().replace(/^"|"$/g, "");
          return source && target ? `${source}=${target}` : null;
        })
        .filter((line): line is string => line !== null);
      if (normalized.length === 0) {
        onError("术语表文件没有可识别的条目(需要 source=target 或两列 CSV)");
        return;
      }
      update("translationGlossary", normalized.join("\n"));
    } catch {
      onError("术语表文件读取失败");
    } finally {
      if (glossaryFileRef.current) glossaryFileRef.current.value = "";
    }
  }

  /** 术语表导出:UTF-8 CSV(带 BOM,便于 Excel 打开中文);不改动已保存设置。 */
  function exportGlossary(): void {
    const rows = draft.translationGlossary
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line.includes("="))
      .map((line): [string, string] => {
        const index = line.indexOf("=");
        return [line.slice(0, index).trim(), line.slice(index + 1).trim()];
      })
      .filter(([source, target]) => source.length > 0 && target.length > 0);
    const escape = (value: string) => `"${value.replace(/"/g, '""')}"`;
    const csv = ["source,target", ...rows.map(([s, t]) => `${escape(s)},${escape(t)}`)].join(
      "\r\n",
    );
    const blob = new Blob([`\uFEFF${csv}`], { type: "text/csv;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = "glossary.csv";
    anchor.click();
    URL.revokeObjectURL(url);
  }
  // 主题是纯界面偏好(localStorage),不写入后端设置;切换即生效。
  const [themeMode, setThemeMode] = useState<ThemeMode>(() => loadTheme());
  const [accentPreset, setAccentPreset] = useState<AccentPreset>(() => loadAccent());

  function chooseTheme(mode: ThemeMode) {
    setThemeMode(mode);
    applyTheme(mode);
    saveTheme(mode);
  }

  function chooseAccent(preset: AccentPreset) {
    setAccentPreset(preset);
    applyAccent(preset);
    saveAccent(preset);
    notifyAccentChanged(preset);
  }

  function update<K extends keyof Draft>(key: K, value: Draft[K]) {
    setDraft((prev) => ({ ...prev, [key]: value }));
  }

  async function save() {
    setSaving(true);
    try {
      await client.call("settings_update", {
        patch: {
          ollamaBaseUrl: draft.ollamaBaseUrl.trim(),
          providers: null,
          generateReference: null,
          embedReference: null,
          reduceMotion: draft.reduceMotion,
          allowRemoteVision: draft.allowRemoteVision,
          diagnosticsIncludePaths: draft.diagnosticsIncludePaths,
          diagnosticsIncludeModelDetails: draft.diagnosticsIncludeModelDetails,
          maxConcurrentRequests: Math.max(1, Number(draft.maxConcurrentRequests) || 1),
          requestTimeoutMs: Math.max(1000, Number(draft.requestTimeoutMs) || 30_000),
          maxResponseBytes: Math.max(1024, Number(draft.maxResponseBytes) || 1_048_576),
          defaultMode: draft.defaultMode,
          defaultPageWindow: Math.min(2, Math.max(0, Number(draft.defaultPageWindow) || 0)),
          showRawReasoning: draft.showRawReasoning,
          saveReasoning: draft.saveReasoning,
          ocrLanguages: draft.ocrLanguages.trim() || "chi_sim+eng",
          autoOcr: draft.autoOcr,
          translationTargetLanguage: draft.translationTargetLanguage.trim() || "zh-CN",
          showCostEstimate: draft.showCostEstimate,
          allowExternalTranslationEngine: draft.allowExternalTranslationEngine,
          streamSpeed: draft.streamSpeed,
          translationGlossary: draft.translationGlossary
            .split("\n")
            .map((line) => line.trim())
            .filter((line) => line.length > 0)
            .map((line) => {
              const index = line.indexOf("=");
              return index > 0
                ? { source: line.slice(0, index).trim(), target: line.slice(index + 1).trim() }
                : { source: line, target: line };
            })
            .filter((entry) => entry.source.length > 0 && entry.target.length > 0),
        },
        expectedRevision: settings.revision,
      });
      setStreamSpeed(draft.streamSpeed);
      onMutated();
    } catch (error) {
      if (error instanceof ApiError) {
        const presentation = describeAppError(error.appError);
        onError(
          [presentation.title, ...presentation.detailLines]
            .filter(Boolean)
            .join(";") + `(错误码 ${error.appError.code})`,
        );
      } else {
        onError("应用设置保存失败,请重试");
      }
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="settings-section">
      <div className="settings-card">
        <h3>本地模型链路</h3>
        <div className="settings-row">
          <label className="settings-input grow">
            <span>Ollama 地址(仅回环地址)</span>
            <input
              type="url"
              value={draft.ollamaBaseUrl}
              onChange={(e) => update("ollamaBaseUrl", e.target.value)}
              placeholder="http://127.0.0.1:11434"
            />
          </label>
        </div>
        <div className="settings-row">
          <label className="settings-input">
            <span>并发上限(≥1)</span>
            <input
              type="number"
              min={1}
              value={draft.maxConcurrentRequests}
              onChange={(e) => update("maxConcurrentRequests", e.target.value)}
            />
          </label>
          <label className="settings-input">
            <span>单次请求超时(ms)</span>
            <input
              type="number"
              min={1000}
              value={draft.requestTimeoutMs}
              onChange={(e) => update("requestTimeoutMs", e.target.value)}
            />
          </label>
          <label className="settings-input">
            <span>响应上限(字节)</span>
            <input
              type="number"
              min={1024}
              value={draft.maxResponseBytes}
              onChange={(e) => update("maxResponseBytes", e.target.value)}
            />
          </label>
        </div>
      </div>
      <div className="settings-card">
        <h3>图片与多模态</h3>
        <label className="switch-row">
          <input
            type="checkbox"
            checked={draft.allowRemoteVision}
            onChange={(e) => update("allowRemoteVision", e.target.checked)}
          />
          <span>允许把图片发送到远程模型(需文档级许可)</span>
        </label>
        <p className="settings-hint">
          关闭时远程请求不会携带任何图片;本地模型按既有本地规则处理。开启后,导入/打开文档会自动授予图片用途;网关仍会校验授权、载荷哈希与来源白名单。
        </p>
      </div>
      <div className="settings-card">
        <h3>外观</h3>
        <div className="settings-row">
          <span className="settings-label">主题</span>
          <div className="theme-choice" role="radiogroup" aria-label="主题">
            {(
              [
                ["light", "浅色"],
                ["dark", "深色"],
                ["system", "跟随系统"],
              ] as const
            ).map(([mode, label]) => (
              <label key={mode} className={`theme-choice-item${themeMode === mode ? " active" : ""}`}>
                <input
                  type="radio"
                  name="theme-mode"
                  value={mode}
                  checked={themeMode === mode}
                  onChange={() => chooseTheme(mode)}
                />
                <span>{label}</span>
              </label>
            ))}
          </div>
        </div>
        <div className="settings-row">
          <span className="settings-label">强调色</span>
          <div className="accent-settings-grid" role="radiogroup" aria-label="强调色">
            {ACCENT_PRESETS.map((preset) => (
              <button
                key={preset.id}
                type="button"
                role="radio"
                aria-checked={accentPreset === preset.id}
                className={`accent-settings-item${accentPreset === preset.id ? " active" : ""}`}
                title={preset.hint}
                onClick={() => chooseAccent(preset.id)}
              >
                <span
                  className="accent-settings-dot"
                  style={{ backgroundColor: preset.light }}
                  aria-hidden="true"
                />
                <span className="accent-settings-label">{preset.label}</span>
              </button>
            ))}
          </div>
        </div>
        <p className="settings-hint">
          深色主题只改变界面与阅读纸面,不修改任何文档内容;PDF 的「夜间」阅读样式仍可在阅读工具栏单独选择。
          强调色会应用到按钮、引用、焦点与选中态,并记住你的选择。
        </p>
      </div>
      <div className="settings-card">
        <h3>偏好与诊断</h3>
        <label className="switch-row">
          <input
            type="checkbox"
            checked={draft.reduceMotion}
            onChange={(e) => update("reduceMotion", e.target.checked)}
          />
          <span>减少动态效果</span>
        </label>
        <label className="switch-row">
          <input
            type="checkbox"
            checked={draft.diagnosticsIncludePaths}
            onChange={(e) => update("diagnosticsIncludePaths", e.target.checked)}
          />
          <span>诊断默认包含文件路径(导出前仍需逐次确认)</span>
        </label>
        <label className="switch-row">
          <input
            type="checkbox"
            checked={draft.diagnosticsIncludeModelDetails}
            onChange={(e) => update("diagnosticsIncludeModelDetails", e.target.checked)}
          />
          <span>诊断默认包含模型明细</span>
        </label>
      </div>
      <PrivacySection
        client={client}
        diagnosticsIncludePaths={draft.diagnosticsIncludePaths}
        diagnosticsIncludeModelDetails={draft.diagnosticsIncludeModelDetails}
        onError={onError}
      />
      <div className="settings-card">
        <h3>问答与阅读体验</h3>
        <div className="settings-row">
          <label className="settings-input">
            <span>默认模式</span>
            <select
              value={draft.defaultMode}
              onChange={(e) =>
                update("defaultMode", e.target.value === "quality" ? "quality" : "fast")
              }
            >
              <option value="fast">快速</option>
              <option value="quality">质量(深度研究)</option>
            </select>
          </label>
          <label className="settings-input">
            <span>默认页范围</span>
            <select
              value={draft.defaultPageWindow}
              onChange={(e) => update("defaultPageWindow", e.target.value)}
            >
              <option value="0">仅当前页</option>
              <option value="1">当前页 ±1</option>
              <option value="2">当前页 ±2</option>
            </select>
          </label>
          <label className="settings-input">
            <span>流式输出速度</span>
            <select
              value={draft.streamSpeed}
              onChange={(e) =>
                update(
                  "streamSpeed",
                  e.target.value === "slow"
                    ? "slow"
                    : e.target.value === "fast"
                      ? "fast"
                      : "standard",
                )
              }
            >
              <option value="slow">慢</option>
              <option value="standard">标准(默认)</option>
              <option value="fast">快</option>
            </select>
          </label>
        </div>
        <p className="settings-hint">
          输出速度只改变已到达文字的呈现节奏,不改变生成过程、内容或顺序。
        </p>
        <div className="settings-row">
          <label className="settings-check">
            <input
              type="checkbox"
              checked={draft.showRawReasoning}
              onChange={(e) => update("showRawReasoning", e.target.checked)}
            />
            显示模型原始推理(关闭后仍显示真实工具/阶段轨迹)
          </label>
        </div>
        <div className="settings-row">
          <label className="settings-check">
            <input
              type="checkbox"
              checked={draft.saveReasoning}
              onChange={(e) => update("saveReasoning", e.target.checked)}
            />
            保存思考记录(默认关闭;开启后可在隐私清除中删除)
          </label>
        </div>
        <div className="settings-row">
          <label className="settings-check">
            <input
              type="checkbox"
              checked={draft.autoOcr}
              onChange={(e) => update("autoOcr", e.target.checked)}
            />
            自动 OCR(默认开启):解析时和解析完成后自动识别无文本层/缺失页;只用本地 Tesseract,可暂停/继续/取消
          </label>
        </div>
        <div className="settings-row">
          <label className="settings-input">
            <span>OCR 语言(离线)</span>
            <input
              value={draft.ocrLanguages}
              onChange={(e) => update("ocrLanguages", e.target.value)}
              placeholder="chi_sim+eng"
            />
          </label>
          <label className="settings-input">
            <span>翻译目标语言</span>
            <input
              value={draft.translationTargetLanguage}
              onChange={(e) => update("translationTargetLanguage", e.target.value)}
              placeholder="zh-CN"
            />
          </label>
        </div>
        <div className="settings-row">
          <label className="settings-check">
            <input
              type="checkbox"
              checked={draft.showCostEstimate}
              onChange={(e) => update("showCostEstimate", e.target.checked)}
            />
            显示成本/时间预估
          </label>
        </div>
        <div className="settings-row">
          <label className="settings-check">
            <input
              type="checkbox"
              checked={draft.allowExternalTranslationEngine}
              onChange={(e) => update("allowExternalTranslationEngine", e.target.checked)}
            />
            允许外部 PDF 翻译引擎(默认关闭;AGPL 工具需用户自行安装,应用不打包)
          </label>
        </div>
        <div className="settings-row">
          <label className="settings-input grow">
            <span>翻译术语表(每行 source=target;强制命中)</span>
            <textarea
              rows={4}
              value={draft.translationGlossary}
              onChange={(e) => update("translationGlossary", e.target.value)}
              placeholder="source=target (one per line)"
            />
          </label>
        </div>
        <div className="settings-row">
          <input
            ref={glossaryFileRef}
            type="file"
            accept=".csv,.txt,text/csv,text/plain"
            className="sr-only"
            aria-label="导入术语表文件"
            onChange={(event) => {
              const file = event.target.files?.[0];
              if (file) void importGlossary(file);
            }}
          />
          <button
            type="button"
            className="btn small"
            onClick={() => glossaryFileRef.current?.click()}
          >
            导入术语表
          </button>
          <button
            type="button"
            className="btn small"
            onClick={exportGlossary}
            disabled={!draft.translationGlossary.includes("=")}
          >
            导出 CSV
          </button>
          <span className="settings-hint">
            导入为草稿;点击“保存应用设置”后才生效。
          </span>
        </div>
      </div>
      <div className="settings-row">
        <button type="button" className="btn primary" onClick={() => void save()} disabled={saving}>
          {saving ? "保存中…" : "保存应用设置"}
        </button>
      </div>
    </div>
  );
}
