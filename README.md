# Codex Companion

> English version: [README.en.md](README.en.md)

Codex Companion 是 Codex Desktop / CLI 的本地 provider runtime。

Codex Desktop / CLI 只需要配置一个稳定的本地 `base_url`。Companion 在本地统一承接官方 Codex 账号、API Key 中转、OpenAI-compatible provider、relay provider、provider group、fallback、cooldown、健康刷新、token 用量统计，以及 Codex 历史会话/插件 namespace 修复。

> 下方截图使用的是浏览器预览里的脱敏 demo 数据，不包含真实账号 token 或真实 Codex 用量。

### 它解决什么

Codex 原生 provider 配置适合单个 provider，但不适合在官方账号、中转站、第三方 OpenAI-compatible provider 之间热切换。Codex Companion 把这些 provider 收到一个本地 runtime 里：

```text
Codex Desktop / CLI
        |
        v
http://127.0.0.1:17687/v1
        |
        v
Codex Companion Relay
        |
        v
official_codex / openai_compatible / relay_provider
```

核心目标：

- 使用 Provider Group 时，Codex 固定指向 `codex-companion` 本地 `base_url`。
- group 内 provider 按优先级 fallback、cooldown、健康刷新和热切换，不需要重启 Codex。
- 单 Provider 可选择直连中转站或走本地代理。
- 官方 Codex 账号必须走 Companion relay，因为它需要 Companion 维护 OAuth token、refresh token、`ChatGPT-Account-Id` 和 Codex 请求头。
- 通过 Companion 启动 Codex 前，自动 dry-run/repair 历史会话和插件 provider namespace，避免上下文断裂。

### 功能截图

#### 总览

查看当前分组、本地转发地址、可用账号和 Codex 接入状态。

<img src="assets/readme/dashboard.jpg" alt="Codex Companion dashboard" width="720">

#### 账号

紧凑展示大量 provider，支持刷新健康度、切换启动方式、直连或本地代理。

<img src="assets/readme/providers-compact.jpg" alt="Provider compact list" width="720">

#### 添加账号

支持 API Key、Token / JSON、CPA/sub2api/cockpit JSON 批量导入，以及本机 Codex 账号导入。

<img src="assets/readme/provider-add-dialog.jpg" alt="Add provider dialog" width="720">

#### 分组

按顺序编排 provider，fallback 顺序以 `provider_order` 为准。

<img src="assets/readme/groups.jpg" alt="Provider groups" width="720">

#### 转发

查看本地 relay 地址、启动模式说明和请求/fallback/错误日志。

<img src="assets/readme/relay.jpg" alt="Relay page" width="720">

#### 修复

对历史会话和插件状态做 dry-run、备份和 namespace 修复，目标 provider 可选。

<img src="assets/readme/repair.jpg" alt="Repair page" width="720">

#### 用量

扫描 Codex session JSONL，按日期、模型和 provider 聚合 token 使用量。

<img src="assets/readme/token-usage.jpg" alt="Token usage page" width="720">

### 支持的 provider

| kind | 用途 | 是否可直连 |
| --- | --- | --- |
| `official_codex` | Codex 官方账号，来自 CPA/sub2api/cockpit OAuth JSON 或本机 Codex auth | 否，必须通过 Companion relay |
| `openai_compatible` | OpenRouter、New API、中转站、自建 OpenAI-compatible API | 可以直连，也可以走本地代理 |
| `relay_provider` | 已经是另一个本地/远端 relay 的 provider | 视认证方式决定 |

API Key JSON 导入支持类似下面的格式：

```json
{
  "auth_mode": "apikey",
  "OPENAI_API_KEY": "sk-...",
  "email": "api-key-demo",
  "api_base_url": "https://api.example.com/v1",
  "api_provider_id": "example",
  "api_provider_name": "Example API"
}
```

### 启动模式

| 模式 | Codex 配置 | repair 目标 | 适用场景 |
| --- | --- | --- | --- |
| Provider Group / Relay | `codex-companion` 本地 `base_url` | `codex-companion` | 多 provider fallback、cooldown、热切换 |
| Single Provider / Direct | provider 自己的 `base_url + env api key` | provider id | API Key provider 单独直连中转站 |
| Single Provider / Relay | `codex-companion` 本地 `base_url` | `codex-companion` | 官方账号，或密钥由 Companion 本地文件注入的 provider |

Relay fallback 规则很明确：只有在 stream 尚未开始写回 Codex 前才会自动切换到下一个 provider。一旦第一个 stream chunk 已经返回，就不会静默 fallback，避免破坏当前上下文流。

### 目录结构

```text
apps/desktop          Tauri v2 + React + Vite + Radix UI
crates/core           共享类型、配置、路径、错误
crates/provider       provider registry、JSON 导入、账号刷新、group 选择
crates/relay          本地 HTTP relay、proxy、stream passthrough、fallback
crates/health         错误分类、健康状态、cooldown
crates/state          Codex 安装、doctor、history/plugin repair、token usage
crates/daemon         App / CLI / TUI 共用 runtime facade
crates/cli            命令行入口
crates/tui            ratatui 终端入口
ARCHITECTURE.md       架构细节
```

### 开发

需要 Node 22：

```bash
source ~/.nvm/nvm.sh
nvm use 22
pnpm install
```

启动桌面开发版：

```bash
pnpm dev
```

`pnpm dev` 会优先使用 `127.0.0.1:1420`。如果端口被占用，开发脚本会自动寻找后续空端口，并把同一个 dev URL 注入给 Tauri 和 Vite。正式打包使用内置前端资源，不会占用 1420。本地 relay 默认端口是 `17687`。

指定开发端口：

```bash
CODEX_COMPANION_DEV_PORT=15000 pnpm dev
```

检查和测试：

```bash
pnpm check
pnpm build
cargo check --workspace
cargo test --workspace
```

### CLI

```bash
cargo run -p codex-companion-cli -- status
cargo run -p codex-companion-cli -- install
cargo run -p codex-companion-cli -- doctor

cargo run -p codex-companion-cli -- provider import --json-file ./account.json
cargo run -p codex-companion-cli -- provider import-local
cargo run -p codex-companion-cli -- provider refresh-all
cargo run -p codex-companion-cli -- provider list

cargo run -p codex-companion-cli -- group create \
  --id daily \
  --name Daily \
  --providers openrouter_demo,codex_team
cargo run -p codex-companion-cli -- group use daily

cargo run -p codex-companion-cli -- repair \
  --history \
  --plugins \
  --dry-run \
  --target-provider-id codex-companion

cargo run -p codex-companion-cli -- token-stats
cargo run -p codex-companion-cli -- relay start
```

### TUI

```bash
cargo run -p codex-companion-tui
```

TUI 支持账号导入/刷新、分组编排、repair、token 扫描和启动动作。快捷键在 TUI 内按 `?` 查看。

### 数据与安全边界

- Companion 配置保存在自己的数据目录。
- 普通 provider 配置只保存 `auth_ref`，不会把完整 token 直接塞进 provider 列表。
- API key 可以使用 `env:<VAR>`，也可以保存到 Companion 本地 auth 文件后由 relay 注入。
- repair 正式写入前会先生成计划；正式执行会创建备份。
- token usage 只读取本机 Codex session JSONL，不做价格估算。
