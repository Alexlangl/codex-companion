import { Hammer, LoaderCircle, Search } from "lucide-react";
import { useEffect, useState } from "react";
import { Button, Field, Panel } from "../../components/ui";
import { currentApplication } from "../../lib/current-application";
import { compactPath } from "../../lib/format";
import type { CompanionStatus, RepairOutcome } from "../../types/domain";

export function Repair({
  status,
  outcome,
  onRepair,
}: {
  status: CompanionStatus;
  outcome: RepairOutcome | null;
  onRepair: (
    history: boolean,
    plugins: boolean,
    dryRun: boolean,
    codexDir?: string,
    targetProviderId?: string,
  ) => Promise<void>;
}) {
  const [codexDir, setCodexDir] = useState(status.codex.codexDir);
  const [history, setHistory] = useState(true);
  const [plugins, setPlugins] = useState(true);
  const [pendingMode, setPendingMode] = useState<"dry-run" | "repair" | null>(null);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const repairing = pendingMode !== null;
  const application = currentApplication(status);
  const modelProvider = status.codex.modelProvider?.trim();
  const currentProviderId =
    application.kind === "provider"
      ? application.provider.kind === "official_codex" && application.launchMode === "direct"
        ? "openai"
        : application.provider.id
      : modelProvider && modelProvider !== "codex-companion"
        ? modelProvider
      : "codex-companion";
  const currentProviderName =
    application.kind === "provider"
      ? application.name
      : modelProvider && modelProvider !== "codex-companion"
        ? `Codex 当前 provider（${modelProvider}）`
      : "本地代理（分组 / 账号代理）";
  const resultPrefix = outcome?.plan.dryRun ? "预计" : "已修复";
  const historyFiles = outcome?.plan.dryRun ? outcome.plan.historyFiles : outcome?.migratedHistoryFiles;
  const historyLines = outcome?.plan.dryRun ? outcome.plan.historyLines : outcome?.migratedHistoryLines;
  const pluginFiles = outcome?.plan.dryRun ? outcome.plan.pluginFiles : outcome?.migratedPluginFiles;
  const stateRows = outcome?.plan.dryRun ? outcome.plan.stateRows : outcome?.migratedStateRows;
  const pendingLabel = pendingMode === "dry-run" ? "Dry-run" : "修复";

  useEffect(() => {
    if (!repairing || !startedAt) {
      setElapsedSeconds(0);
      return undefined;
    }
    const updateElapsed = () => setElapsedSeconds(Math.max(0, Math.floor((Date.now() - startedAt) / 1000)));
    updateElapsed();
    const timer = window.setInterval(updateElapsed, 1000);
    return () => window.clearInterval(timer);
  }, [repairing, startedAt]);

  async function runRepair(dryRun: boolean) {
    setPendingMode(dryRun ? "dry-run" : "repair");
    setStartedAt(Date.now());
    try {
      await onRepair(history, plugins, dryRun, codexDir, currentProviderId);
    } finally {
      setPendingMode(null);
      setStartedAt(null);
    }
  }

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
          <Button disabled={repairing} onClick={() => void runRepair(true)} variant="secondary">
            {repairing && pendingMode === "dry-run" ? <LoaderCircle className="spin-icon" size={15} /> : <Search size={15} />}
            {repairing && pendingMode === "dry-run" ? "扫描中" : "Dry-run"}
          </Button>
          <Button disabled={repairing} onClick={() => void runRepair(false)}>
            {repairing && pendingMode === "repair" ? <LoaderCircle className="spin-icon" size={15} /> : <Hammer size={15} />}
            {repairing && pendingMode === "repair" ? "修复中" : "执行修复"}
          </Button>
        </div>
        {repairing ? (
          <div className="repair-status" aria-live="polite">
            <div className="repair-status-head">
              <LoaderCircle className="spin-icon" size={16} />
              <strong>{pendingLabel} 进行中</strong>
              <span>{elapsedSeconds}s</span>
            </div>
            <div className="repair-status-bar" />
            <p>正在扫描 Codex 历史、插件状态和 SQLite，完成后会自动刷新结果。</p>
          </div>
        ) : null}
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
