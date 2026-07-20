import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useRef, useState } from "react";

export type AppUpdateState =
  | { status: "loading"; currentVersion: string }
  | { status: "unsupported"; currentVersion: string }
  | { status: "checking"; currentVersion: string }
  | { status: "latest"; currentVersion: string }
  | { status: "available"; currentVersion: string; nextVersion: string; notes: string }
  | { status: "downloading"; currentVersion: string; nextVersion: string; progress: number | null }
  | { status: "check-error"; currentVersion: string; message: string }
  | { status: "install-error"; currentVersion: string; nextVersion: string; message: string }
  | { status: "restart-error"; currentVersion: string; nextVersion: string; message: string };

export type AppUpdaterController = {
  state: AppUpdateState;
  checkForUpdates: () => Promise<void>;
  installUpdate: () => Promise<void>;
  restartApp: () => Promise<void>;
};

export function useAppUpdater(notify: (message: string) => void): AppUpdaterController {
  const [state, setState] = useState<AppUpdateState>({ status: "loading", currentVersion: "" });
  const pendingUpdateRef = useRef<Update | null>(null);
  const hasAutoCheckedRef = useRef(false);

  const checkForUpdates = useCallback(async (automatic = false): Promise<void> => {
    if (!isTauri()) {
      setState({ status: "unsupported", currentVersion: "开发模式" });
      return;
    }

    let currentVersion = "";
    try {
      currentVersion = await getVersion();
      setState({ status: "checking", currentVersion });
      const update = await check({ timeout: 15_000 });
      await replacePendingUpdate(pendingUpdateRef, update);

      if (!update) {
        setState({ status: "latest", currentVersion });
        if (!automatic) {
          notify("当前已是最新版本");
        }
        return;
      }

      const notes = update.body?.trim() || "查看 GitHub Release 获取本次更新说明。";
      setState({
        status: "available",
        currentVersion,
        nextVersion: update.version,
        notes,
      });
      if (!automatic) {
        notify(`发现新版本 v${update.version}`);
      }
    } catch (unknownError) {
      setState({
        status: "check-error",
        currentVersion,
        message: normalizeUpdateError(unknownError, "检查更新"),
      });
    }
  }, [notify]);

  const installUpdate = useCallback(async (): Promise<void> => {
    const update = pendingUpdateRef.current;
    if (!update) {
      await checkForUpdates();
      return;
    }

    const currentVersion = update.currentVersion;
    const nextVersion = update.version;
    let downloadedBytes = 0;
    let contentLength: number | undefined;

    setState({ status: "downloading", currentVersion, nextVersion, progress: 0 });
    try {
      await update.downloadAndInstall((event) => {
        const progress = updateDownloadProgress(event, {
          contentLength,
          downloadedBytes,
        });
        contentLength = progress.contentLength;
        downloadedBytes = progress.downloadedBytes;
        setState({
          status: "downloading",
          currentVersion,
          nextVersion,
          progress: progress.percent,
        });
      });
    } catch (unknownError) {
      setState({
        status: "install-error",
        currentVersion,
        nextVersion,
        message: normalizeUpdateError(unknownError, "安装更新"),
      });
      return;
    }

    notify(`v${nextVersion} 已安装，正在重启`);
    try {
      await relaunch();
    } catch (unknownError) {
      setState({
        status: "restart-error",
        currentVersion,
        nextVersion,
        message: normalizeUpdateError(unknownError, "重启应用"),
      });
    }
  }, [checkForUpdates, notify]);

  const restartApp = useCallback(async (): Promise<void> => {
    const update = pendingUpdateRef.current;
    if (!update) {
      return;
    }
    try {
      await relaunch();
    } catch (unknownError) {
      setState({
        status: "restart-error",
        currentVersion: update.currentVersion,
        nextVersion: update.version,
        message: normalizeUpdateError(unknownError, "重启应用"),
      });
    }
  }, []);

  useEffect(() => {
    if (hasAutoCheckedRef.current) {
      return;
    }
    hasAutoCheckedRef.current = true;
    void checkForUpdates(true);
  }, [checkForUpdates]);

  useEffect(() => {
    return () => {
      const update = pendingUpdateRef.current;
      pendingUpdateRef.current = null;
      if (update) {
        void update.close();
      }
    };
  }, []);

  return {
    state,
    checkForUpdates: () => checkForUpdates(false),
    installUpdate,
    restartApp,
  };
}

async function replacePendingUpdate(
  updateRef: { current: Update | null },
  nextUpdate: Update | null,
): Promise<void> {
  const previousUpdate = updateRef.current;
  updateRef.current = nextUpdate;
  if (previousUpdate && previousUpdate !== nextUpdate) {
    await previousUpdate.close();
  }
}

function updateDownloadProgress(
  event: DownloadEvent,
  current: { contentLength?: number; downloadedBytes: number },
): { contentLength?: number; downloadedBytes: number; percent: number | null } {
  if (event.event === "Started") {
    return {
      contentLength: event.data.contentLength,
      downloadedBytes: 0,
      percent: event.data.contentLength ? 0 : null,
    };
  }
  if (event.event === "Progress") {
    const downloadedBytes = current.downloadedBytes + event.data.chunkLength;
    const percent = current.contentLength
      ? Math.min(100, Math.round((downloadedBytes / current.contentLength) * 100))
      : null;
    return { ...current, downloadedBytes, percent };
  }
  return { ...current, percent: 100 };
}

function normalizeUpdateError(error: unknown, action: "检查更新" | "安装更新" | "重启应用"): string {
  const message = error instanceof Error ? error.message : String(error);
  if (action === "检查更新" && message.includes("404")) {
    return "尚未发布可用于自动更新的稳定版本。";
  }
  return `${action}失败：${message}`;
}
