import * as Select from "@radix-ui/react-select";
import {
  Cable,
  Clipboard,
  Download,
  ExternalLink,
  FileText,
  FolderOpen,
  Play,
  RefreshCw,
  RotateCcw,
  ShieldCheck,
  SquareTerminal,
  Trash2,
} from "lucide-react";
import { useEffect, useState, type ChangeEvent } from "react";
import { Badge, Button, Field, Panel } from "../../components/ui";
import {
  clearDiagnosticLogs,
  getDiagnosticInfo,
  launchCli,
  openDiagnosticDirectory,
  previewCliCommand,
} from "../../lib/api";
import { compactPath } from "../../lib/format";
import { userFacingError } from "../../lib/errors";
import type {
  BusyState,
  CompanionStatus,
  DiagnosticInfo,
  TerminalKind,
  ThemeMode,
} from "../../types/domain";
import type { AppUpdateState, AppUpdaterController } from "./useAppUpdater";

type SettingsProps = {
  appUpdater: AppUpdaterController;
  busy: BusyState;
  status: CompanionStatus;
  onInstall: () => Promise<void>;
  onUninstall: () => Promise<void>;
  onPreserveOfficialCodexAuth: (preserve: boolean) => Promise<void>;
  onTheme: (theme: ThemeMode) => Promise<void>;
  onTokenUsageRefreshInterval: (seconds: number) => Promise<void>;
  onResetPreferences: () => Promise<void>;
};

const TERMINAL_OPTIONS = [
  { value: "auto", label: "自动选择" },
  { value: "terminal", label: "Terminal (macOS)" },
  { value: "i_term2", label: "iTerm2 (macOS)" },
  { value: "windows_terminal", label: "Windows Terminal" },
  { value: "power_shell", label: "Windows PowerShell" },
  { value: "pwsh", label: "PowerShell 7" },
  { value: "cmd", label: "Command Prompt" },
  { value: "shell", label: "Linux 终端" },
] as const satisfies ReadonlyArray<{ value: TerminalKind; label: string }>;

const TOKEN_REFRESH_OPTIONS = [
  { value: 0, label: "关闭自动刷新" },
  { value: 15, label: "每 15 秒" },
  { value: 30, label: "每 30 秒" },
  { value: 60, label: "每 1 分钟" },
  { value: 300, label: "每 5 分钟" },
] as const;

export function Settings(props: SettingsProps) {
  const {
    appUpdater,
    busy,
    status,
    onInstall,
    onPreserveOfficialCodexAuth,
    onResetPreferences,
    onTheme,
    onTokenUsageRefreshInterval,
    onUninstall,
  } = props;
  const [diagnosticInfo, setDiagnosticInfo] = useState<DiagnosticInfo | null>(null);
  const [diagnosticBusy, setDiagnosticBusy] = useState(false);
  const [diagnosticMessage, setDiagnosticMessage] = useState("");
  const [workingDirectory, setWorkingDirectory] = useState(
    () => status.config.app.recentWorkingDirectories[0] ?? status.codex.codexDir,
  );
  const [terminal, setTerminal] = useState<TerminalKind>(status.config.app.preferredTerminal);
  const [cliPreview, setCliPreview] = useState("");
  const [cliMessage, setCliMessage] = useState("");
  const [cliBusy, setCliBusy] = useState(false);
  const disabled = busy !== "idle";
  const update = appUpdater.state;
  const updateBusy = ["checking", "downloading", "installing"].includes(update.status);
  const updateStatus = appUpdateStatusLabel(update);

  useEffect(() => {
    void loadDiagnostics();
  }, []);

  useEffect(() => {
    const directory = workingDirectory.trim();
    if (!directory) {
      setCliPreview("");
      setCliMessage("");
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(async () => {
      try {
        const command = await previewCliCommand({ workingDirectory: directory, terminal });
        if (!cancelled) {
          setCliPreview(command);
          setCliMessage("");
        }
      } catch (unknownError) {
        if (!cancelled) {
          setCliPreview("");
          setCliMessage(userFacingError(unknownError));
        }
      }
    }, 220);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [terminal, workingDirectory]);

  async function loadDiagnostics(): Promise<void> {
    setDiagnosticBusy(true);
    try {
      setDiagnosticInfo(await getDiagnosticInfo());
      setDiagnosticMessage("");
    } catch (unknownError) {
      setDiagnosticMessage(userFacingError(unknownError));
    } finally {
      setDiagnosticBusy(false);
    }
  }

  function handleCheckForUpdates(): void {
    void appUpdater.checkForUpdates();
  }

  function handleInstallUpdate(): void {
    void appUpdater.installUpdate();
  }

  function handleOpenUpdateDownload(): void {
    if (update.status !== "available" && update.status !== "install-error") {
      return;
    }
    void appUpdater.openDownloadUrl(update.downloadUrl);
  }

  function handleRestartUpdate(): void {
    void appUpdater.restartApp();
  }

  function handleWorkingDirectoryChange(event: ChangeEvent<HTMLInputElement>): void {
    setWorkingDirectory(event.target.value);
  }

  function handleTerminalChange(event: ChangeEvent<HTMLSelectElement>): void {
    setTerminal(event.target.value as TerminalKind);
  }

  function handleTokenRefreshChange(event: ChangeEvent<HTMLSelectElement>): void {
    void onTokenUsageRefreshInterval(Number(event.target.value));
  }

  async function handleLaunchCli(): Promise<void> {
    setCliBusy(true);
    setCliMessage("");
    try {
      const outcome = await launchCli({
        workingDirectory: workingDirectory.trim(),
        terminal,
      });
      setCliPreview(outcome.command);
      setCliMessage(outcome.message);
    } catch (unknownError) {
      setCliMessage(userFacingError(unknownError));
    } finally {
      setCliBusy(false);
    }
  }

  async function handleCopyCliCommand(): Promise<void> {
    if (!cliPreview) return;
    try {
      await navigator.clipboard.writeText(cliPreview);
      setCliMessage("命令已复制");
    } catch (unknownError) {
      setCliMessage(userFacingError(unknownError));
    }
  }

  async function handleOpenDiagnosticDirectory(): Promise<void> {
    setDiagnosticBusy(true);
    try {
      const opened = await openDiagnosticDirectory();
      setDiagnosticMessage(opened ? "已打开诊断日志目录" : "当前环境无法自动打开目录");
    } catch (unknownError) {
      setDiagnosticMessage(userFacingError(unknownError));
    } finally {
      setDiagnosticBusy(false);
    }
  }

  async function handleClearDiagnosticLogs(): Promise<void> {
    const confirmed = window.confirm("确定清空 Companion 诊断日志吗？账号、配置和会话不会被删除。");
    if (!confirmed) return;
    setDiagnosticBusy(true);
    try {
      const removed = await clearDiagnosticLogs();
      setDiagnosticMessage(`已清理 ${removed} 个日志文件`);
      setDiagnosticInfo(await getDiagnosticInfo());
    } catch (unknownError) {
      setDiagnosticMessage(userFacingError(unknownError));
    } finally {
      setDiagnosticBusy(false);
    }
  }

  return (
    <div className="content-grid settings-grid">
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
            <ShieldCheck aria-hidden="true" size={15} /> 切换 Provider 时保留官方登录
          </span>
        </label>
        <p className="field-hint" id="preserve-official-codex-auth-hint">
          第三方 API Key 和本地代理凭据只写入对应 Provider 配置；Companion 不接管 Codex auth.json。
        </p>
        <div className="actions">
          <Button disabled={disabled} onClick={() => void onInstall()}>
            <Cable aria-hidden="true" size={15} /> 写入 Codex 配置
          </Button>
          <Button disabled={disabled} onClick={() => void onUninstall()} variant="secondary">
            <RotateCcw aria-hidden="true" size={15} /> 恢复原配置
          </Button>
        </div>
      </Panel>

      <Panel eyebrow="应用" title="偏好与更新">
        <Field label="主题">
          <Select.Root value={status.config.app.theme} onValueChange={(theme) => void onTheme(theme as ThemeMode)}>
            <Select.Trigger className="select-trigger">
              <Select.Value />
            </Select.Trigger>
            <Select.Portal>
              <Select.Content className="select-content" position="popper" sideOffset={4}>
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
        <Field label="Token 自动刷新">
          <select
            disabled={disabled}
            onChange={handleTokenRefreshChange}
            value={status.config.app.tokenUsageRefreshIntervalSeconds}
          >
            {TOKEN_REFRESH_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>{option.label}</option>
            ))}
          </select>
        </Field>
        <dl className="details-grid details-top">
          <dt>账号展示</dt>
          <dd>{status.config.app.providerViewMode === "cards" ? "卡片" : "紧凑"}</dd>
          <dt>数据目录</dt>
          <dd>
            {compactPath(status.dataDir)} {status.dataRoots.companionIsolated ? <Badge tone="info">隔离</Badge> : null}
          </dd>
          <dt>会话目录</dt>
          <dd>
            {compactPath(status.codex.codexDir)} {status.dataRoots.codexIsolated ? <Badge tone="info">隔离</Badge> : null}
          </dd>
        </dl>
        <div className="actions">
          <Button disabled={disabled} onClick={() => void onResetPreferences()} variant="secondary">
            <RotateCcw aria-hidden="true" size={15} /> 恢复界面默认
          </Button>
        </div>
        <div aria-busy={updateBusy} aria-live="polite" className="settings-update">
          <dl className="details-grid details-top">
            <dt>当前版本</dt>
            <dd>{update.currentVersion || "读取中"}</dd>
            <dt>软件更新</dt>
            <dd>{updateStatus}</dd>
          </dl>
          {update.status === "available" ? <p className="field-hint">{update.notes}</p> : null}
          <div className="actions">
            <Button disabled={disabled || updateBusy} onClick={handleCheckForUpdates} variant="secondary">
              <RefreshCw aria-hidden="true" size={15} /> 检查更新
            </Button>
            {update.status === "available" ? (
              <>
                <Button disabled={disabled || updateBusy} onClick={handleOpenUpdateDownload} variant="secondary">
                  <ExternalLink aria-hidden="true" size={15} /> 打开下载页
                </Button>
                <Button disabled={disabled || updateBusy} onClick={handleInstallUpdate}>
                  <Download aria-hidden="true" size={15} /> 下载并安装 v{update.nextVersion}
                </Button>
              </>
            ) : null}
            {update.status === "install-error" ? (
              <>
                <Button disabled={disabled} onClick={handleOpenUpdateDownload} variant="secondary">
                  <Download aria-hidden="true" size={15} /> 手动下载
                </Button>
                <Button disabled={disabled} onClick={handleInstallUpdate}>
                  <RefreshCw aria-hidden="true" size={15} /> 重试安装
                </Button>
              </>
            ) : null}
            {update.status === "restart-error" ? (
              <Button disabled={disabled} onClick={handleRestartUpdate}>
                <RefreshCw aria-hidden="true" size={15} /> 立即重启
              </Button>
            ) : null}
          </div>
        </div>
      </Panel>

      <Panel eyebrow="CLI" title="在终端启动 Codex">
        <Field label="工作目录">
          <input
            list="recent-working-directories"
            onChange={handleWorkingDirectoryChange}
            placeholder="/path/to/project"
            value={workingDirectory}
          />
        </Field>
        <datalist id="recent-working-directories">
          {status.config.app.recentWorkingDirectories.map((directory) => (
            <option key={directory} value={directory} />
          ))}
        </datalist>
        <Field label="终端">
          <select onChange={handleTerminalChange} value={terminal}>
            {TERMINAL_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>{option.label}</option>
            ))}
          </select>
        </Field>
        <div className="cli-command-preview" aria-live="polite">
          <SquareTerminal aria-hidden="true" size={16} />
          <code>{cliPreview || "输入有效工作目录后生成启动命令"}</code>
        </div>
        {cliMessage ? <p className="field-hint" role="status">{cliMessage}</p> : null}
        <div className="actions">
          <Button disabled={cliBusy || !cliPreview} onClick={() => void handleLaunchCli()}>
            <Play aria-hidden="true" size={15} /> {cliBusy ? "启动中" : "打开终端"}
          </Button>
          <Button disabled={!cliPreview} onClick={() => void handleCopyCliCommand()} variant="secondary">
            <Clipboard aria-hidden="true" size={15} /> 复制命令
          </Button>
        </div>
      </Panel>

      <Panel eyebrow="诊断" title="本地日志">
        <dl className="details-grid diagnostic-details">
          <dt>当前日志</dt>
          <dd>{diagnosticInfo ? compactPath(diagnosticInfo.currentLogPath) : "读取中"}</dd>
          <dt>保留文件</dt>
          <dd>{diagnosticInfo?.retainedFiles ?? 0}</dd>
          <dt>总大小</dt>
          <dd>{formatBytes(diagnosticInfo?.totalBytes ?? 0)}</dd>
        </dl>
        <div className="diagnostic-note">
          <FileText aria-hidden="true" size={16} />
          <span>日志采用 JSONL，写入前会脱敏，并按大小轮转保留。</span>
        </div>
        {diagnosticMessage ? <p className="field-hint" role="status">{diagnosticMessage}</p> : null}
        <div className="actions">
          <Button disabled={diagnosticBusy} onClick={() => void handleOpenDiagnosticDirectory()} variant="secondary">
            <FolderOpen aria-hidden="true" size={15} /> 打开日志目录
          </Button>
          <Button disabled={diagnosticBusy} onClick={() => void loadDiagnostics()} variant="ghost">
            <RefreshCw aria-hidden="true" size={15} /> 刷新信息
          </Button>
          <Button disabled={diagnosticBusy} onClick={() => void handleClearDiagnosticLogs()} variant="danger">
            <Trash2 aria-hidden="true" size={15} /> 清空日志
          </Button>
        </div>
      </Panel>
    </div>
  );
}

function appUpdateStatusLabel(state: AppUpdateState): string {
  switch (state.status) {
    case "loading":
      return "正在读取版本";
    case "unsupported":
      return "浏览器开发模式不执行更新";
    case "checking":
      return state.retryAttempt && state.retryTotal
        ? `网络波动，正在重试检查（${state.retryAttempt}/${state.retryTotal}）`
        : "正在检查更新";
    case "latest":
      return "当前已是最新版本";
    case "available":
      return `发现 v${state.nextVersion}`;
    case "downloading":
      if (state.retryAttempt && state.retryTotal) {
        return `下载遇到网络波动，正在重试（${state.retryAttempt}/${state.retryTotal}）`;
      }
      return state.progress === null
        ? `正在下载 v${state.nextVersion}`
        : `正在下载 v${state.nextVersion}（${state.progress}%）`;
    case "installing":
      return `正在安装 v${state.nextVersion}`;
    case "check-error":
    case "install-error":
    case "restart-error":
      return state.message;
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
