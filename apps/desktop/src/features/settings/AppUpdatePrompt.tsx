import * as Dialog from "@radix-ui/react-dialog";
import { Download, ExternalLink, RefreshCw, Sparkles, X } from "lucide-react";
import { useRef, useState } from "react";
import { Button } from "../../components/ui";
import type { AppUpdateState, AppUpdaterController } from "./useAppUpdater";

type AvailableUpdate = Extract<AppUpdateState, { status: "available" }>;
type DownloadingUpdate = Extract<AppUpdateState, { status: "downloading" }>;
type InstallingUpdate = Extract<AppUpdateState, { status: "installing" }>;
type CheckError = Extract<AppUpdateState, { status: "check-error" }>;
type InstallError = Extract<AppUpdateState, { status: "install-error" }>;
type RestartError = Extract<AppUpdateState, { status: "restart-error" }>;

export function AppUpdatePrompt({
  isBlocked,
  updater,
}: {
  isBlocked: boolean;
  updater: AppUpdaterController;
}) {
  const [dismissedPrompt, setDismissedPrompt] = useState<string | null>(null);
  const update = updater.state;

  if (update.status === "check-error") {
    if (update.automatic) {
      return null;
    }
    const promptKey = `check-error:${update.errorId}`;
    return (
      <CheckErrorPrompt
        isOpen={!isBlocked && dismissedPrompt !== promptKey}
        onDismiss={() => setDismissedPrompt(promptKey)}
        onCheck={updater.checkForUpdates}
        update={update}
      />
    );
  }

  if (update.status === "available") {
    const promptKey = `available:${update.nextVersion}`;
    const isOpen = !isBlocked && dismissedPrompt !== promptKey;
    return (
      <AvailableUpdatePrompt
        isOpen={isOpen}
        onDismiss={() => setDismissedPrompt(promptKey)}
        onInstall={updater.installUpdate}
        onOpenDownload={() => void updater.openDownloadUrl(update.downloadUrl)}
        update={update}
      />
    );
  }

  if (update.status === "downloading") {
    return <DownloadingUpdatePrompt update={update} />;
  }

  if (update.status === "installing") {
    return <InstallingUpdatePrompt update={update} />;
  }

  if (update.status === "install-error") {
    const promptKey = `install-error:${update.nextVersion}:${update.errorId}`;
    const isOpen = !isBlocked && dismissedPrompt !== promptKey;
    return (
      <InstallErrorPrompt
        isOpen={isOpen}
        onDismiss={() => setDismissedPrompt(promptKey)}
        onOpenDownload={() => void updater.openDownloadUrl(update.downloadUrl)}
        onRetry={updater.installUpdate}
        update={update}
      />
    );
  }

  if (update.status === "restart-error") {
    const promptKey = `restart-error:${update.nextVersion}:${update.errorId}`;
    const isOpen = !isBlocked && dismissedPrompt !== promptKey;
    return (
      <RestartErrorPrompt
        isOpen={isOpen}
        onDismiss={() => setDismissedPrompt(promptKey)}
        onRestart={updater.restartApp}
        update={update}
      />
    );
  }

  return null;
}

function AvailableUpdatePrompt({
  isOpen,
  onDismiss,
  onInstall,
  onOpenDownload,
  update,
}: {
  isOpen: boolean;
  onDismiss: () => void;
  onInstall: () => Promise<void>;
  onOpenDownload: () => void;
  update: AvailableUpdate;
}) {
  const titleRef = useRef<HTMLHeadingElement>(null);

  function handleOpenChange(open: boolean): void {
    if (!open) {
      onDismiss();
    }
  }

  function handleOpenAutoFocus(event: Event): void {
    event.preventDefault();
    titleRef.current?.focus();
  }

  function handleInstallUpdate(): void {
    void onInstall();
  }

  return (
    <Dialog.Root onOpenChange={handleOpenChange} open={isOpen}>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          className="dialog-content app-update-dialog"
          onOpenAutoFocus={handleOpenAutoFocus}
        >
          <div className="dialog-header">
            <div>
              <UpdateEyebrow />
              <Dialog.Title className="dialog-title" ref={titleRef} tabIndex={-1}>
                Codex Companion v{update.nextVersion} 已可用
              </Dialog.Title>
              <Dialog.Description className="dialog-description">
                当前版本 v{update.currentVersion}。更新不会自动下载，请选择是否立即安装。
              </Dialog.Description>
            </div>
            <Dialog.Close aria-label="稍后更新" className="icon-button">
              <X aria-hidden="true" size={16} />
            </Dialog.Close>
          </div>

          <section aria-labelledby="app-update-notes-title" className="app-update-notes">
            <h3 id="app-update-notes-title">版本信息</h3>
            <p>{update.notes}</p>
          </section>

          <p className="app-update-later-hint">选择稍后后，本次运行不再打扰；仍可前往设置手动更新。</p>

          <div className="actions app-update-actions">
            <Dialog.Close asChild>
              <Button variant="secondary">稍后</Button>
            </Dialog.Close>
            <Button onClick={onOpenDownload} variant="secondary">
              <ExternalLink aria-hidden="true" size={15} /> 打开下载页
            </Button>
            <Button onClick={handleInstallUpdate}>
              <Download aria-hidden="true" size={15} /> 立即更新
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function DownloadingUpdatePrompt({ update }: { update: DownloadingUpdate }) {
  const titleRef = useRef<HTMLHeadingElement>(null);
  const progressLabel = getDownloadProgressLabel(update);

  function handleOpenAutoFocus(event: Event): void {
    event.preventDefault();
    titleRef.current?.focus();
  }

  return (
    <Dialog.Root open>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          className="dialog-content app-update-dialog"
          onEscapeKeyDown={(event) => event.preventDefault()}
          onInteractOutside={(event) => event.preventDefault()}
          onOpenAutoFocus={handleOpenAutoFocus}
        >
          <div className="dialog-header">
            <div>
              <UpdateEyebrow />
              <Dialog.Title className="dialog-title" ref={titleRef} tabIndex={-1}>
                正在下载 v{update.nextVersion}
              </Dialog.Title>
              <Dialog.Description className="dialog-description">
                请保持应用运行，下载和签名校验完成后将进入安装。
              </Dialog.Description>
            </div>
          </div>

          <div aria-live="polite" className="app-update-progress">
            <progress aria-label={progressLabel} max={100} value={update.progress ?? undefined} />
            <span>{progressLabel}</span>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function InstallingUpdatePrompt({ update }: { update: InstallingUpdate }) {
  const titleRef = useRef<HTMLHeadingElement>(null);

  function handleOpenAutoFocus(event: Event): void {
    event.preventDefault();
    titleRef.current?.focus();
  }

  return (
    <Dialog.Root open>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          aria-busy="true"
          className="dialog-content app-update-dialog"
          onEscapeKeyDown={(event) => event.preventDefault()}
          onInteractOutside={(event) => event.preventDefault()}
          onOpenAutoFocus={handleOpenAutoFocus}
        >
          <div className="dialog-header">
            <div>
              <UpdateEyebrow />
              <Dialog.Title className="dialog-title" ref={titleRef} tabIndex={-1}>
                正在安装 v{update.nextVersion}
              </Dialog.Title>
              <Dialog.Description className="dialog-description">
                更新包已完成下载和签名校验，安装完成后将自动重启。
              </Dialog.Description>
            </div>
          </div>

          <div aria-live="polite" className="app-update-progress">
            <progress aria-label="正在安装更新" />
            <span>正在安装，请勿退出应用</span>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function CheckErrorPrompt({
  isOpen,
  onDismiss,
  onCheck,
  update,
}: {
  isOpen: boolean;
  onDismiss: () => void;
  onCheck: () => Promise<void>;
  update: CheckError;
}) {
  const titleRef = useRef<HTMLHeadingElement>(null);

  function handleOpenChange(open: boolean): void {
    if (!open) {
      onDismiss();
    }
  }

  function handleOpenAutoFocus(event: Event): void {
    event.preventDefault();
    titleRef.current?.focus();
  }

  function handleRetry(): void {
    void onCheck();
  }

  return (
    <Dialog.Root onOpenChange={handleOpenChange} open={isOpen}>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          className="dialog-content app-update-dialog"
          onOpenAutoFocus={handleOpenAutoFocus}
        >
          <div className="dialog-header">
            <div>
              <UpdateEyebrow />
              <Dialog.Title className="dialog-title" ref={titleRef} tabIndex={-1}>
                检查更新失败
              </Dialog.Title>
              <Dialog.Description className="dialog-description">
                网络或更新服务暂时不可用，可以稍后重试。
              </Dialog.Description>
            </div>
            <Dialog.Close aria-label="关闭" className="icon-button">
              <X aria-hidden="true" size={16} />
            </Dialog.Close>
          </div>

          <div className="warning-box" role="alert">
            <strong>更新检查未完成</strong>
            <p>{update.message}</p>
          </div>

          <div className="actions app-update-actions">
            <Dialog.Close asChild>
              <Button variant="secondary">稍后</Button>
            </Dialog.Close>
            <Button onClick={handleRetry}>
              <RefreshCw aria-hidden="true" size={15} /> 重试
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function InstallErrorPrompt({
  isOpen,
  onDismiss,
  onOpenDownload,
  onRetry,
  update,
}: {
  isOpen: boolean;
  onDismiss: () => void;
  onOpenDownload: () => void;
  onRetry: () => Promise<void>;
  update: InstallError;
}) {
  const titleRef = useRef<HTMLHeadingElement>(null);

  function handleOpenChange(open: boolean): void {
    if (!open) {
      onDismiss();
    }
  }

  function handleOpenAutoFocus(event: Event): void {
    event.preventDefault();
    titleRef.current?.focus();
  }

  function handleRetry(): void {
    void onRetry();
  }

  return (
    <Dialog.Root onOpenChange={handleOpenChange} open={isOpen}>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          className="dialog-content app-update-dialog"
          onOpenAutoFocus={handleOpenAutoFocus}
        >
          <div className="dialog-header">
            <div>
              <UpdateEyebrow />
              <Dialog.Title className="dialog-title" ref={titleRef} tabIndex={-1}>
                v{update.nextVersion} 安装失败
              </Dialog.Title>
              <Dialog.Description className="dialog-description">
                更新包未能完成安装，当前版本未改变。
              </Dialog.Description>
            </div>
            <Dialog.Close aria-label="稍后重试" className="icon-button">
              <X aria-hidden="true" size={16} />
            </Dialog.Close>
          </div>

          <div className="warning-box" role="alert">
            <strong>更新失败</strong>
            <p>{update.message}</p>
          </div>

          <div className="actions app-update-actions">
            <Dialog.Close asChild>
              <Button variant="secondary">稍后</Button>
            </Dialog.Close>
            <Button onClick={onOpenDownload} variant="secondary">
              <ExternalLink aria-hidden="true" size={15} /> 打开下载页
            </Button>
            <Button onClick={handleRetry}>
              <RefreshCw aria-hidden="true" size={15} /> 重试
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function RestartErrorPrompt({
  isOpen,
  onDismiss,
  onRestart,
  update,
}: {
  isOpen: boolean;
  onDismiss: () => void;
  onRestart: () => Promise<void>;
  update: RestartError;
}) {
  const titleRef = useRef<HTMLHeadingElement>(null);

  function handleOpenChange(open: boolean): void {
    if (!open) {
      onDismiss();
    }
  }

  function handleOpenAutoFocus(event: Event): void {
    event.preventDefault();
    titleRef.current?.focus();
  }

  function handleRestart(): void {
    void onRestart();
  }

  return (
    <Dialog.Root onOpenChange={handleOpenChange} open={isOpen}>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          className="dialog-content app-update-dialog"
          onOpenAutoFocus={handleOpenAutoFocus}
        >
          <div className="dialog-header">
            <div>
              <UpdateEyebrow />
              <Dialog.Title className="dialog-title" ref={titleRef} tabIndex={-1}>
                v{update.nextVersion} 已安装
              </Dialog.Title>
              <Dialog.Description className="dialog-description">
                更新已经安装，但应用未能自动重启。你也可以稍后手动重新打开应用。
              </Dialog.Description>
            </div>
            <Dialog.Close aria-label="稍后重启" className="icon-button">
              <X aria-hidden="true" size={16} />
            </Dialog.Close>
          </div>

          <div className="warning-box" role="alert">
            <strong>自动重启失败</strong>
            <p>{update.message}</p>
          </div>

          <div className="actions app-update-actions">
            <Dialog.Close asChild>
              <Button variant="secondary">稍后</Button>
            </Dialog.Close>
            <Button onClick={handleRestart}>
              <RefreshCw aria-hidden="true" size={15} /> 立即重启
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function UpdateEyebrow() {
  return (
    <div className="app-update-eyebrow">
      <Sparkles aria-hidden="true" size={14} /> 软件更新
    </div>
  );
}

function getDownloadProgressLabel(update: DownloadingUpdate): string {
  if (update.retryAttempt && update.retryTotal) {
    return `下载遇到网络波动，正在第 ${update.retryAttempt} / ${update.retryTotal} 次重试`;
  }
  if (update.progress === null) {
    return "正在准备下载";
  }
  return `已下载 ${update.progress}%`;
}
