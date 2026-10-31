// 主阅读区的「资料阅览」:在阅读器位置打开 AI 产物,像文档一样滚动阅读。
//
// 为什么放在主区而不是右栏:右栏是固定高度的窄面板,长产物在里面只能
// 反复内滚,用户既看不到整体也容易丢位置;放进主区后可与文档一样整篇
// 滚动、复制、导出,返回即回到原来的文档阅读位置。
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { Download, X } from "lucide-react";
import type { ArtifactView } from "../../shared/generated/ArtifactView";
import type { ArtifactVersionView } from "../../shared/generated/ArtifactVersionView";
import { useServices } from "../../app/services";
import { useWorkspaceUi, type ArtifactReading } from "../workspace/uiStore";
import { ArtifactReader } from "./ArtifactReader";
import "./artifactReadingPane.css";

export function ArtifactReadingPane({ reading }: { reading: ArtifactReading }): ReactNode {
  const { client } = useServices();
  const queryClient = useQueryClient();
  const closeArtifactReading = useWorkspaceUi((s) => s.closeArtifactReading);
  const setDockTab = useWorkspaceUi((s) => s.setDockTab);

  const detail = useQuery({
    queryKey: ["artifact", reading.artifactId],
    queryFn: () =>
      client.callFlat<ArtifactView>("artifact_get", { artifactId: reading.artifactId }),
    staleTime: 10_000,
  });

  const artifact = detail.data;
  const versions: ArtifactVersionView[] =
    artifact === undefined
      ? []
      : artifact.versions.length > 0
        ? artifact.versions
        : artifact.currentVersion !== undefined
          ? [artifact.currentVersion]
          : [];
  const version =
    versions.find((item) => item.id === reading.versionId) ?? artifact?.currentVersion;

  return (
    <div className="artifact-reading-pane">
      <div className="artifact-reading-bar" role="toolbar" aria-label="资料阅览操作">
        <span className="artifact-reading-caption">资料阅览 · AI 草稿,不是原文事实</span>
        <span className="artifact-reading-spacer" />
        <button
          type="button"
          className="btn tiny ghost"
          onClick={() => setDockTab("artifacts")}
        >
          <Download className="w-3.5 h-3.5" aria-hidden="true" />
          <span>导出/详情</span>
        </button>
        <button
          type="button"
          className="btn tiny ghost"
          onClick={() => {
            // 关闭阅览不刷新文档位置:文档阅读态与滚动位置保持原样。
            closeArtifactReading();
            void queryClient.invalidateQueries({ queryKey: ["artifacts"] });
          }}
          aria-label="返回文档"
        >
          <X className="w-3.5 h-3.5" aria-hidden="true" />
          <span>返回文档</span>
        </button>
      </div>
      {detail.isLoading ? (
        <p className="artifact-reading-state" role="status">
          资料加载中…
        </p>
      ) : null}
      {detail.isError ? (
        <div className="artifact-reading-state" role="alert">
          <p>资料读取失败;产物可能已被删除。</p>
          <button type="button" className="btn tiny" onClick={() => void detail.refetch()}>
            重试
          </button>
        </div>
      ) : null}
      {artifact !== undefined && version === undefined ? (
        <p className="artifact-reading-state" role="alert">
          该资料没有可读取的版本。
        </p>
      ) : null}
      {artifact !== undefined && version !== undefined ? (
        <div className="artifact-reading-scroll">
          <ArtifactReader
            artifact={artifact}
            version={version}
            client={client}
            onSaved={() => {
              void queryClient.invalidateQueries({ queryKey: ["artifact", reading.artifactId] });
              void detail.refetch();
            }}
            onBack={closeArtifactReading}
          />
        </div>
      ) : null}
    </div>
  );
}
