// M13 拥有:产物 Renderer 解析与未知 kind 兜底(《M13》§6)。未知 kind
// 显示“该版本应用不支持此产物”,保留原始 JSON 只读查看与复制导出
// 入口,不丢弃数据;复制经剪贴板能力并给成功/失败反馈(《M13》§8)。
import { useState } from "react";
import type { ReactElement } from "react";
import type { WriteClipboardTextFn } from "../../app/ipc/transport";
import type { ModuleRegistry } from "../../app/modules/registry";

export function UnknownArtifact({
  kind,
  contentJson,
  writeClipboard,
}: {
  kind: string;
  contentJson: string;
  writeClipboard: WriteClipboardTextFn;
}) {
  const [copyResult, setCopyResult] = useState<string | null>(null);

  async function copyRaw() {
    try {
      await writeClipboard(contentJson);
      setCopyResult("已复制原始 JSON");
    } catch {
      setCopyResult("复制失败:剪贴板不可用");
    }
  }

  return (
    <div className="unknown-artifact" role="region" aria-label={`不支持的产物类型:${kind}`}>
      <p role="status">该版本应用不支持此产物类型({kind});数据已保留。</p>
      <details>
        <summary>查看原始 JSON(只读)</summary>
        <pre tabIndex={0} aria-label="产物原始 JSON">
          {contentJson}
        </pre>
      </details>
      <div>
        <button type="button" onClick={copyRaw}>
          复制原始 JSON
        </button>
        {copyResult ? (
          <span role="status" className="copy-result">
            {copyResult}
          </span>
        ) : null}
      </div>
    </div>
  );
}

/** 渲染一个产物版本:已注册 kind 用注册组件,未知 kind 走兜底。 */
export function renderArtifactVersion(options: {
  registry: ModuleRegistry;
  kind: string;
  artifactId: string;
  versionId: string;
  contentJson: string;
  writeClipboard: WriteClipboardTextFn;
}): ReactElement {
  const registration = options.registry.rendererFor(options.kind);
  if (!registration) {
    return (
      <UnknownArtifact
        kind={options.kind}
        contentJson={options.contentJson}
        writeClipboard={options.writeClipboard}
      />
    );
  }
  const Component = registration.component;
  return <Component artifactId={options.artifactId} versionId={options.versionId} />;
}
