#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..");
const devTarget = (process.env.CODEX_COMPANION_DEV_TARGET || "sandbox").trim().toLowerCase();

if (!["sandbox", "local"].includes(devTarget)) {
  console.error("CODEX_COMPANION_DEV_TARGET must be either 'sandbox' or 'local'.");
  process.exit(1);
}

const devCodexHome = envPath("DEV_CODEX_HOME", join(tmpdir(), "devcodex-home"));
const devCodexWorkspace = envPath("DEV_CODEX_WORKSPACE", join(tmpdir(), "devcodex-workspace"));
const devCodexAppData = envPath("DEV_CODEX_APP_DATA", join(tmpdir(), "devcodex-app-data"));
const devCompanionHome = envPath("DEV_COMPANION_HOME", join(tmpdir(), "devcodex-companion"));

mkdirSync(devCompanionHome, { recursive: true });
process.env.DEV_COMPANION_HOME = devCompanionHome;
process.env.CODEX_COMPANION_HOME = devCompanionHome;

if (devTarget === "sandbox") {
  mkdirSync(devCodexHome, { recursive: true });
  mkdirSync(devCodexWorkspace, { recursive: true });
  mkdirSync(devCodexAppData, { recursive: true });
  ensureSandboxConfig(join(devCodexHome, "config.toml"));

  const devcodexScript = join(scriptDir, "devcodex.mjs");
  process.env.DEV_CODEX_HOME = devCodexHome;
  process.env.DEV_CODEX_WORKSPACE = devCodexWorkspace;
  process.env.DEV_CODEX_APP_DATA = devCodexAppData;
  process.env.CODEX_COMPANION_CODEX_DIR = devCodexHome;
  process.env.CODEX_COMPANION_CODEX_COMMAND = `${shellQuote(process.execPath)} ${shellQuote(devcodexScript)}`;
  process.env.CODEX_COMPANION_CODEX_APP_DATA = devCodexAppData;
  process.env.CODEX_COMPANION_CODEX_PROCESS_MATCH = `--user-data-dir=${devCodexAppData}`;

  if (truthy(process.env.CODEX_COMPANION_START_DEVCODEX) || process.env.DEVCODEX_COMMAND?.trim()) {
    const child = spawn(process.env.CODEX_COMPANION_CODEX_COMMAND, {
      cwd: repoRoot,
      env: process.env,
      shell: true,
      stdio: "inherit",
    });
    child.on("error", (error) => {
      console.error(`failed to start devcodex: ${error.message}`);
    });
  }
} else {
  delete process.env.CODEX_COMPANION_CODEX_DIR;
  delete process.env.CODEX_COMPANION_CODEX_COMMAND;
  delete process.env.CODEX_COMPANION_CODEX_APP_DATA;
  delete process.env.CODEX_COMPANION_CODEX_PROCESS_MATCH;
  delete process.env.CODEX_COMPANION_SKIP_CODEX_RESTART;
}

const packageManager = packageManagerCommand();
const devApp = spawn(packageManager.command, packageManager.args, {
  cwd: repoRoot,
  env: process.env,
  shell: false,
  stdio: "inherit",
});
devApp.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 0);
});
devApp.on("error", (error) => {
  console.error(`failed to start codex-companion dev app: ${error.message}`);
  process.exit(1);
});

function ensureSandboxConfig(path) {
  let text = existsSync(path) ? readFileSync(path, "utf8") : "";
  text = upsertTomlLine(text, "sandbox_mode", '"workspace-write"');
  text = upsertTomlLine(text, "approval_policy", '"on-request"');
  if (!/^[ \t]*\[sandbox_workspace_write\]/m.test(text)) {
    text += `${text.endsWith("\n") ? "" : "\n"}\n[sandbox_workspace_write]\nnetwork_access = false\nexclude_tmpdir_env_var = false\nexclude_slash_tmp = false\n`;
  }
  writeFileSync(path, text);
}

function upsertTomlLine(text, key, value) {
  const line = `${key} = ${value}`;
  const pattern = new RegExp(`^[ \\t]*${key}[ \\t]*=.*$`, "m");
  if (pattern.test(text)) return text.replace(pattern, line);
  return `${text}${text.endsWith("\n") || text.length === 0 ? "" : "\n"}${line}\n`;
}

function packageManagerCommand() {
  const execPath = process.env.npm_execpath;
  if (execPath) return { command: process.execPath, args: [execPath, "run", "dev:app"] };
  return { command: "pnpm", args: ["run", "dev:app"] };
}

function envPath(name, fallback) {
  const value = process.env[name]?.trim();
  if (!value) return fallback;
  return isAbsolute(value) ? value : resolve(repoRoot, value);
}

function truthy(value) {
  return /^(1|true|yes|on)$/i.test(value || "");
}

function shellQuote(value) {
  if (process.platform === "win32") return `"${value.replaceAll('"', '\\"')}"`;
  return `'${value.replaceAll("'", "'\\''")}'`;
}
