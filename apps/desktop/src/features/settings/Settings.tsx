import * as Select from "@radix-ui/react-select";
import { Cable, RotateCcw } from "lucide-react";
import { Button, Field, Panel } from "../../components/ui";
import { compactPath } from "../../lib/format";
import type { BusyState, CompanionStatus, ThemeMode } from "../../types/domain";

export function Settings({
  busy,
  status,
  onInstall,
  onResetPreferences,
  onUninstall,
  onTheme,
}: {
  busy: BusyState;
  status: CompanionStatus;
  onInstall: () => Promise<void>;
  onUninstall: () => Promise<void>;
  onTheme: (theme: ThemeMode) => Promise<void>;
  onResetPreferences: () => Promise<void>;
}) {
  const disabled = busy !== "idle";

  return (
    <div className="content-grid">
      <Panel eyebrow="Codex" title="启动配置">
        <dl className="details-grid">
          <dt>Codex 目录</dt>
          <dd>{compactPath(status.codex.codexDir)}</dd>
          <dt>配置</dt>
          <dd>{compactPath(status.codex.configPath)}</dd>
          <dt>Base URL</dt>
          <dd>{status.codex.companionBaseUrl}</dd>
          <dt>状态</dt>
          <dd>{status.codex.message}</dd>
        </dl>
        <div className="actions">
          <Button disabled={disabled} onClick={() => void onInstall()}>
            <Cable size={15} /> 写入 Codex 配置
          </Button>
          <Button disabled={disabled} onClick={() => void onUninstall()} variant="secondary">
            <RotateCcw size={15} /> 恢复原配置
          </Button>
        </div>
      </Panel>

      <Panel eyebrow="应用" title="偏好设置">
        <Field label="主题">
          <Select.Root value={status.config.app.theme} onValueChange={(theme) => void onTheme(theme as ThemeMode)}>
            <Select.Trigger className="select-trigger">
              <Select.Value />
            </Select.Trigger>
            <Select.Portal>
              <Select.Content className="select-content">
                <Select.Item className="select-item" value="system">
                  <Select.ItemText>跟随系统</Select.ItemText>
                </Select.Item>
                <Select.Item className="select-item" value="light">
                  <Select.ItemText>亮色</Select.ItemText>
                </Select.Item>
                <Select.Item className="select-item" value="dark">
                  <Select.ItemText>暗色</Select.ItemText>
                </Select.Item>
              </Select.Content>
            </Select.Portal>
          </Select.Root>
        </Field>
        <dl className="details-grid details-top">
          <dt>账号展示</dt>
          <dd>{status.config.app.providerViewMode === "cards" ? "卡片" : "紧凑"}</dd>
        </dl>
        <div className="actions">
          <Button disabled={disabled} onClick={() => void onResetPreferences()} variant="secondary">
            <RotateCcw size={15} /> 恢复界面默认
          </Button>
        </div>
        <dl className="details-grid details-top">
          <dt>数据目录</dt>
          <dd>{compactPath(status.dataDir)}</dd>
          <dt>配置</dt>
          <dd>{compactPath(status.configPath)}</dd>
        </dl>
      </Panel>
    </div>
  );
}
