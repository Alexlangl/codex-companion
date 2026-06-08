#!/usr/bin/env node
import { existsSync, mkdirSync } from "node:fs";
import { homedir, platform, tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..");
const isWindows = platform() === "win32";
const isMac = platform() === "darwin";

const devCodexHome = envPath("DEV_CODEX_HOME", join(tmpdir(), "devcodex-home"));
const devCodexWorkspace = envPath("DEV_CODEX_WORKSPACE", join(tmpdir(), "devcodex-workspace"));
const devCodexAppData = envPath("DEV_CODEX_APP_DATA", join(tmpdir(), "devcodex-app-data"));
const devcodexLang = resolveLanguage();

mkdirSync(devCodexHome, { recursive: true });
mkdirSync(devCodexWorkspace, { recursive: true });
mkdirSync(devCodexAppData, { recursive: true });

process.env.CODEX_HOME = devCodexHome;
process.env.CODEX_SQLITE_HOME ||= devCodexHome;
process.chdir(devCodexWorkspace);

const explicitCommand = process.env.DEVCODEX_COMMAND?.trim();
if (explicitCommand) {
  runForeground(explicitCommand);
}

const kind = (process.env.DEVCODEX_KIND || defaultKind()).toLowerCase();
if (kind === "app") {
  const executable = resolveAppExecutable();
  if (!executable) {
    console.error("devcodex app executable not found. Set DEVCODEX_APP_PATH or use DEVCODEX_KIND=cli.");
    process.exit(127);
  }
  const appArgs = [`--user-data-dir=${devCodexAppData}`];
  if (devcodexLang) {
    appArgs.push(`--lang=${devcodexLang}`);
    process.env.LANG ||= `${devcodexLang}.UTF-8`;
    process.env.LC_ALL ||= `${devcodexLang}.UTF-8`;
    process.env.LANGUAGE ||= devcodexLang;
  }
  const child = spawn(executable, appArgs, {
    cwd: devCodexWorkspace,
    detached: true,
    env: process.env,
    stdio: "ignore",
  });
  child.unref();
  process.exit(0);
}

const bin = process.env.DEVCODEX_BIN || "codex";
const child = spawn(bin, process.argv.slice(2), {
  cwd: devCodexWorkspace,
  env: process.env,
  shell: isWindows,
  stdio: "inherit",
});
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 0);
});
child.on("error", (error) => {
  console.error(`failed to start devcodex CLI: ${error.message}`);
  process.exit(1);
});

function defaultKind() {
  if (isMac && resolveAppExecutable()) return "app";
  if (isWindows && resolveAppExecutable()) return "app";
  return "cli";
}

function resolveLanguage() {
  const configured = process.env.DEVCODEX_LANG?.trim();
  if (!configured || /^(system|auto|none|off|0)$/i.test(configured)) return "";
  return normalizeLocale(configured);
}

function normalizeLocale(locale) {
  return locale.trim().replaceAll("_", "-");
}

function resolveAppExecutable() {
  const configured = process.env.DEVCODEX_APP_PATH?.trim();
  const candidates = [];
  if (configured) candidates.push(configured);
  if (isMac) candidates.push("/Applications/Codex.app");
  if (isWindows) {
    const localAppData = process.env.LOCALAPPDATA || join(homedir(), "AppData", "Local");
    const programFiles = process.env.ProgramFiles || "C:\\Program Files";
    candidates.push(
      join(localAppData, "Programs", "OpenAI", "Codex", "Codex.exe"),
      join(localAppData, "Programs", "Codex", "Codex.exe"),
      join(programFiles, "OpenAI", "Codex", "Codex.exe"),
      join(programFiles, "Codex", "Codex.exe"),
    );
  }
  for (const candidate of candidates) {
    const executable = appExecutable(candidate);
    if (executable && existsSync(executable)) return executable;
  }
  return null;
}

function appExecutable(value) {
  if (!value) return null;
  if (isMac && value.endsWith(".app")) return join(value, "Contents", "MacOS", "Codex");
  if (isWindows && value.toLowerCase().endsWith(".exe")) return value;
  if (isWindows) return join(value, "Codex.exe");
  return value;
}

function runForeground(command) {
  const result = spawnSync(command, {
    cwd: devCodexWorkspace,
    env: process.env,
    shell: true,
    stdio: "inherit",
  });
  if (result.error) {
    console.error(`failed to start devcodex command: ${result.error.message}`);
    process.exit(1);
  }
  if (result.signal) process.kill(process.pid, result.signal);
  process.exit(result.status ?? 0);
}

function envPath(name, fallback) {
  const value = process.env[name]?.trim();
  if (!value) return fallback;
  return isAbsolute(value) ? value : resolve(repoRoot, value);
}
