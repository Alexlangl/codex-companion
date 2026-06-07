#!/usr/bin/env node
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const host = process.env.CODEX_COMPANION_DEV_HOST || "127.0.0.1";
const preferredPort = parsePort(process.env.CODEX_COMPANION_DEV_PORT, 1420);
const port = await findAvailablePort(preferredPort, host);
const devUrl = `http://${host}:${port}`;
const tauriConfig = JSON.stringify({
  build: {
    devUrl,
  },
});

if (port === preferredPort) {
  console.log(`[codex-companion] dev UI: ${devUrl}`);
} else {
  console.log(`[codex-companion] dev UI: ${devUrl} (preferred ${preferredPort} was busy)`);
}

if (process.env.CODEX_COMPANION_DEV_DRY_RUN === "1") {
  console.log("[codex-companion] dry run; tauri dev was not started");
  process.exit(0);
}

const child = spawn(
  "pnpm",
  ["exec", "tauri", "dev", "--config", tauriConfig, ...process.argv.slice(2)],
  {
    cwd: appDir,
    env: {
      ...process.env,
      CODEX_COMPANION_DEV_HOST: host,
      CODEX_COMPANION_DEV_PORT: String(port),
      CODEX_COMPANION_DEV_STRICT_PORT: "1",
    },
    stdio: "inherit",
  },
);

let forwardedSignal = false;
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    forwardedSignal = true;
    child.kill(signal);
  });
}

child.on("exit", (code, signal) => {
  if (signal && !forwardedSignal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 0);
});

child.on("error", (error) => {
  console.error(`[codex-companion] failed to start tauri dev: ${error.message}`);
  process.exit(1);
});

function parsePort(value, fallback) {
  if (!value) return fallback;
  const parsed = Number.parseInt(value, 10);
  if (Number.isInteger(parsed) && parsed > 0 && parsed < 65536) {
    return parsed;
  }
  console.warn(`[codex-companion] ignored invalid CODEX_COMPANION_DEV_PORT=${value}`);
  return fallback;
}

async function findAvailablePort(startPort, listenHost) {
  for (let portToTry = startPort; portToTry < startPort + 100; portToTry += 1) {
    if (await canListen(portToTry, listenHost)) {
      return portToTry;
    }
  }
  throw new Error(`No available dev port found from ${startPort} to ${startPort + 99}`);
}

function canListen(portToTry, listenHost) {
  return new Promise((resolveCanListen) => {
    const server = createServer();
    server.once("error", () => resolveCanListen(false));
    server.once("listening", () => {
      server.close(() => resolveCanListen(true));
    });
    server.listen(portToTry, listenHost);
  });
}
