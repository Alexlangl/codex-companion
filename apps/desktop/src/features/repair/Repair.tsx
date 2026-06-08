import { Hammer, Search } from "lucide-react";
import { useState } from "react";
import { Button, Field, Panel } from "../../components/ui";
import { compactPath } from "../../lib/format";
import type { BusyState, CompanionStatus, RepairOutcome } from "../../types/domain";

export function Repair({
  busy,
  status,
  outcome,
  onRepair,
}: {
  busy: BusyState;
  status: CompanionStatus;
  outcome: RepairOutcome | null;
  onRepair: (
    history: boolean,
    plugins: boolean,
    dryRun: boolean,
    codexDir?: string,
  ) => Promise<void>;
}) {
  const [codexDir, setCodexDir] = useState(status.codex.codexDir);
  const [history, setHistory] = useState(true);
  const [plugins, setPlugins] = useState(true);
  const disabled = busy !== "idle";
  const currentProviderId = status.codex.modelProvider || "codex-companion";
  const currentProviderName =
    currentProviderId === "codex-companion"
      ? "Codex Companion（分组 / 本地代理）"
      : (status.config.providers[currentProviderId]?.name ?? currentProviderId);
  const resultPrefix = outcome?.plan.dryRun ? "预计" : "已修复";
  const historyFiles = outcome?.plan.dryRun ? outcome.plan.historyFiles : outcome?.migratedHistoryFiles;
  const historyLines = outcome?.plan.dryRun ? outcome.plan.historyLines : outcome?.migratedHistoryLines;
  const pluginFiles = outcome?.plan.dryRun ? outcome.plan.pluginFiles : outcome?.migratedPluginFiles;
  const stateRows = outcome?.plan.dryRun ? outcome.plan.stateRows : outcome?.migratedStateRows;

  return (
    <div className="content-grid">
      <Panel eyebrow="修复" title="状态修复">
        <Field label="Codex 目录">
          <input value={codexDir} onChange={(event) => setCodexDir(event.target.value)} />
        </Field>
        <dl className="details-grid details-top">
          <dt>当前归属</dt>
          <dd>{currentProviderName}</dd>
          <dt>目标 namespace</dt>
          <dd>{currentProviderId}</dd>
        </dl>
        <label className="check-row">
          <input checked={history} onChange={(event) => setHistory(event.target.checked)} type="checkbox" />
          <span>历史会话</span>
        </label>
        <label className="check-row">
          <input checked={plugins} onChange={(event) => setPlugins(event.target.checked)} type="checkbox" />
          <span>插件状态</span>
        </label>
        <div className="actions">
          <Button disabled={disabled} onClick={() => void onRepair(history, plugins, true, codexDir)} variant="secondary">
            <Search size={15} /> Dry-run
          </Button>
          <Button disabled={disabled} onClick={() => void onRepair(history, plugins, false, codexDir)}>
            <Hammer size={15} /> 执行修复
          </Button>
        </div>
      </Panel>

      <Panel eyebrow="结果" title="修复结果">
        {outcome ? (
          <dl className="details-grid">
            <dt>来源</dt>
            <dd>{outcome.plan.sourceProviderIds.join(", ") || "无"}</dd>
            <dt>历史</dt>
            <dd>{resultPrefix} {historyFiles} 个文件 / {historyLines} 行</dd>
            <dt>插件</dt>
            <dd>{resultPrefix} {pluginFiles} 个文件</dd>
            <dt>SQLite</dt>
            <dd>{resultPrefix} {stateRows} 行</dd>
            <dt>备份</dt>
            <dd>{outcome.backupRoot ? compactPath(outcome.backupRoot) : "无"}</dd>
            <dt>备注</dt>
            <dd>{outcome.skippedReason ?? (outcome.plan.dryRun ? "预览未写入" : "完成")}</dd>
          </dl>
        ) : (
          <p className="empty">先执行 dry-run，可以预览 namespace 修复计划。</p>
        )}
      </Panel>
    </div>
  );
}
