import react from "@vitejs/plugin-react";
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { defineConfig } from "vite";

const workspaceRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const devHost = process.env.CODEX_COMPANION_DEV_HOST ?? "127.0.0.1";
const devPort = parseDevPort(process.env.CODEX_COMPANION_DEV_PORT, 1420);
const strictDevPort = process.env.CODEX_COMPANION_DEV_STRICT_PORT === "1";

export default defineConfig({
  plugins: [react(), tokenUsageDevEndpoint()],
  clearScreen: false,
  server: {
    host: devHost,
    port: devPort,
    strictPort: strictDevPort,
  },
  build: {
    outDir: "dist",
    target: "es2022",
  },
});

function tokenUsageDevEndpoint() {
  return {
    name: "codex-companion-token-usage-dev-endpoint",
    configureServer(server) {
      server.middlewares.use("/__codex_companion__/token-usage", (request, response) => {
        const url = new URL(request.url ?? "/", "http://127.0.0.1");
        const codexDir = expandHome(url.searchParams.get("codexDir") || "~/.codex");
        const command = tokenStatsCommand(codexDir);
        execFile(
          command.bin,
          command.args,
          { cwd: workspaceRoot, maxBuffer: 32 * 1024 * 1024 },
          (error, stdout, stderr) => {
            response.setHeader("content-type", "application/json; charset=utf-8");
            if (error) {
              response.statusCode = 500;
              response.end(JSON.stringify({ error: stderr || error.message }));
              return;
            }
            response.end(stdout);
          },
        );
      });
    },
  };
}

function tokenStatsCommand(codexDir: string) {
  const debugBinary = resolve(workspaceRoot, "target/debug/codex-companion");
  if (existsSync(debugBinary)) {
    return {
      bin: debugBinary,
      args: ["token-stats", "--codex-dir", codexDir],
    };
  }
  return {
    bin: "cargo",
    args: ["run", "-p", "codex-companion-cli", "--", "token-stats", "--codex-dir", codexDir],
  };
}

function expandHome(path: string) {
  if (path === "~") return process.env.HOME ?? path;
  if (path.startsWith("~/")) return resolve(process.env.HOME ?? ".", path.slice(2));
  return path;
}

function parseDevPort(value: string | undefined, fallback: number) {
  if (!value) return fallback;
  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed > 0 && parsed < 65536 ? parsed : fallback;
}
