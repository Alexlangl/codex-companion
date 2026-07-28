#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..");
requireNode22();
const options = parseDevOptions(process.argv.slice(2));

if (options.help) {
  printHelp();
  process.exit(0);
}

const devRoot = resolvePathSetting(options.devRoot, ["CODEX_COMPANION_DEV_ROOT"], join(tmpdir(), "codex-companion-dev"));
const devClientHome = resolvePathSetting(null, ["DEV_CLIENT_HOME", "DEV_CODEX_HOME"], join(devRoot, "client-home"));
const devClientWorkspace = resolvePathSetting(options.workspace, ["DEV_CLIENT_WORKSPACE", "DEV_CODEX_WORKSPACE"], join(devRoot, "workspace"));
const devClientAppData = resolvePathSetting(null, ["DEV_CLIENT_APP_DATA", "DEV_CODEX_APP_DATA"], join(devRoot, "client-app-data"));
const devCompanionHome = resolvePathSetting(null, ["DEV_COMPANION_HOME"], join(devRoot, "companion-home"));
const devClientScript = join(scriptDir, "devcodex.mjs");

applyOptionEnvironment(options);
process.env.DEV_COMPANION_HOME = devCompanionHome;
process.env.CODEX_COMPANION_HOME = devCompanionHome;
setCompatibleEnv("DEV_CLIENT_WORKSPACE", "DEV_CODEX_WORKSPACE", devClientWorkspace);

const managementOverrides = {
  appName: envValue("CODEX_COMPANION_CLIENT_APP_NAME", "CODEX_COMPANION_CODEX_APP_NAME"),
  processMatch: envValue("CODEX_COMPANION_CLIENT_PROCESS_MATCH", "CODEX_COMPANION_CODEX_PROCESS_MATCH"),
};

if (options.target === "sandbox") {
  setCompatibleEnv("DEV_CLIENT_HOME", "DEV_CODEX_HOME", devClientHome);
  process.env.CODEX_COMPANION_CODEX_DIR = devClientHome;
} else {
  clearEnv(
    "CODEX_COMPANION_CODEX_DIR",
    "DEV_CLIENT_HOME",
    "DEV_CODEX_HOME",
    "DEV_CLIENT_APP_DATA",
    "DEV_CODEX_APP_DATA",
  );
}

const clientCommand = [
  shellQuote(process.execPath),
  shellQuote(devClientScript),
  options.target === "local" ? "--local" : null,
].filter(Boolean).join(" ");
setCompatibleEnv("CODEX_COMPANION_CLIENT_COMMAND", "CODEX_COMPANION_CODEX_COMMAND", clientCommand);

const clientConfig = probeClientConfig(devClientScript, options.target, options.startClient || options.dryRun);
configureClientManagement({
  clientConfig,
  devClientAppData,
  managementOverrides,
  options,
});

if (options.dryRun) {
  printPlan({
    devClientAppData,
    devClientHome,
    devClientWorkspace,
    devCompanionHome,
    clientConfig,
    options,
  });
  process.exit(0);
}

mkdirSync(devCompanionHome, { recursive: true });
mkdirSync(devClientWorkspace, { recursive: true });

if (options.target === "sandbox") {
  mkdirSync(devClientHome, { recursive: true });
  if (clientConfig.kind === "app" && !clientConfig.customCommandConfigured) {
    mkdirSync(devClientAppData, { recursive: true });
  }
  ensureSandboxConfig(join(devClientHome, "config.toml"));
}

if (options.startClient) {
  startClient(process.env.CODEX_COMPANION_CLIENT_COMMAND);
}

console.log(`[codex-companion] dev target: ${options.target}`);
console.log(`[codex-companion] Companion data: ${devCompanionHome}`);
if (options.target === "sandbox") {
  console.log(`[codex-companion] isolated client data: ${devClientHome}`);
}

const packageManager = packageManagerCommand(options.tauriArgs);
const devApp = spawn(packageManager.command, packageManager.args, {
  cwd: repoRoot,
  env: process.env,
  shell: false,
  stdio: (clientConfig.kind === "cli" || clientConfig.customCommandConfigured) && options.startClient
    ? ["ignore", "inherit", "inherit"]
    : "inherit",
});
devApp.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 0);
});
devApp.on("error", (error) => {
  console.error(`failed to start codex-companion dev app: ${error.message}`);
  process.exit(1);
});

function parseDevOptions(args) {
  const configuredCommand = envValue("DEV_CLIENT_COMMAND", "DEVCODEX_COMMAND");
  const parsed = {
    appPath: envValue("DEV_CLIENT_APP_PATH", "DEVCODEX_APP_PATH"),
    clientKind: envValue("DEV_CLIENT_KIND", "DEVCODEX_KIND") || "auto",
    cliBin: envValue("DEV_CLIENT_BIN", "DEVCODEX_BIN"),
    customCommand: configuredCommand,
    devRoot: null,
    dryRun: false,
    help: false,
    host: process.env.CODEX_COMPANION_DEV_HOST?.trim() || null,
    language: envValue("DEV_CLIENT_LANG", "DEVCODEX_LANG"),
    port: process.env.CODEX_COMPANION_DEV_PORT?.trim() || null,
    skipClientRestart:
      truthy(process.env.CODEX_COMPANION_SKIP_CLIENT_RESTART)
      || truthy(process.env.CODEX_COMPANION_SKIP_CODEX_RESTART),
    startClient:
      truthy(process.env.CODEX_COMPANION_START_CLIENT)
      || truthy(process.env.CODEX_COMPANION_START_DEVCODEX)
      || Boolean(configuredCommand),
    target: (process.env.CODEX_COMPANION_DEV_TARGET || "sandbox").trim().toLowerCase(),
    tauriArgs: [],
    workspace: null,
  };

  const invokedByPackageManager = Boolean(process.env.npm_lifecycle_event);
  let skippedPackageManagerSeparator = false;
  let startClientExplicit = false;
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") {
      if (invokedByPackageManager && !skippedPackageManagerSeparator) {
        skippedPackageManagerSeparator = true;
        continue;
      }
      parsed.tauriArgs.push(...args.slice(index + 1));
      break;
    }
    if (arg === "--help" || arg === "-h") parsed.help = true;
    else if (arg === "--sandbox") parsed.target = "sandbox";
    else if (arg === "--local") parsed.target = "local";
    else if (arg === "--app") parsed.clientKind = "app";
    else if (arg === "--cli") parsed.clientKind = "cli";
    else if (arg === "--start-client") {
      parsed.startClient = true;
      startClientExplicit = true;
    } else if (arg === "--no-start-client") {
      parsed.startClient = false;
      startClientExplicit = true;
    } else if (arg === "--skip-client-restart") parsed.skipClientRestart = true;
    else if (arg === "--dry-run") parsed.dryRun = true;
    else if (arg === "--tauri") {
      parsed.tauriArgs.push(...args.slice(index + 1));
      break;
    } else if (arg === "--app-path") parsed.appPath = optionValue(args, ++index, arg);
    else if (arg === "--cli-bin") parsed.cliBin = optionValue(args, ++index, arg);
    else if (arg === "--command") {
      parsed.customCommand = optionValue(args, ++index, arg);
      if (!startClientExplicit) parsed.startClient = true;
    } else if (arg === "--dev-root") parsed.devRoot = optionValue(args, ++index, arg);
    else if (arg === "--host") parsed.host = optionValue(args, ++index, arg);
    else if (arg === "--lang") parsed.language = optionValue(args, ++index, arg);
    else if (arg === "--port") parsed.port = optionValue(args, ++index, arg);
    else if (arg === "--workspace") parsed.workspace = optionValue(args, ++index, arg);
    else fail(`unknown option: ${arg}. Run pnpm dev --help for usage.`);
  }

  if (!["sandbox", "local"].includes(parsed.target)) {
    fail("development target must be 'sandbox' or 'local'");
  }
  if (!["auto", "app", "cli"].includes(parsed.clientKind.toLowerCase())) {
    fail("client kind must be 'auto', 'app', or 'cli'");
  }
  parsed.clientKind = parsed.clientKind.toLowerCase();
  if (parsed.port && !validPort(parsed.port)) {
    fail(`invalid port: ${parsed.port}`);
  }
  return parsed;
}

function applyOptionEnvironment(devOptions) {
  if (devOptions.appPath) setCompatibleEnv("DEV_CLIENT_APP_PATH", "DEVCODEX_APP_PATH", resolvePath(devOptions.appPath));
  if (devOptions.cliBin) setCompatibleEnv("DEV_CLIENT_BIN", "DEVCODEX_BIN", devOptions.cliBin);
  if (devOptions.customCommand) setCompatibleEnv("DEV_CLIENT_COMMAND", "DEVCODEX_COMMAND", devOptions.customCommand);
  if (devOptions.language) setCompatibleEnv("DEV_CLIENT_LANG", "DEVCODEX_LANG", devOptions.language);
  if (devOptions.clientKind !== "auto") setCompatibleEnv("DEV_CLIENT_KIND", "DEVCODEX_KIND", devOptions.clientKind);
  if (devOptions.host) process.env.CODEX_COMPANION_DEV_HOST = devOptions.host;
  if (devOptions.port) process.env.CODEX_COMPANION_DEV_PORT = devOptions.port;
  if (devOptions.skipClientRestart) {
    process.env.CODEX_COMPANION_SKIP_CLIENT_RESTART = "1";
    process.env.CODEX_COMPANION_SKIP_CODEX_RESTART = "1";
  }
}

function probeClientConfig(clientScript, target, requireAvailable) {
  const probeArgs = [clientScript, "--print-config-json"];
  if (target === "local") probeArgs.push("--local");
  const probe = spawnSync(process.execPath, probeArgs, {
    cwd: repoRoot,
    encoding: "utf8",
    env: process.env,
  });
  if (probe.error) fail(`failed to inspect development client: ${probe.error.message}`);

  let clientConfig;
  try {
    clientConfig = JSON.parse(probe.stdout);
  } catch {
    fail("development client returned an invalid discovery result");
  }

  if (probe.status !== 0) {
    const message = probe.stderr.trim() || "development client is unavailable";
    if (requireAvailable) fail(message);
    console.warn(`[codex-companion] ${message}`);
  }
  return clientConfig;
}

function configureClientManagement(input) {
  clearEnv(
    "CODEX_COMPANION_CLIENT_APP_DATA",
    "CODEX_COMPANION_CODEX_APP_DATA",
    "CODEX_COMPANION_CLIENT_APP_NAME",
    "CODEX_COMPANION_CODEX_APP_NAME",
    "CODEX_COMPANION_CLIENT_PROCESS_MATCH",
    "CODEX_COMPANION_CODEX_PROCESS_MATCH",
    "DEV_CLIENT_APP_DATA",
    "DEV_CODEX_APP_DATA",
  );

  if (input.clientConfig.customCommandConfigured) {
    if (input.managementOverrides.processMatch) {
      setCompatibleEnv(
        "CODEX_COMPANION_CLIENT_PROCESS_MATCH",
        "CODEX_COMPANION_CODEX_PROCESS_MATCH",
        input.managementOverrides.processMatch,
      );
    } else if (input.managementOverrides.appName) {
      setCompatibleEnv(
        "CODEX_COMPANION_CLIENT_APP_NAME",
        "CODEX_COMPANION_CODEX_APP_NAME",
        input.managementOverrides.appName,
      );
    } else {
      enableSkipClientRestart();
    }
    return;
  }

  if (input.clientConfig.kind === "cli") {
    enableSkipClientRestart();
    return;
  }

  if (input.options.target === "sandbox") {
    setCompatibleEnv("DEV_CLIENT_APP_DATA", "DEV_CODEX_APP_DATA", input.devClientAppData);
    setCompatibleEnv(
      "CODEX_COMPANION_CLIENT_APP_DATA",
      "CODEX_COMPANION_CODEX_APP_DATA",
      input.devClientAppData,
    );
    setCompatibleEnv(
      "CODEX_COMPANION_CLIENT_PROCESS_MATCH",
      "CODEX_COMPANION_CODEX_PROCESS_MATCH",
      input.managementOverrides.processMatch || `--user-data-dir=${input.devClientAppData}`,
    );
    return;
  }

  const appName = input.managementOverrides.appName || input.clientConfig.appProcessName;
  if (input.managementOverrides.processMatch) {
    setCompatibleEnv(
      "CODEX_COMPANION_CLIENT_PROCESS_MATCH",
      "CODEX_COMPANION_CODEX_PROCESS_MATCH",
      input.managementOverrides.processMatch,
    );
  } else if (appName) {
    setCompatibleEnv("CODEX_COMPANION_CLIENT_APP_NAME", "CODEX_COMPANION_CODEX_APP_NAME", appName);
  } else {
    enableSkipClientRestart();
  }
}

function enableSkipClientRestart() {
  process.env.CODEX_COMPANION_SKIP_CLIENT_RESTART = "1";
  process.env.CODEX_COMPANION_SKIP_CODEX_RESTART = "1";
}

function printPlan(input) {
  console.log("[codex-companion] development plan");
  console.log(`  target: ${input.options.target}`);
  console.log(`  start client now: ${input.options.startClient ? "yes" : "no"}`);
  console.log(`  Companion data: ${input.devCompanionHome}`);
  console.log(`  client workspace: ${input.devClientWorkspace}`);
  if (input.options.target === "sandbox") {
    console.log(`  client state: ${input.devClientHome}`);
    if (input.clientConfig.kind === "app" && !input.clientConfig.customCommandConfigured) {
      console.log(`  client app data: ${input.devClientAppData}`);
    }
  }
  console.log(`  client kind: ${input.clientConfig.customCommandConfigured ? "custom command" : input.clientConfig.kind}`);
  if (input.clientConfig.customCommandConfigured) {
    console.log("  custom command: configured (contents hidden)");
  } else if (input.clientConfig.kind === "app") {
    console.log(`  app: ${input.clientConfig.appExecutable || "not found"}`);
  } else {
    console.log(`  CLI: ${input.clientConfig.cliExecutable || `${input.clientConfig.configuredCli} (not found)`}`);
  }
  console.log(
    `  automatic client restart: ${truthy(process.env.CODEX_COMPANION_SKIP_CLIENT_RESTART) ? "disabled" : "enabled"}`,
  );
  console.log(`  client process tracking: ${clientProcessTracking()}`);
  if (input.options.tauriArgs.length > 0) {
    console.log(`  tauri arguments: ${input.options.tauriArgs.join(" ")}`);
  }
}

function clientProcessTracking() {
  if (envValue("CODEX_COMPANION_CLIENT_PROCESS_MATCH", "CODEX_COMPANION_CODEX_PROCESS_MATCH")) {
    return "command-line match";
  }
  const appName = envValue("CODEX_COMPANION_CLIENT_APP_NAME", "CODEX_COMPANION_CODEX_APP_NAME");
  return appName ? `process name (${appName})` : "none";
}

function startClient(command) {
  if (!command) return;
  const child = spawn(command, {
    cwd: repoRoot,
    env: process.env,
    shell: true,
    stdio: "inherit",
  });
  child.on("error", (error) => {
    console.error(`failed to start development client: ${error.message}`);
  });
}

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

function packageManagerCommand(tauriArgs) {
  const execPath = process.env.npm_execpath;
  const scriptArgs = ["run", "dev:app", ...tauriArgs];
  if (execPath) return { command: process.execPath, args: [execPath, ...scriptArgs] };
  return { command: "pnpm", args: scriptArgs };
}

function resolvePathSetting(option, envNames, fallback) {
  const value = option || envValue(...envNames);
  return resolvePath(value || fallback);
}

function resolvePath(value) {
  return isAbsolute(value) ? value : resolve(repoRoot, value);
}

function setCompatibleEnv(currentName, legacyName, value) {
  process.env[currentName] = value;
  process.env[legacyName] = value;
}

function clearEnv(...names) {
  for (const name of names) delete process.env[name];
}

function envValue(...names) {
  for (const name of names) {
    const value = process.env[name]?.trim();
    if (value) return value;
  }
  return null;
}

function optionValue(args, index, option) {
  const value = args[index]?.trim();
  if (!value) fail(`${option} requires a value`);
  return value;
}

function validPort(value) {
  const port = Number(value);
  return Number.isInteger(port) && port > 0 && port < 65536;
}

function requireNode22() {
  const major = Number.parseInt(process.versions.node.split(".")[0], 10);
  if (major < 22) {
    console.error(`[codex-companion] Node.js 22 or newer is required (current: ${process.versions.node}). Run nvm use.`);
    process.exit(1);
  }
}

function truthy(value) {
  return /^(1|true|yes|on)$/i.test(value || "");
}

function shellQuote(value) {
  if (process.platform === "win32") return `"${value.replaceAll('"', '\\"')}"`;
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function fail(message) {
  console.error(`[codex-companion] ${message}`);
  process.exit(1);
}

function printHelp() {
  console.log(`Codex Companion development launcher

Usage:
  pnpm dev [options]

Common commands:
  pnpm dev                 Start isolated Companion and ChatGPT/Codex app
  pnpm dev:companion       Start only isolated Companion
  pnpm dev:local           Use the real local ChatGPT/Codex installation
  pnpm dev:cli             Start isolated Companion and Codex CLI

Options:
  --sandbox                Use isolated client state (default)
  --local                  Use the real local ChatGPT/Codex state and client
  --app                    Require the ChatGPT/Codex desktop app
  --cli                    Use Codex CLI instead of the desktop app
  --start-client           Start the selected client immediately
  --no-start-client        Start only Companion; UI actions can launch later
  --app-path <path>        Override ChatGPT/Codex .app or .exe
  --cli-bin <path>         Override the Codex CLI executable
  --command <command>      Use a complete custom client launch command
  --lang <locale>          Client locale, e.g. zh-CN, en-US, or system
  --dev-root <path>        Root for all isolated development data
  --workspace <path>       Client working directory
  --host <address>         Vite host (default: 127.0.0.1)
  --port <number>          Preferred Vite port (default: 1420)
  --skip-client-restart    Write config without stopping/starting the client
  --dry-run                Print resolved paths and client discovery, then exit
  --tauri <args...>        Forward the remaining arguments to tauri dev
  --help                   Show this help

Both "pnpm dev --help" and "pnpm dev -- --help" are accepted.
Legacy DEVCODEX_* and CODEX_COMPANION_*_CODEX_* variables remain supported.`);
}
