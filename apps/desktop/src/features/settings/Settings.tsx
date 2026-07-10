import * as Select from "@radix-ui/react-select";
import { Cable, RotateCcw, ShieldCheck } from "lucide-react";
import { Button, Field, Panel } from "../../components/ui";
import { compactPath } from "../../lib/format";
import type { BusyState, CompanionStatus, ThemeMode } from "../../types/domain";

export function Settings({
  busy,
  status,
  onInstall,
  onResetPreferences,
  onPreserveOfficialCodexAuth,
  onUninstall,
  onTheme,
}: {
  busy: BusyState;
  status: CompanionStatus;
  onInstall: () => Promise<void>;
  onUninstall: () => Promise<void>;
  onPreserveOfficialCodexAuth: (preserve: boolean) => Promise<void>;
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
        <label className="toggle-row">
          <input
            aria-describedby="preserve-official-codex-auth-hint"
            checked={Boolean(status.config.app.preserveOfficialCodexAuth)}
            disabled={disabled}
            onChange={(event) => void onPreserveOfficialCodexAuth(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>
            <ShieldCheck aria-hidden="true" size={15} /> 保留官方 Codex 登录
          </span>
        </label>
        <p className="field-hint" id="preserve-official-codex-auth-hint">
          开启后，第三方 API key 直连会写入对应 provider 的 experimental_bearer_token，官方 ChatGPT OAuth 继续保留在 auth.json；本地代理仍由 Companion 注入密钥。
        </p>
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
