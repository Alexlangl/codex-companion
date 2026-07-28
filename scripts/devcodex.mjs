#!/usr/bin/env node
import { accessSync, constants, existsSync, mkdirSync } from "node:fs";
import { homedir, platform, tmpdir } from "node:os";
import { basename, delimiter, dirname, extname, isAbsolute, join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..");
const isWindows = platform() === "win32";
const isMac = platform() === "darwin";
requireNode22();
const options = parseClientOptions(process.argv.slice(2));

if (options.help) {
  printHelp();
  process.exit(0);
}

const devRoot = envPath(["CODEX_COMPANION_DEV_ROOT"], join(tmpdir(), "codex-companion-dev"));
const devClientHome = envPath(["DEV_CLIENT_HOME", "DEV_CODEX_HOME"], join(devRoot, "client-home"));
const devClientWorkspace = envPath(["DEV_CLIENT_WORKSPACE", "DEV_CODEX_WORKSPACE"], join(devRoot, "workspace"));
const devClientAppData = envPath(["DEV_CLIENT_APP_DATA", "DEV_CODEX_APP_DATA"], join(devRoot, "client-app-data"));
const language = resolveLanguage();
const explicitCommand = envValue("DEV_CLIENT_COMMAND", "DEVCODEX_COMMAND");
const requestedKind = options.kind || envValue("DEV_CLIENT_KIND", "DEVCODEX_KIND") || "auto";
if (!["auto", "app", "cli"].includes(requestedKind.toLowerCase())) {
  console.error(`invalid client kind: ${requestedKind}`);
  process.exit(1);
}
const resolvedApp = resolveAppExecutable();
const kind = requestedKind.toLowerCase() === "auto" ? defaultKind(resolvedApp) : requestedKind.toLowerCase();
const configuredCli = envValue("DEV_CLIENT_BIN", "DEVCODEX_BIN") || "codex";
const resolvedCli = kind === "cli" && !explicitCommand ? resolveCliExecutable(configuredCli) : null;
const clientConfig = {
  appExecutable: resolvedApp,
  appProcessName: resolvedApp ? basename(resolvedApp, extname(resolvedApp)) : null,
  appProfile: options.local ? null : devClientAppData,
  cliExecutable: resolvedCli,
  configuredCli,
  customCommandConfigured: Boolean(explicitCommand),
  kind,
  language: language || null,
  mode: options.local ? "local" : "sandbox",
  stateDirectory: options.local ? null : devClientHome,
  workspace: devClientWorkspace,
};
const validationError = clientValidationError(clientConfig);

if (options.printConfig) {
  if (options.printConfigJson) {
    process.stdout.write(`${JSON.stringify(clientConfig)}\n`);
  } else {
    printResolvedConfig(clientConfig);
  }
  if (validationError) console.error(validationError);
  process.exit(validationError ? 1 : 0);
}

if (validationError) {
  console.error(validationError);
  process.exit(127);
}

mkdirSync(devClientWorkspace, { recursive: true });
if (!options.local) {
  mkdirSync(devClientHome, { recursive: true });
  process.env.CODEX_HOME = devClientHome;
  process.env.CODEX_SQLITE_HOME ||= devClientHome;
  if (kind === "app" && !explicitCommand) {
    mkdirSync(devClientAppData, { recursive: true });
  }
}
process.chdir(devClientWorkspace);

if (explicitCommand) {
  runForeground(explicitCommand);
}

if (kind === "app") {
  const appArgs = options.local ? [] : [`--user-data-dir=${devClientAppData}`];
  if (language) {
    appArgs.push(`--lang=${language}`);
    process.env.LANG ||= `${language}.UTF-8`;
    process.env.LC_ALL ||= `${language}.UTF-8`;
    process.env.LANGUAGE ||= language;
  }
  console.log(`[dev-client] starting ${clientName(resolvedApp)}: ${resolvedApp}`);
  const child = spawn(resolvedApp, appArgs, {
    cwd: devClientWorkspace,
    detached: true,
    env: process.env,
    stdio: "ignore",
  });
  child.unref();
  process.exit(0);
}

console.log(`[dev-client] starting Codex CLI: ${resolvedCli}`);
const child = spawn(resolvedCli, options.passthroughArgs, {
  cwd: devClientWorkspace,
  env: process.env,
  shell: isWindows,
  stdio: "inherit",
});
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 0);
});
child.on("error", (error) => {
  console.error(`failed to start Codex CLI: ${error.message}`);
  process.exit(1);
});

function parseClientOptions(args) {
  const parsed = {
    help: false,
    kind: null,
    local: false,
    passthroughArgs: [],
    printConfig: false,
    printConfigJson: false,
  };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--help" || arg === "-h") parsed.help = true;
    else if (arg === "--local") parsed.local = true;
    else if (arg === "--app") parsed.kind = "app";
    else if (arg === "--cli") parsed.kind = "cli";
    else if (arg === "--print-config") parsed.printConfig = true;
    else if (arg === "--print-config-json") {
      parsed.printConfig = true;
      parsed.printConfigJson = true;
    } else if (arg === "--app-path") {
      const value = optionValue(args, ++index, arg);
      process.env.DEV_CLIENT_APP_PATH = value;
    } else if (arg === "--") {
      parsed.passthroughArgs.push(...args.slice(index + 1));
      break;
    } else {
      parsed.passthroughArgs.push(arg);
    }
  }
  return parsed;
}

function defaultKind(resolvedApp) {
  if ((isMac || isWindows) && resolvedApp) return "app";
  return "cli";
}

function resolveLanguage() {
  const configured = envValue("DEV_CLIENT_LANG", "DEVCODEX_LANG");
  if (/^(system|none|off|0)$/i.test(configured || "")) return "";
  if (!configured || /^auto$/i.test(configured)) return detectSystemLocale();
  return normalizeLocale(configured);
}

function normalizeLocale(locale) {
  return locale.trim().split(".")[0].replaceAll("_", "-");
}

function detectSystemLocale() {
  if (isMac) {
    const result = spawnSync("defaults", ["read", "-g", "AppleLocale"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    const locale = normalizeLocale(result.stdout || "");
    if (usableLocale(locale)) return locale;
  }
  for (const value of [
    process.env.LC_ALL,
    process.env.LC_MESSAGES,
    process.env.LANG,
    Intl.DateTimeFormat().resolvedOptions().locale,
  ]) {
    const locale = normalizeLocale(value || "");
    if (usableLocale(locale)) return locale;
  }
  return "";
}

function usableLocale(locale) {
  return Boolean(locale) && !/^(c|posix)$/i.test(locale);
}

function resolveAppExecutable() {
  const configured = envValue("DEV_CLIENT_APP_PATH", "DEVCODEX_APP_PATH");
  const candidates = [];
  if (configured) candidates.push(isAbsolute(configured) ? configured : resolve(repoRoot, configured));
  if (isMac) {
    candidates.push(
      "/Applications/ChatGPT.app",
      join(homedir(), "Applications", "ChatGPT.app"),
      "/Applications/Codex.app",
      join(homedir(), "Applications", "Codex.app"),
    );
  }
  if (isWindows) {
    const localAppData = process.env.LOCALAPPDATA || join(homedir(), "AppData", "Local");
    const programFiles = process.env.ProgramFiles || "C:\\Program Files";
    candidates.push(
      join(localAppData, "Programs", "OpenAI", "ChatGPT", "ChatGPT.exe"),
      join(localAppData, "Programs", "ChatGPT", "ChatGPT.exe"),
      join(programFiles, "OpenAI", "ChatGPT", "ChatGPT.exe"),
      join(programFiles, "ChatGPT", "ChatGPT.exe"),
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
  if (isMac && value.endsWith(".app")) {
    const appName = basename(value, ".app");
    for (const executableName of [appName, "ChatGPT", "Codex"]) {
      const executable = join(value, "Contents", "MacOS", executableName);
      if (existsSync(executable)) return executable;
    }
    return null;
  }
  if (isWindows && extname(value).toLowerCase() === ".exe") return value;
  if (isWindows) {
    for (const executableName of ["ChatGPT.exe", "Codex.exe"]) {
      const executable = join(value, executableName);
      if (existsSync(executable)) return executable;
    }
    return null;
  }
  return value;
}

function clientName(executable) {
  const name = basename(executable, extname(executable));
  return name === "Codex" ? "legacy Codex" : name;
}

function clientValidationError(config) {
  if (config.customCommandConfigured) return null;
  if (config.kind === "app" && !config.appExecutable) {
    return "ChatGPT/Codex app executable not found. Pass --app-path, set DEV_CLIENT_APP_PATH, or use --cli.";
  }
  if (config.kind === "cli" && !config.cliExecutable) {
    return `Codex CLI executable not found: ${config.configuredCli}. Pass --cli-bin or set DEV_CLIENT_BIN.`;
  }
  return null;
}

function printResolvedConfig(config) {
  console.log("[dev-client] resolved client");
  console.log(`  mode: ${config.mode}`);
  console.log(`  kind: ${config.customCommandConfigured ? "custom command" : config.kind}`);
  if (config.customCommandConfigured) {
    console.log("  custom command: configured (contents hidden)");
  } else if (config.kind === "app") {
    console.log(`  app: ${config.appExecutable || "not found"}`);
  } else {
    console.log(`  CLI: ${config.cliExecutable || `${config.configuredCli} (not found)`}`);
  }
  console.log(`  language: ${config.language || "client default"}`);
  console.log(`  workspace: ${config.workspace}`);
  console.log(`  Codex state: ${config.stateDirectory || "local default"}`);
  console.log(`  app profile: ${config.appProfile || "local default"}`);
}

function resolveCliExecutable(command) {
  const pathLike = isAbsolute(command) || command.includes("/") || command.includes("\\");
  if (pathLike) {
    const resolvedCommand = isAbsolute(command) ? command : resolve(repoRoot, command);
    return firstExecutable(executableVariants(resolvedCommand));
  }

  const pathDirectories = (process.env.PATH || "")
    .split(delimiter)
    .map((directory) => directory.replace(/^"|"$/g, "").trim())
    .filter(Boolean);
  for (const directory of pathDirectories) {
    const executable = firstExecutable(executableVariants(join(directory, command)));
    if (executable) return executable;
  }
  return null;
}

function executableVariants(command) {
  if (!isWindows || extname(command)) return [command];
  const extensions = (process.env.PATHEXT || ".COM;.EXE;.BAT;.CMD")
    .split(";")
    .map((extension) => extension.trim())
    .filter(Boolean);
  return [command, ...extensions.map((extension) => `${command}${extension.toLowerCase()}`)];
}

function firstExecutable(candidates) {
  for (const candidate of candidates) {
    try {
      accessSync(candidate, isWindows ? constants.F_OK : constants.X_OK);
      return candidate;
    } catch {
      // Try the next PATH candidate.
    }
  }
  return null;
}

function runForeground(command) {
  const result = spawnSync(command, {
    cwd: devClientWorkspace,
    env: process.env,
    shell: true,
    stdio: "inherit",
  });
  if (result.error) {
    console.error(`failed to start development client command: ${result.error.message}`);
    process.exit(1);
  }
  if (result.signal) process.kill(process.pid, result.signal);
  process.exit(result.status ?? 0);
}

function envPath(names, fallback) {
  const value = envValue(...names) || fallback;
  return isAbsolute(value) ? value : resolve(repoRoot, value);
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
  if (!value) {
    console.error(`${option} requires a value`);
    process.exit(1);
  }
  return value;
}

function requireNode22() {
  const major = Number.parseInt(process.versions.node.split(".")[0], 10);
  if (major < 22) {
    console.error(`[dev-client] Node.js 22 or newer is required (current: ${process.versions.node}). Run nvm use.`);
    process.exit(1);
  }
}

function printHelp() {
  console.log(`Development ChatGPT/Codex client wrapper

Usage:
  node scripts/devcodex.mjs [--local] [--app|--cli] [-- client args]

Options:
  --local               Use the real local Codex state and app profile
  --app                 Require the ChatGPT/Codex desktop app
  --cli                 Use Codex CLI
  --app-path <path>     Override the .app, .exe, or executable path
  --print-config        Print client discovery without launching
  --help                Show this help

The desktop app discovery order is ChatGPT first, then legacy Codex.
Prefer pnpm dev and its aliases for normal development.`);
}
