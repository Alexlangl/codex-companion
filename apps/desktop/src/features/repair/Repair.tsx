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
    targetProviderId?: string,
  ) => Promise<void>;
}) {
  const [codexDir, setCodexDir] = useState(status.codex.codexDir);
  const [history, setHistory] = useState(true);
  const [plugins, setPlugins] = useState(true);
  const [targetProviderId, setTargetProviderId] = useState(status.codex.modelProvider || "codex-companion");
  const disabled = busy !== "idle";
  const providerOptions = [
    { id: "codex-companion", label: "Codex Companion（分组 / 本地代理）" },
    ...Object.values(status.config.providers).map((provider) => ({
      id: provider.id,
      label: `${provider.name}（单 Provider 直连）`,
    })),
  ].filter((option, index, options) => options.findIndex((item) => item.id === option.id) === index);

  return (
    <div className="content-grid">
      <Panel eyebrow="修复" title="状态修复">
        <Field label="Codex 目录">
          <input value={codexDir} onChange={(event) => setCodexDir(event.target.value)} />
        </Field>
        <Field label="修复目标">
          <select
            className="select-trigger"
            onChange={(event) => setTargetProviderId(event.currentTarget.value)}
            value={targetProviderId}
          >
            {providerOptions.map((option) => (
              <option key={option.id} value={option.id}>
                {option.label}
              </option>
            ))}
          </select>
        </Field>
        <label className="check-row">
          <input checked={history} onChange={(event) => setHistory(event.target.checked)} type="checkbox" />
          <span>历史会话</span>
        </label>
        <label className="check-row">
          <input checked={plugins} onChange={(event) => setPlugins(event.target.checked)} type="checkbox" />
          <span>插件状态</span>
        </label>
        <div className="actions">
          <Button disabled={disabled} onClick={() => void onRepair(history, plugins, true, codexDir, targetProviderId)} variant="secondary">
            <Search size={15} /> Dry-run
          </Button>
          <Button disabled={disabled} onClick={() => void onRepair(history, plugins, false, codexDir, targetProviderId)}>
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
            <dd>{outcome.migratedHistoryFiles} 个文件 / {outcome.migratedHistoryLines} 行</dd>
            <dt>插件</dt>
            <dd>{outcome.migratedPluginFiles} 个文件</dd>
            <dt>SQLite</dt>
            <dd>{outcome.migratedStateRows} 行</dd>
            <dt>备份</dt>
            <dd>{outcome.backupRoot ? compactPath(outcome.backupRoot) : "无"}</dd>
            <dt>备注</dt>
            <dd>{outcome.skippedReason ?? "完成"}</dd>
          </dl>
        ) : (
          <p className="empty">先执行 dry-run，可以预览 namespace 修复计划。</p>
        )}
      </Panel>
    </div>
  );
}
