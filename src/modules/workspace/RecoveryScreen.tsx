// M13 拥有:启动失败恢复页(《M13》§2)。数据库损坏、迁移失败或配置
// 不可解析时进入本页:不写业务状态、不删任何文件,迁移前的备份与
// 原文件都保留;提供可操作指引而非笼统报错。
import { useEffect, useRef } from "react";
import type { AppError } from "../../shared/generated/AppError";
import type { ShellStatusView } from "../../shared/generated/ShellStatusView";
import { describeAppError } from "../../app/api/errors";
import { handleTrapKey } from "./focusTrap";

const CODE_GUIDANCE: Record<string, string[]> = {
  DB_CORRUPT: [
    "本地数据库未能通过完整性检查,应用已停止写入以保护数据。",
    "已保存的笔记、标注与产物保留在数据库文件和迁移前备份中(backups 目录)。",
    "可以关闭应用后重试启动;若反复失败,请备份应用数据目录后再联系支持。",
  ],
  DB_MIGRATION_FAILED: [
    "数据库升级未完成。迁移开始前的完整备份已保留在应用数据目录的 backups 下。",
    "请勿手动修改数据库文件;重试启动会从备份继续,不会丢失已保存数据。",
  ],
  SETTINGS_INVALID: [
    "设置文件(settings.json)无法解析,可能被外部工具修改。",
    "应用不会用默认值覆盖它。可关闭应用、修复或移走该文件后重试启动。",
  ],
};

function guidanceFor(error: AppError): string[] {
  return CODE_GUIDANCE[error.code] ?? [
    "启动过程遇到意外错误。已保存数据未受影响。",
    "可以重试启动;若持续失败,请备份应用数据目录后联系支持。",
  ];
}

export function RecoveryScreen({
  status,
  onRetry,
}: {
  status: ShellStatusView;
  onRetry?: () => void;
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const error = status.error ?? null;
  const presentation = error ? describeAppError(error) : null;

  useEffect(() => {
    dialogRef.current?.focus();
  }, []);

  return (
    <div className="recovery-backdrop" role="presentation">
      <div
        ref={dialogRef}
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="recovery-title"
        aria-describedby="recovery-desc"
        tabIndex={-1}
        className="recovery-dialog"
        onKeyDown={(e) => handleTrapKey(dialogRef.current ?? e.currentTarget, e)}
      >
        <h1 id="recovery-title" className="recovery-title">
          Knowwise 需要恢复
        </h1>
        <p id="recovery-desc">
          {presentation
            ? `${presentation.title}${presentation.detailLines.length > 0 ? `(${presentation.detailLines.join(";")})` : ""}`
            : "启动失败,原因未知。"}
        </p>
        <ul className="recovery-guidance">
          {guidanceFor(
            error ?? {
              schemaVersion: "error.v1",
              code: "INTERNAL",
              messageKey: "error.internal",
              retryable: false,
              userAction: "none",
              details: null,
              traceId: "none",
            },
          ).map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
        <div className="recovery-actions">
          <button type="button" onClick={onRetry} autoFocus={false}>
            重试启动
          </button>
          <span className="recovery-hint">按 Tab 在本对话框内循环焦点</span>
        </div>
      </div>
    </div>
  );
}
