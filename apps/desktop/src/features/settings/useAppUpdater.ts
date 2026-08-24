import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useRef, useState } from "react";
import { openExternalUrl } from "../../lib/api";
import {
  isRetryableUpdaterError,
  normalizeUpdaterErrorMessage,
  retryWithBackoff,
  sanitizeUpdaterErrorMessage,
  UPDATE_CHECK_RETRY_DELAYS_MS,
  UPDATE_DOWNLOAD_RETRY_DELAYS_MS,
} from "./updaterRetry";

const AUTO_CHECK_STORAGE_KEY = "codex-companion.updater.last-auto-check-at";
const AUTO_CHECK_INTERVAL_MS = 60 * 60 * 1_000;
const CHECK_TIMEOUT_MS = 12_000;
const DOWNLOAD_TIMEOUT_MS = 10 * 60 * 1_000;
const RELEASE_PAGE_URL = "https://github.com/Alexlangl/codex-companion/releases";

export type AppUpdateState =
  | { status: "loading"; currentVersion: string }
  | { status: "unsupported"; currentVersion: string }
  | { status: "checking"; currentVersion: string; retryAttempt?: number; retryTotal?: number }
  | { status: "latest"; currentVersion: string }
  | {
      status: "available";
      currentVersion: string;
      nextVersion: string;
      notes: string;
      downloadUrl: string;
    }
  | {
      status: "downloading";
      currentVersion: string;
      nextVersion: string;
      progress: number | null;
      retryAttempt?: number;
      retryTotal?: number;
    }
  | { status: "installing"; currentVersion: string; nextVersion: string }
  | {
      status: "check-error";
      currentVersion: string;
      message: string;
      automatic: boolean;
      errorId: number;
    }
  | {
      status: "install-error";
      currentVersion: string;
      nextVersion: string;
      message: string;
      downloadUrl: string;
      errorId: number;
    }
  | {
      status: "restart-error";
      currentVersion: string;
      nextVersion: string;
      message: string;
      errorId: number;
    };

export type AppUpdaterController = {
  state: AppUpdateState;
  checkForUpdates: () => Promise<void>;
  installUpdate: () => Promise<void>;
  restartApp: () => Promise<void>;
  openDownloadUrl: (url: string) => Promise<void>;
};

export function useAppUpdater(notify: (message: string) => void): AppUpdaterController {
  const [state, setState] = useState<AppUpdateState>({ status: "loading", currentVersion: "" });
  const pendingUpdateRef = useRef<Update | null>(null);
  const downloadedVersionRef = useRef<string | null>(null);
  const hasAutoCheckedRef = useRef(false);
  const mountedRef = useRef(true);
  const checkInFlightRef = useRef<Promise<void> | null>(null);
  const errorIdRef = useRef(0);

  const checkForUpdates = useCallback(
    async (automatic = false): Promise<void> => {
      if (!isTauri()) {
        setState({ status: "unsupported", currentVersion: "开发模式" });
        return;
      }

      const inFlightCheck = checkInFlightRef.current;
      if (inFlightCheck) {
        await inFlightCheck;
        return;
      }

      const checkTask = (async (): Promise<void> => {
        let currentVersion = "";
        try {
          currentVersion = await getVersion();
          if (automatic && !shouldRunAutomaticCheck()) {
            if (mountedRef.current) {
              setState({ status: "latest", currentVersion });
            }
            return;
          }
          if (mountedRef.current) {
            setState({ status: "checking", currentVersion });
          }
          const update = await retryWithBackoff(
            () => check({ timeout: CHECK_TIMEOUT_MS }),
            {
              delaysMs: UPDATE_CHECK_RETRY_DELAYS_MS,
              shouldRetry: isRetryableUpdaterError,
              onRetry: ({ retryIndex, totalRetries }) => {
                if (mountedRef.current) {
                  setState({
                    status: "checking",
                    currentVersion,
                    retryAttempt: retryIndex,
                    retryTotal: totalRetries,
                  });
                }
              },
            },
          );
          if (!mountedRef.current) {
            await update?.close();
            return;
          }
          await replacePendingUpdate(pendingUpdateRef, update);
          if (automatic) {
            markAutomaticCheckCompleted();
          }
          downloadedVersionRef.current = null;

          if (!update) {
            if (mountedRef.current) {
              setState({ status: "latest", currentVersion });
              if (!automatic) {
                notify("当前已是最新版本");
              }
            }
            return;
          }

          const notes = update.body?.trim() || "查看 GitHub Release 获取本次更新说明。";
          if (mountedRef.current) {
            setState({
              status: "available",
              currentVersion,
              nextVersion: update.version,
              notes,
              downloadUrl: resolveUpdaterDownloadUrl(update.rawJson),
            });
            if (!automatic) {
              notify(`发现新版本 v${update.version}`);
            }
          }
        } catch (unknownError) {
          if (mountedRef.current) {
            setState({
              status: "check-error",
              currentVersion,
              message: normalizeUpdateError(unknownError, "检查更新"),
              automatic,
              errorId: nextErrorId(errorIdRef),
            });
          }
        }
      })();
      checkInFlightRef.current = checkTask;
      try {
        await checkTask;
      } finally {
        if (checkInFlightRef.current === checkTask) {
          checkInFlightRef.current = null;
        }
      }
    },
    [notify],
  );

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

    try {
      if (downloadedVersionRef.current !== nextVersion) {
        setState({ status: "downloading", currentVersion, nextVersion, progress: 0 });
        await retryWithBackoff(
          () =>
            update.download(
              (event) => {
                const progress = updateDownloadProgress(event, {
                  contentLength,
                  downloadedBytes,
                });
                contentLength = progress.contentLength;
                downloadedBytes = progress.downloadedBytes;
                if (mountedRef.current) {
                  setState({
                    status: "downloading",
                    currentVersion,
                    nextVersion,
                    progress: progress.percent,
                  });
                }
              },
              { timeout: DOWNLOAD_TIMEOUT_MS },
            ),
          {
            delaysMs: UPDATE_DOWNLOAD_RETRY_DELAYS_MS,
            shouldRetry: isRetryableUpdaterError,
            onRetry: ({ retryIndex, totalRetries }) => {
              downloadedBytes = 0;
              contentLength = undefined;
              if (mountedRef.current) {
                setState({
                  status: "downloading",
                  currentVersion,
                  nextVersion,
                  progress: 0,
                  retryAttempt: retryIndex,
                  retryTotal: totalRetries,
                });
              }
            },
          },
        );
        downloadedVersionRef.current = nextVersion;
      }

      setState({ status: "installing", currentVersion, nextVersion });
      await update.install();
    } catch (unknownError) {
      setState({
        status: "install-error",
        currentVersion,
        nextVersion,
        message: normalizeUpdateError(unknownError, "安装更新"),
        downloadUrl: resolveUpdaterDownloadUrl(update.rawJson),
        errorId: nextErrorId(errorIdRef),
      });
      return;
    }

    downloadedVersionRef.current = null;
    notify(`v${nextVersion} 已安装，正在重启`);
    try {
      await relaunch();
    } catch (unknownError) {
      setState({
        status: "restart-error",
        currentVersion,
        nextVersion,
        message: normalizeUpdateError(unknownError, "重启应用"),
        errorId: nextErrorId(errorIdRef),
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
        errorId: nextErrorId(errorIdRef),
      });
    }
  }, []);

  const openDownloadUrl = useCallback(
    async (url: string): Promise<void> => {
      try {
        await openExternalUrl(url);
      } catch (error) {
        notify(`无法打开下载页：${sanitizeUpdaterErrorMessage(error)}`);
      }
    },
    [notify],
  );

  useEffect(() => {
    mountedRef.current = true;
    if (!hasAutoCheckedRef.current) {
      hasAutoCheckedRef.current = true;
      void checkForUpdates(true);
    }
  }, [checkForUpdates]);

  useEffect(() => {
    return () => {
      mountedRef.current = false;
      const update = pendingUpdateRef.current;
      pendingUpdateRef.current = null;
      downloadedVersionRef.current = null;
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
    openDownloadUrl,
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

function normalizeUpdateError(
  error: unknown,
  action: "检查更新" | "安装更新" | "重启应用",
): string {
  const message = normalizeUpdaterErrorMessage(error);
  const normalizedMessage = message.toLowerCase();
  if (
    action === "检查更新" &&
    (/\b404\b/.test(message) ||
      /release(?:\s+|_)?not[\s_-]?found/.test(normalizedMessage) ||
      normalizedMessage.includes("could not fetch a valid release json"))
  ) {
    return "暂未发布可用于自动更新的稳定版本。";
  }
  const sanitized = sanitizeUpdaterErrorMessage(error);
  return sanitized && sanitized !== "undefined" && sanitized !== "null"
    ? `${action}失败：${sanitized}`
    : `${action}失败，请稍后重试。`;
}

function nextErrorId(errorIdRef: { current: number }): number {
  errorIdRef.current += 1;
  return errorIdRef.current;
}

function resolveUpdaterDownloadUrl(rawJson: Record<string, unknown>): string {
  for (const key of ["url", "download_url", "html_url", "details_url"]) {
    const value = rawJson[key];
    if (typeof value === "string" && isAllowedDownloadUrl(value)) {
      return value;
    }
  }
  return RELEASE_PAGE_URL;
}

function isAllowedDownloadUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && ["github.com", "www.github.com"].includes(url.hostname);
  } catch {
    return false;
  }
}

function shouldRunAutomaticCheck(): boolean {
  try {
    const raw = window.localStorage.getItem(AUTO_CHECK_STORAGE_KEY);
    const lastCheckedAt = raw ? Number(raw) : 0;
    return !Number.isFinite(lastCheckedAt) || Date.now() - lastCheckedAt >= AUTO_CHECK_INTERVAL_MS;
  } catch {
    return true;
  }
}

function markAutomaticCheckCompleted(): void {
  try {
    window.localStorage.setItem(AUTO_CHECK_STORAGE_KEY, String(Date.now()));
  } catch {
    // Storage can be unavailable in restricted webviews; the network check still works.
  }
}
