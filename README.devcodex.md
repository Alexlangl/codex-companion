# Dev Codex 调试 / Dev Codex Debugging

## 中文

`codex-companion` 的开发模式支持两种 Codex 启动目标：

- `sandbox`：默认模式。启动隔离的 Codex app，不碰真实 `~/.codex`。
- `local`：本机模式。开发时直接使用本机真实 Codex。

### 1. 默认：沙盒 Codex app

直接运行：

```bash
pnpm dev
```

或者：

```bash
yarn dev
```

默认等价于：

```bash
CODEX_COMPANION_DEV_TARGET=sandbox pnpm dev
```

这个模式会让 companion 后续的“启动 Codex / 启动当前分组”动作启动一个隔离的 Codex app。

默认隔离目录位于系统临时目录：

- `DEV_CODEX_HOME`：devcodex 状态目录，macOS/Linux 通常是 `/tmp/devcodex-home`，Windows 通常是 `%TEMP%\devcodex-home`。
- `DEV_CODEX_WORKSPACE`：devcodex 工作目录，macOS/Linux 通常是 `/tmp/devcodex-workspace`，Windows 通常是 `%TEMP%\devcodex-workspace`。
- `DEV_CODEX_APP_DATA`：devcodex app data，macOS/Linux 通常是 `/tmp/devcodex-app-data`，Windows 通常是 `%TEMP%\devcodex-app-data`。
- `DEV_COMPANION_HOME`：dev companion 状态目录，macOS/Linux 通常是 `/tmp/devcodex-companion`，Windows 通常是 `%TEMP%\devcodex-companion`。

沙盒模式会强制写入 devcodex 的 sandbox 配置：

```toml
sandbox_mode = "workspace-write"
approval_policy = "on-request"

[sandbox_workspace_write]
network_access = false
exclude_tmpdir_env_var = false
exclude_slash_tmp = false
```

macOS 下默认启动：

```text
/Applications/Codex.app/Contents/MacOS/Codex
```

沙盒 Codex app 默认会从本机 locale 推断语言，并通过 `--lang` 传给 Codex。

如果需要临时强制语言，可以显式设置：

```bash
DEVCODEX_LANG=zh-CN
```

如果要强制英文：

```bash
DEVCODEX_LANG=en-US pnpm dev
```

显式不传语言参数，让 Codex 自己决定：

```bash
DEVCODEX_LANG=system pnpm dev
```

Windows 下会尝试查找 Codex `.exe`。如果找不到，手动指定：

```powershell
$env:DEVCODEX_APP_PATH="C:\Path\To\Codex.exe"
pnpm dev
```

如果你有单独的 dev Codex app：

```bash
DEVCODEX_APP_PATH=/absolute/path/to/DevCodex.app pnpm dev
```

Windows：

```powershell
$env:DEVCODEX_APP_PATH="C:\Path\To\DevCodex.exe"
pnpm dev
```

### 2. run dev 时立刻启动沙盒 Codex

默认 `pnpm dev` 只启动 companion dev app；当你在 UI 里点“启动 Codex”时才启动沙盒 Codex。

如果想 `pnpm dev` 时就立刻启动沙盒 Codex：

```bash
CODEX_COMPANION_START_DEVCODEX=1 pnpm dev
```

Windows：

```powershell
$env:CODEX_COMPANION_START_DEVCODEX="1"
pnpm dev
```

立刻启动后，UI 按钮仍然会按当前启动模式启动/重启沙盒 Codex。脚本会用 `DEV_CODEX_APP_DATA` 标记沙盒进程，避免误判真实本机 Codex。

如果你只想让 UI 写配置、不自动停止或启动 Codex，可以手动设置：

```bash
CODEX_COMPANION_SKIP_CODEX_RESTART=1 pnpm dev
```

### 3. 本机 Codex 模式

如果你开发时就是想启用本机真实 Codex，而不是沙盒：

```bash
CODEX_COMPANION_DEV_TARGET=local pnpm dev
```

Windows：

```powershell
$env:CODEX_COMPANION_DEV_TARGET="local"
pnpm dev
```

这个模式下：

- companion dev 状态仍然默认写到 `DEV_COMPANION_HOME`。
- Codex 目录不再强制指向 `DEV_CODEX_HOME`。
- Codex 启动命令不再走 sandbox wrapper。
- UI 里的“启动 Codex”会使用本机默认 Codex 行为。

也就是说，`local` 会影响真实本机 Codex 配置和进程，适合你明确要做本机联调时使用。

### 4. CLI 模式

默认沙盒目标是 Codex app，不是 CLI。

如果你确实想调 CLI：

```bash
DEVCODEX_KIND=cli DEVCODEX_BIN=/absolute/path/to/codex pnpm dev
```

或者单独启动：

```bash
DEVCODEX_KIND=cli ./scripts/devcodex.sh
```

### 5. 重要环境变量

- `CODEX_COMPANION_DEV_TARGET`：`sandbox` 或 `local`。默认 `sandbox`。
- `CODEX_COMPANION_START_DEVCODEX`：设为 `1` 时，`pnpm dev` 期间立刻启动沙盒 Codex。
- `CODEX_COMPANION_SKIP_CODEX_RESTART`：设为 `1` 时，companion 写配置但不停止/启动 Codex；默认不设置。
- `CODEX_COMPANION_CODEX_PROCESS_MATCH`：自定义 Codex 启动命令时可设置进程命令行匹配片段，供 UI 判断是否已运行以及停止目标进程；沙盒模式自动设置。
- `DEV_CODEX_HOME`：沙盒 Codex 状态目录。
- `DEV_CODEX_WORKSPACE`：沙盒 Codex 工作目录。
- `DEV_CODEX_APP_DATA`：沙盒 Codex app data 目录。
- `DEV_COMPANION_HOME`：dev companion 状态目录。
- `DEVCODEX_KIND`：`app` 或 `cli`。能找到 app 时默认 `app`。
- `DEVCODEX_APP_PATH`：沙盒 Codex app 路径。macOS 可传 `.app`，Windows 可传 `.exe`。
- `DEVCODEX_LANG`：沙盒 Codex app 语言。默认从本机 locale 推断；可显式设为 `zh-CN`、`en-US` 等，或设为 `system` 表示不传 `--lang`。
- `DEVCODEX_BIN`：CLI 模式下的 Codex binary。
- `DEVCODEX_COMMAND`：完整自定义启动命令。设置后会在 `pnpm dev` 时立刻启动。

macOS 下脚本不会使用 `open -a Codex`，因为那样不可靠地传入 `CODEX_HOME`。它会直接执行 `.app/Contents/MacOS/Codex`，让 app 继承隔离环境变量。

## English

Development mode supports two Codex targets:

- `sandbox`: default. Starts an isolated Codex app and does not touch the real `~/.codex`.
- `local`: uses the real local Codex during development.

### 1. Default: sandbox Codex app

Run:

```bash
pnpm dev
```

Or:

```bash
yarn dev
```

This is equivalent to:

```bash
CODEX_COMPANION_DEV_TARGET=sandbox pnpm dev
```

In this mode, companion's later "Launch Codex" / "Launch Current Group" action starts an isolated Codex app.

Default isolated directories live under the OS temp directory:

- `DEV_CODEX_HOME`: devcodex state, usually `/tmp/devcodex-home` on macOS/Linux and `%TEMP%\devcodex-home` on Windows.
- `DEV_CODEX_WORKSPACE`: devcodex workspace.
- `DEV_CODEX_APP_DATA`: devcodex app data.
- `DEV_COMPANION_HOME`: dev companion state.

Sandbox mode enforces this devcodex config:

```toml
sandbox_mode = "workspace-write"
approval_policy = "on-request"

[sandbox_workspace_write]
network_access = false
exclude_tmpdir_env_var = false
exclude_slash_tmp = false
```

On macOS, the default app executable is:

```text
/Applications/Codex.app/Contents/MacOS/Codex
```

The sandbox Codex app infers the host locale by default and passes it to Codex with `--lang`.

To temporarily force a language, set:

```bash
DEVCODEX_LANG=zh-CN
```

To force English:

```bash
DEVCODEX_LANG=en-US pnpm dev
```

To explicitly pass no language flag and let Codex decide:

```bash
DEVCODEX_LANG=system pnpm dev
```

On Windows, the script tries to find the Codex `.exe`. If needed, set it manually:

```powershell
$env:DEVCODEX_APP_PATH="C:\Path\To\Codex.exe"
pnpm dev
```

For a separate dev Codex app:

```bash
DEVCODEX_APP_PATH=/absolute/path/to/DevCodex.app pnpm dev
```

Windows:

```powershell
$env:DEVCODEX_APP_PATH="C:\Path\To\DevCodex.exe"
pnpm dev
```

### 2. Start sandbox Codex immediately during run dev

By default, `pnpm dev` starts only the companion dev app; sandbox Codex starts when you click "Launch Codex" in the UI.

To start sandbox Codex immediately:

```bash
CODEX_COMPANION_START_DEVCODEX=1 pnpm dev
```

Windows:

```powershell
$env:CODEX_COMPANION_START_DEVCODEX="1"
pnpm dev
```

After immediate startup, UI launch buttons still start or restart the sandbox Codex according to the selected launch mode. The script marks the sandbox process with `DEV_CODEX_APP_DATA` so Companion does not confuse it with the real local Codex.

If you only want UI launch buttons to write config without stopping or starting Codex, set it manually:

```bash
CODEX_COMPANION_SKIP_CODEX_RESTART=1 pnpm dev
```

### 3. Local Codex mode

To use the real local Codex during development:

```bash
CODEX_COMPANION_DEV_TARGET=local pnpm dev
```

Windows:

```powershell
$env:CODEX_COMPANION_DEV_TARGET="local"
pnpm dev
```

In this mode:

- Companion dev state still defaults to `DEV_COMPANION_HOME`.
- Codex is not forced to `DEV_CODEX_HOME`.
- Codex launch does not use the sandbox wrapper.
- UI launch buttons use the normal local Codex behavior.

`local` can affect your real local Codex config and process. Use it only when you intentionally want local integration testing.

### 4. CLI mode

The default sandbox target is the Codex app, not CLI.

To use CLI mode:

```bash
DEVCODEX_KIND=cli DEVCODEX_BIN=/absolute/path/to/codex pnpm dev
```

Or start it directly:

```bash
DEVCODEX_KIND=cli ./scripts/devcodex.sh
```

### 5. Important environment variables

- `CODEX_COMPANION_DEV_TARGET`: `sandbox` or `local`. Defaults to `sandbox`.
- `CODEX_COMPANION_START_DEVCODEX`: set to `1` to start sandbox Codex immediately during `pnpm dev`.
- `CODEX_COMPANION_SKIP_CODEX_RESTART`: set to `1` to let companion write config without stopping or starting Codex; unset by default.
- `CODEX_COMPANION_CODEX_PROCESS_MATCH`: optional process command-line substring for custom Codex launch commands; UI launch buttons use it to detect and stop the target process. Sandbox mode sets it automatically.
- `DEV_CODEX_HOME`: sandbox Codex state directory.
- `DEV_CODEX_WORKSPACE`: sandbox Codex workspace directory.
- `DEV_CODEX_APP_DATA`: sandbox Codex app data directory.
- `DEV_COMPANION_HOME`: dev companion state directory.
- `DEVCODEX_KIND`: `app` or `cli`. Defaults to `app` when an app is found.
- `DEVCODEX_APP_PATH`: sandbox Codex app path. Use `.app` on macOS or `.exe` on Windows.
- `DEVCODEX_LANG`: sandbox Codex app language. Defaults to the inferred host locale; set it explicitly to `zh-CN`, `en-US`, etc., or set `system` to pass no `--lang`.
- `DEVCODEX_BIN`: Codex binary for CLI mode.
- `DEVCODEX_COMMAND`: full custom launch command. When set, it starts immediately during `pnpm dev`.

On macOS, the scripts do not use `open -a Codex`, because that does not reliably pass `CODEX_HOME`. They execute `.app/Contents/MacOS/Codex` directly so the app inherits the isolated environment.
