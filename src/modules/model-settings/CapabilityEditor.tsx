// 2F 长上下文:模型能力(上下文窗口/最大输出)查看与手动修正。
//
// 为什么需要:预算按模型真实窗口计算;自动探测只能按模型名猜,
// 用户必须能纠正(否则长文档会被错误截断或超窗)。
// 交互约定:
// - 默认展示当前生效值 + 来源(auto/manual);
// - 手动修正后来源变 manual,自动探测不再覆盖;
// - 非法值(窗口过小/输出不小于窗口)由后端拒绝,前端只做输入约束与提示。
import { useEffect, useState, type ReactNode } from "react";
import type { CommandClient } from "../../app/api/client";
import type { ModelCapabilityView } from "../../shared/generated/ModelCapabilityView";
import { ApiError } from "../../app/api/client";
import { describeAppError } from "../../app/api/errors";

export function CapabilityEditor({
  client,
  providerId,
  modelId,
  onError,
}: {
  client: CommandClient;
  providerId: string;
  modelId: string;
  onError: (message: string) => void;
}): ReactNode {
  const [view, setView] = useState<ModelCapabilityView | null>(null);
  const [contextWindow, setContextWindow] = useState("");
  const [maxOutput, setMaxOutput] = useState("");
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const next = await client.call<ModelCapabilityView>("model_capability_get", {
          providerId,
          modelId,
        });
        if (cancelled) return;
        setView(next);
        setContextWindow(String(next.contextWindow));
        setMaxOutput(String(next.maxOutput));
      } catch {
        if (!cancelled) setView(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, providerId, modelId]);

  async function save(): Promise<void> {
    const window = Number(contextWindow);
    const output = Number(maxOutput);
    if (!Number.isFinite(window) || !Number.isFinite(output)) {
      setStatus("请填写数字");
      return;
    }
    setBusy(true);
    setStatus(null);
    try {
      const saved = await client.call<ModelCapabilityView>("model_capability_set", {
        providerId,
        modelId,
        contextWindow: Math.trunc(window),
        maxOutput: Math.trunc(output),
        toolCalling: view?.toolCalling ?? false,
        vision: view?.vision ?? false,
        jsonSchema: view?.jsonSchema ?? false,
      });
      setView(saved);
      setStatus("已保存(手动修正优先于自动探测)");
    } catch (caught) {
      if (caught instanceof ApiError) {
        setStatus(describeAppError(caught.appError).title);
      } else {
        onError("保存模型能力失败,请重试");
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="capability-editor">
      <div className="capability-head">
        <span className="capability-title">上下文窗口</span>
        {view ? (
          <span className={`capability-source source-${view.source}`}>
            {view.source === "manual" ? "手动修正" : "自动探测"}
          </span>
        ) : null}
      </div>
      <div className="capability-fields">
        <label>
          <span>窗口(tokens)</span>
          <input
            type="number"
            min={2048}
            step={1024}
            value={contextWindow}
            aria-label="上下文窗口 tokens"
            onChange={(event) => setContextWindow(event.target.value)}
          />
        </label>
        <label>
          <span>最大输出(tokens)</span>
          <input
            type="number"
            min={256}
            step={256}
            value={maxOutput}
            aria-label="最大输出 tokens"
            onChange={(event) => setMaxOutput(event.target.value)}
          />
        </label>
        <button type="button" className="btn small" disabled={busy} onClick={() => void save()}>
          {busy ? "保存中…" : "保存"}
        </button>
      </div>
      {view ? (
        <p className="capability-flags">
          {[
            view.streaming ? "流式" : null,
            view.reasoning ? "推理" : null,
            view.toolCalling ? "工具调用" : null,
            view.vision ? "视觉" : null,
            view.jsonSchema ? "JSON Schema" : null,
          ]
            .filter(Boolean)
            .join(" · ") || "未声明额外能力"}
        </p>
      ) : null}
      {status ? (
        <p className="capability-status" role="status">
          {status}
        </p>
      ) : null}
    </div>
  );
}
