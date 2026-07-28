# Codex Companion Development

[中文](#中文) | [English](#english)

## 中文

### 快速启动

需要 Node.js 22、pnpm 10.23、稳定版 Rust 工具链，以及当前系统对应的 [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/)。仓库提供了 `.nvmrc`，使用 nvm 时可直接运行：

```bash
nvm use
corepack enable
pnpm install --frozen-lockfile
pnpm dev
```

`pnpm dev` 会同时启动：

- 使用隔离数据目录的 Codex Companion 开发版；
- 自动发现的 ChatGPT 桌面 App；
- 如果没有桌面 App，则回退到 Codex CLI。

macOS 会优先使用 `/Applications/ChatGPT.app`，同时兼容旧的 `Codex.app`。正常开发不再需要预先设置一组环境变量。

### 启动模式

| 命令 | Companion 数据 | ChatGPT / Codex 数据 | 是否立即启动客户端 |
| --- | --- | --- | --- |
| `pnpm dev` | 隔离 | 隔离 | 是 |
| `pnpm dev:companion` | 隔离 | 隔离 | 否；桌面 App 可稍后从 UI 启动 |
| `pnpm dev:local` | 隔离 | 使用本机配置与 App profile | 是 |
| `pnpm dev:cli` | 隔离 | 隔离 | 是；在当前终端启动 Codex CLI |

首次排查环境时先运行：

```bash
pnpm dev --dry-run
```

它只输出解析后的模式、路径、客户端类型和可执行文件，不会启动 ChatGPT、Codex CLI 或 Tauri。

`pnpm dev:local` 会让开发版 Companion 操作本机 Codex 目录，并启动真实的 ChatGPT / Codex profile。涉及安装、切换或修复配置时可能修改本机 `~/.codex`；日常开发优先使用默认的沙盒模式。

桌面 App 可以由 Companion 根据隔离 profile 或进程名安全地启动和重启。交互式 CLI 与任意 `--command` 默认只在开发命令启动时运行，Companion UI 不会自动启停它们；CLI 独占当前终端输入，切换配置后请手动重启。自定义命令只有在同时设置精确的 `CODEX_COMPANION_CLIENT_PROCESS_MATCH` 或 `CODEX_COMPANION_CLIENT_APP_NAME` 时才启用自动管理。

### 自动发现与隔离目录

桌面客户端发现顺序：

1. `--app-path` 或 `DEV_CLIENT_APP_PATH` 指定的位置；
2. macOS 的 `/Applications/ChatGPT.app`、`~/Applications/ChatGPT.app`；
3. 旧的 `Codex.app`；
4. Windows 下常见的 ChatGPT 和旧 Codex 安装位置；
5. 找不到桌面 App 时，在 `auto` 模式回退到 PATH 中的 `codex` CLI。

默认沙盒位于系统临时目录的 `codex-companion-dev/` 下：

| 目录 | 用途 |
| --- | --- |
| `companion-home/` | Companion 配置、认证材料与数据库 |
| `client-home/` | 隔离的 `CODEX_HOME` / `CODEX_SQLITE_HOME` |
| `client-app-data/` | ChatGPT / Codex Electron profile |
| `workspace/` | 客户端启动工作目录 |

启动器会自动创建目录和最小沙盒配置。macOS 下直接执行 `.app/Contents/MacOS/ChatGPT`，确保隔离环境和 `--user-data-dir` 能传给客户端进程。

### 常用参数

```bash
# 查看完整帮助
pnpm dev --help

# 指定 ChatGPT.app；相对路径按仓库根目录解析
pnpm dev --app --app-path "/Applications/ChatGPT.app"

# 使用系统语言，不注入 --lang
pnpm dev --lang system

# 修改隔离根目录和工作目录
pnpm dev --dev-root .dev-data --workspace ./fixtures/workspace

# 使用另一个 Codex CLI
pnpm dev:cli --cli-bin /absolute/path/to/codex

# 把剩余参数交给 tauri dev
pnpm dev --tauri --release
```

| 参数 | 用途 |
| --- | --- |
| `--sandbox` / `--local` | 选择隔离或本机客户端状态 |
| `--app` / `--cli` | 强制使用桌面 App 或 CLI |
| `--start-client` / `--no-start-client` | 控制是否立即启动客户端 |
| `--app-path <path>` | 指定 ChatGPT / 旧 Codex 的 `.app`、`.exe` 或可执行文件 |
| `--cli-bin <path>` | 指定 Codex CLI 可执行文件 |
| `--command <command>` | 使用完整的自定义客户端启动命令 |
| `--lang <locale>` | 设置 `zh-CN`、`en-US` 等语言；`system` 表示不注入 |
| `--dev-root <path>` | 修改所有默认隔离数据的根目录 |
| `--workspace <path>` | 修改客户端启动工作目录 |
| `--host <address>` / `--port <number>` | 设置 Vite 地址和首选端口 |
| `--skip-client-restart` | 写配置时不停止或重启客户端 |
| `--dry-run` | 只检查解析结果和客户端发现 |
| `--tauri <args...>` | 把剩余参数传给 `tauri dev` |

pnpm 9/10 的 `pnpm dev --help` 和 `pnpm dev -- --help` 写法都受支持。

### 环境变量兼容

命令行参数适合本地开发；环境变量仍可用于 CI、IDE launch configuration 和自定义脚本。

| 当前名称 | 用途 |
| --- | --- |
| `CODEX_COMPANION_DEV_TARGET` | `sandbox` 或 `local` |
| `CODEX_COMPANION_DEV_ROOT` | 默认隔离目录的根路径 |
| `DEV_COMPANION_HOME` | Companion 开发数据目录 |
| `DEV_CLIENT_HOME` | 隔离的 Codex 状态目录 |
| `DEV_CLIENT_WORKSPACE` | 客户端工作目录 |
| `DEV_CLIENT_APP_DATA` | 隔离的桌面 App profile |
| `DEV_CLIENT_KIND` | `auto`、`app` 或 `cli` |
| `DEV_CLIENT_APP_PATH` | ChatGPT / 旧 Codex App 路径 |
| `DEV_CLIENT_LANG` | 客户端语言 |
| `DEV_CLIENT_BIN` | Codex CLI 可执行文件 |
| `DEV_CLIENT_COMMAND` | 完整自定义启动命令 |
| `CODEX_COMPANION_START_CLIENT` | `1` 表示立即启动客户端 |
| `CODEX_COMPANION_SKIP_CLIENT_RESTART` | `1` 表示不自动重启客户端 |
| `CODEX_COMPANION_CLIENT_PROCESS_MATCH` | 自定义命令的精确进程命令行匹配文本 |
| `CODEX_COMPANION_CLIENT_APP_NAME` | 自定义桌面命令对应的精确进程名 |
| `CODEX_COMPANION_DEV_HOST` / `CODEX_COMPANION_DEV_PORT` | Vite 地址和首选端口 |

旧的 `DEV_CODEX_*`、`DEVCODEX_*`、`CODEX_COMPANION_START_DEVCODEX`、`CODEX_COMPANION_SKIP_CODEX_RESTART` 和 `CODEX_COMPANION_*_CODEX_*` 名称仍然可用，但新脚本和文档统一使用 `CLIENT` 命名。`CODEX_HOME`、`CODEX_SQLITE_HOME` 与 `CODEX_COMPANION_CODEX_DIR` 仍保留原名，因为它们指向的配置目录依然是 `.codex`。

### 常见问题

- **提示 Node 版本不匹配**：运行 `nvm use`，确认 `node --version` 为 22 或更高。
- **没有找到桌面 App**：运行 `pnpm dev --dry-run` 检查发现结果，再用 `--app-path` 指定位置；也可以使用 `pnpm dev:cli`。
- **1420 端口被占用**：开发脚本会自动向后查找最多 100 个端口，并把最终地址传给 Tauri。
- **只想调试 Companion UI**：使用 `pnpm dev:companion`，需要时再从 UI 启动隔离客户端。
- **CLI 切换配置后没有自动重启**：这是预期行为；开发启动器不会模糊匹配并终止其他 `codex` 进程，请在当前终端手动退出并重新运行 CLI。
- **需要直接运行客户端包装器**：使用 `node scripts/devcodex.mjs --print-config` 检查发现结果，或使用 `scripts/devcodex.sh` 启动。文件名暂时保留用于兼容旧开发脚本，实际会优先启动 ChatGPT。

## English

### Quick Start

You need Node.js 22, pnpm 10.23, the stable Rust toolchain, and the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform. The repository includes `.nvmrc`; with nvm installed, run:

```bash
nvm use
corepack enable
pnpm install --frozen-lockfile
pnpm dev
```

`pnpm dev` starts both:

- an isolated Codex Companion development instance;
- the automatically discovered ChatGPT desktop app;
- or Codex CLI when no desktop app is available.

On macOS, `/Applications/ChatGPT.app` takes precedence while legacy `Codex.app` installations remain supported. Normal development no longer requires a set of environment variables.

### Launch Modes

| Command | Companion data | ChatGPT / Codex data | Starts the client now |
| --- | --- | --- | --- |
| `pnpm dev` | Isolated | Isolated | Yes |
| `pnpm dev:companion` | Isolated | Isolated | No; a desktop app can be launched later from the UI |
| `pnpm dev:local` | Isolated | Local config and app profile | Yes |
| `pnpm dev:cli` | Isolated | Isolated | Yes; starts Codex CLI in the current terminal |

When diagnosing a machine for the first time, start with:

```bash
pnpm dev --dry-run
```

It prints the resolved mode, paths, client kind, and executable without starting ChatGPT, Codex CLI, or Tauri.

`pnpm dev:local` lets the development build operate on the local Codex directory and starts the real ChatGPT / Codex profile. Installing, switching, or repairing configuration may modify local `~/.codex` data; prefer the default sandbox for routine development.

Companion can safely start and restart desktop apps by matching an isolated profile or an exact process name. Interactive CLI sessions and arbitrary `--command` values run only when the development command starts; Companion UI actions do not automatically stop or relaunch them. The CLI owns the current terminal input, so restart it manually after configuration changes. Automatic management for a custom command is enabled only when an exact `CODEX_COMPANION_CLIENT_PROCESS_MATCH` or `CODEX_COMPANION_CLIENT_APP_NAME` is also set.

### Discovery and Isolation

Desktop client discovery order:

1. The location supplied through `--app-path` or `DEV_CLIENT_APP_PATH`.
2. `/Applications/ChatGPT.app` and `~/Applications/ChatGPT.app` on macOS.
3. Legacy `Codex.app` installations.
4. Common ChatGPT and legacy Codex installation paths on Windows.
5. The `codex` CLI on PATH when no app is found in `auto` mode.

The default sandbox lives under `codex-companion-dev/` in the system temporary directory:

| Directory | Purpose |
| --- | --- |
| `companion-home/` | Companion configuration, credentials, and databases |
| `client-home/` | Isolated `CODEX_HOME` / `CODEX_SQLITE_HOME` |
| `client-app-data/` | ChatGPT / Codex Electron profile |
| `workspace/` | Client launch working directory |

The launcher creates the directories and a minimal sandbox configuration automatically. On macOS it executes `.app/Contents/MacOS/ChatGPT` directly so the isolated environment and `--user-data-dir` reach the client process.

### Common Options

```bash
# Show complete help
pnpm dev --help

# Select ChatGPT.app; relative paths resolve from the repository root
pnpm dev --app --app-path "/Applications/ChatGPT.app"

# Keep the client language unchanged
pnpm dev --lang system

# Move the sandbox root and launch workspace
pnpm dev --dev-root .dev-data --workspace ./fixtures/workspace

# Select another Codex CLI binary
pnpm dev:cli --cli-bin /absolute/path/to/codex

# Forward the remaining arguments to tauri dev
pnpm dev --tauri --release
```

| Option | Purpose |
| --- | --- |
| `--sandbox` / `--local` | Select isolated or local client state |
| `--app` / `--cli` | Require the desktop app or Codex CLI |
| `--start-client` / `--no-start-client` | Control immediate client launch |
| `--app-path <path>` | Set the ChatGPT / legacy Codex `.app`, `.exe`, or executable |
| `--cli-bin <path>` | Set the Codex CLI executable |
| `--command <command>` | Use a complete custom client launch command |
| `--lang <locale>` | Set a locale such as `zh-CN` or `en-US`; `system` injects nothing |
| `--dev-root <path>` | Move all default isolated development data |
| `--workspace <path>` | Change the client launch working directory |
| `--host <address>` / `--port <number>` | Set the Vite address and preferred port |
| `--skip-client-restart` | Write configuration without stopping or restarting the client |
| `--dry-run` | Inspect resolution and client discovery only |
| `--tauri <args...>` | Forward the remaining arguments to `tauri dev` |

Both `pnpm dev --help` and `pnpm dev -- --help` are accepted with pnpm 9/10.

### Environment Compatibility

CLI options are intended for local development. Environment variables remain available for CI, IDE launch configurations, and custom scripts.

| Current name | Purpose |
| --- | --- |
| `CODEX_COMPANION_DEV_TARGET` | `sandbox` or `local` |
| `CODEX_COMPANION_DEV_ROOT` | Root for the default isolated directories |
| `DEV_COMPANION_HOME` | Companion development data directory |
| `DEV_CLIENT_HOME` | Isolated Codex state directory |
| `DEV_CLIENT_WORKSPACE` | Client working directory |
| `DEV_CLIENT_APP_DATA` | Isolated desktop app profile |
| `DEV_CLIENT_KIND` | `auto`, `app`, or `cli` |
| `DEV_CLIENT_APP_PATH` | ChatGPT / legacy Codex app path |
| `DEV_CLIENT_LANG` | Client locale |
| `DEV_CLIENT_BIN` | Codex CLI executable |
| `DEV_CLIENT_COMMAND` | Complete custom launch command |
| `CODEX_COMPANION_START_CLIENT` | `1` starts the client immediately |
| `CODEX_COMPANION_SKIP_CLIENT_RESTART` | `1` disables automatic client restarts |
| `CODEX_COMPANION_CLIENT_PROCESS_MATCH` | Exact process-command match for a custom command |
| `CODEX_COMPANION_CLIENT_APP_NAME` | Exact process name for a custom desktop command |
| `CODEX_COMPANION_DEV_HOST` / `CODEX_COMPANION_DEV_PORT` | Vite address and preferred port |

Legacy `DEV_CODEX_*`, `DEVCODEX_*`, `CODEX_COMPANION_START_DEVCODEX`, `CODEX_COMPANION_SKIP_CODEX_RESTART`, and `CODEX_COMPANION_*_CODEX_*` names remain supported. New scripts and documentation use `CLIENT` consistently. `CODEX_HOME`, `CODEX_SQLITE_HOME`, and `CODEX_COMPANION_CODEX_DIR` keep their existing names because the underlying configuration directory is still `.codex`.

### Troubleshooting

- **Node version mismatch**: run `nvm use` and confirm that `node --version` is 22 or newer.
- **Desktop app not found**: inspect `pnpm dev --dry-run`, then provide `--app-path`; `pnpm dev:cli` is also available.
- **Port 1420 is busy**: the development script searches up to 100 subsequent ports and passes the selected URL to Tauri.
- **Only the Companion UI is needed**: use `pnpm dev:companion`, then launch the isolated client from the UI when needed.
- **The CLI did not restart after a configuration switch**: this is expected. The launcher does not broadly match and terminate other `codex` processes; exit and restart the CLI in the current terminal.
- **Direct client wrapper access**: run `node scripts/devcodex.mjs --print-config` to inspect discovery, or use `scripts/devcodex.sh` to launch it. The legacy filename remains for compatibility, but ChatGPT is preferred at runtime.
