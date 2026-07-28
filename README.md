<div align="center">

<img src="apps/desktop/src-tauri/icons/icon.png" width="112" alt="Codex Companion logo" />

# Codex Companion

**为 Codex 打造的本地账号池、兼容网关与会话连续性工具。**

让 ChatGPT 桌面端内的 Codex、旧版 Codex Desktop 和 Codex CLI 只连接一个本地入口，Companion 在背后管理官方账号、API Key 中转、OpenAI-compatible provider、分组路由、故障切换与用量统计。

<p>
  <a href="https://github.com/Alexlangl/codex-companion/releases"><img alt="GitHub Release" src="https://img.shields.io/github/v/release/Alexlangl/codex-companion?style=flat" /></a>
  <a href="https://github.com/Alexlangl/codex-companion/releases"><img alt="GitHub Downloads" src="https://img.shields.io/github/downloads/Alexlangl/codex-companion/total?style=flat" /></a>
  <img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-5b6573?style=flat" />
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/github/license/Alexlangl/codex-companion?style=flat" /></a>
  <a href="https://tauri.app/"><img alt="Built with Tauri 2" src="https://img.shields.io/badge/Tauri-2-24C8DB?style=flat" /></a>
</p>

简体中文 · [English](README.en.md)

</div>

> Codex Companion 是本地优先工具。账号材料、请求审计和会话统计保存在你的设备上；请求正文不会写入审计日志。

<img src="assets/readme/dashboard.jpg" alt="Codex Companion 总览" width="100%" />

## 为什么选择 Codex Companion

很多工具解决的是“把当前配置切到另一个 API”。Codex Companion 更关注切换之后的运行过程：一个 Codex 可以使用账号池，在请求开始前选路、失败时切换上游、保持会话亲和，并在本地看到健康度、额度、请求记录与 token 成本。

- **专注 Codex**：围绕 ChatGPT / Codex Desktop 与 Codex CLI 的真实配置、登录材料、Responses API 和会话文件工作。
- **账号池而不是单一配置**：官方 OAuth、Agent Identity、API Key 和第三方中转可以放进同一个分组。
- **请求级可靠性**：支持优先级、轮询、随机、权重、最低负载、手动选择、模型冷却和失败切换。
- **切换时保留上下文**：提供会话亲和、历史 namespace 修复、插件状态修复和 `codex resume` 入口。
- **本地可观测**：查看账号健康、可识别额度、结构化请求审计，以及主会话和子代理的 token 用量。
- **三种使用界面**：桌面 App 负责日常操作，CLI 适合脚本，TUI 适合纯终端环境。

### 从配置切换到运行时管理

| 关注点 | 只切换 Codex 配置 | 使用 Companion 本地代理 |
| --- | --- | --- |
| 上游 | 当前只使用一个 provider | 一个分组可以包含多个账号与 provider |
| 切换时机 | 改写配置后重启 Codex | 请求开始前选路，失败时自动尝试后备账号 |
| 会话 | 切换后由调用方自行处理 | 稳定会话保持账号亲和，失败后重新绑定 |
| 协议 | 依赖上游直接兼容 Codex | 可在 Responses 与 Chat Completions 之间转换 |
| 运行状态 | 通常只看到最终错误 | 健康度、额度、冷却、尝试次数与请求审计集中展示 |

如果你只需要长期使用一个固定的 `base_url` 和 API Key，直接配置 Codex 已经足够；如果你需要多账号、热切换、失败恢复或本地 API 服务，Companion 才会体现价值。

## 快速开始

1. 从 [GitHub Releases](https://github.com/Alexlangl/codex-companion/releases) 安装桌面 App，或在 macOS 上运行 `brew install --cask Alexlangl/tap/codex-companion`。
2. 打开 `账号` → `添加账号`，导入本机 Codex 登录、粘贴 Token / JSON，或填写 API Key 与 Base URL。
3. 在账号卡片上选择 `自动`、`直连` 或 `本地代理` 后启动；需要多个账号时，到 `分组` 中编排顺序与调度策略，再启动分组。

推荐选择：

| 场景 | 启动方式 |
| --- | --- |
| 单个官方账号或标准 API Key，希望最短链路 | `自动` 或 `直连` |
| 多账号故障切换、热切换或会话亲和 | `本地代理` / 启动分组 |
| 上游只提供 `/chat/completions` | `本地代理`，由 Companion 转换协议 |
| Agent Identity | `本地代理`，由 Companion 动态签名 |

直连会更新 Codex 配置，并可能写入 `auth.json`，启动后需要 ChatGPT / Codex 重新载入；本地代理让 Codex 固定连接 Companion，切换账号和分组时无需重启。

## 核心能力

### 账号与 Provider

- 导入官方 Codex、Codex Companion / CPA、sub2api、Agent Identity、API Key 和 New API 连接 JSON。
- 支持多文件批量导入，逐项报告结果，并可把成功项直接加入当前分组。
- 自动识别可用的订阅、额度窗口和重置时间；瞬时刷新失败会保留上次成功快照。
- 单账号可独立选择直连或本地代理，也可以导出为可迁移 JSON。

### 分组路由与可靠性

- 提供优先级失败切换、轮询、随机、加权、最低负载和手动选择六种策略。
- 稳定 session ID 保持账号亲和；绑定账号失败后自动切换并重新绑定。
- 404 / 429 等模型级失败只冷却“账号 + 模型”，不会误伤该账号的其他模型。
- SSE 只在尚未向客户端输出有效内容时重试，避免重复文本或重复工具调用。

### 本地 Responses API

- 暴露 HTTP 与 WebSocket `/v1/responses`、`/v1/responses/compact` 和 `/v1/models`。
- 为不同调用方创建独立 API client，支持模型白名单、停用、轮换与删除。
- 将 Responses 请求转换到 Chat Completions 上游，并处理流式事件、工具调用和多轮历史。
- 请求审计只保存路由元数据，不保存提示词、响应正文或完整密钥。

### 会话、修复与用量

- 搜索本地 Codex 会话，复制或直接执行 `codex resume SESSION_ID`。
- Dry-run 预览历史与插件修复；正式修复使用事务备份，失败时自动回滚。
- 扫描主会话和子代理的 `token_count`，按日期、provider 和模型筛选。
- 分开统计新输入、缓存读取、缓存写入和输出，并基于可覆盖价格表估算成本。

## 界面预览

<table>
  <tr>
    <td width="50%"><strong>账号池</strong><br />导入、刷新、导出并选择账号启动方式。</td>
    <td width="50%"><strong>分组</strong><br />编排账号顺序、权重、fallback 与 failback。</td>
  </tr>
  <tr>
    <td><img src="assets/readme/providers-compact.jpg" alt="Codex Companion 账号列表" /></td>
    <td><img src="assets/readme/groups.jpg" alt="Codex Companion 分组编排" /></td>
  </tr>
  <tr>
    <td><strong>本地 API</strong><br />查看监听状态、API client、冷却与请求审计。</td>
    <td><strong>Token 用量</strong><br />按时间、provider 和模型分析本地会话。</td>
  </tr>
  <tr>
    <td><img src="assets/readme/relay.jpg" alt="Codex Companion 本地 API" /></td>
    <td><img src="assets/readme/token-usage.jpg" alt="Codex Companion Token 用量" /></td>
  </tr>
  <tr>
    <td><strong>批量导入</strong><br />支持 Token / JSON、本机账号和分组导入。</td>
    <td><strong>会话修复</strong><br />先 dry-run，再执行可回滚的修复。</td>
  </tr>
  <tr>
    <td><img src="assets/readme/provider-add-dialog.jpg" alt="Codex Companion 添加账号" /></td>
    <td><img src="assets/readme/repair.jpg" alt="Codex Companion 会话修复" /></td>
  </tr>
</table>

> 截图使用脱敏 demo 数据，不包含真实 token 或真实 Codex 用量。

## 下载与安装

正式产物发布在 [GitHub Releases](https://github.com/Alexlangl/codex-companion/releases)。每个 Release 包含桌面安装包、CLI/TUI 压缩包、自动更新文件和 `SHA256SUMS`。

如果 Releases 页面暂时没有可下载版本，请按[从源码运行](#从源码运行)操作。

### 桌面 App

| 系统 | 架构 | 安装包 |
| --- | --- | --- |
| macOS | Apple Silicon / Intel / Universal | `.dmg` |
| Windows | x64 | NSIS `.exe` 或 `.msi` |
| Linux | x64 / ARM64 | `.AppImage`、`.deb` 或 `.rpm` |

macOS 可以通过 Homebrew Cask 安装和升级：

```bash
brew install --cask Alexlangl/tap/codex-companion
brew upgrade --cask Alexlangl/tap/codex-companion
```

### CLI 与 TUI

macOS 和 Linux 推荐使用 Homebrew：

```bash
brew install Alexlangl/tap/codex-companion
```

也可以从 Release 下载对应平台的 `codex-companion-<version>-<platform>.tar.gz` 或 Windows `.zip`。压缩包同时包含 `codex-companion` 和 `codex-companion-tui`。

<details>
<summary><strong>首次安装、校验与系统安全提示</strong></summary>

只从本仓库的 GitHub Release 下载文件，并使用同一 Release 中的 `SHA256SUMS` 校验。macOS 可运行 `shasum -a 256 FILE`，Linux 可运行 `sha256sum FILE`，Windows PowerShell 可运行 `Get-FileHash FILE -Algorithm SHA256`。

当前 macOS 安装包使用 ad-hoc 签名，尚未配置 Apple Developer ID 与 notarization；Windows 安装包尚未配置 Authenticode。手动下载安装时可能遇到 Gatekeeper 或 SmartScreen 提示。

macOS 在确认来源与 SHA-256 后，可以运行：

```bash
xattr -dr com.apple.quarantine "/Applications/Codex Companion.app"
open "/Applications/Codex Companion.app"
```

Windows 在确认哈希后，可在 SmartScreen 中选择 `更多信息` → `仍要运行`。不要为此全局关闭 Defender 或 SmartScreen。

Linux AppImage 首次运行前需要执行 `chmod +x Codex-Companion-*.AppImage`。缺少 FUSE 时请改用 `.deb` 或 `.rpm`。

Tauri 自动更新包使用 `.sig` 校验，但它不能替代 macOS Developer ID、Apple notarization 或 Windows Authenticode。首次下载的信任依据仍是 GitHub Release 来源与 `SHA256SUMS`。

</details>

## 支持的账号与导入格式

| 来源 | 导入方式 | 结果 |
| --- | --- | --- |
| 本机 Codex 登录 | `导入本机 Codex 账号` | 读取现有 `~/.codex/auth.json` 与 provider 配置 |
| 官方 Codex / ChatGPT OAuth | Token / JSON | 创建官方账号 provider |
| Codex Companion / CPA / sub2api | 单个 JSON、`accounts[]` 或多文件 | 提取账号身份、token 与可用元数据 |
| Agent Identity | Token / JSON | 保存私有凭据，请求时动态生成 `AgentAssertion` |
| API Key provider | 表单或 API Key JSON | 创建 OpenAI-compatible 或中转 provider |
| New API 连接信息 | `_type: "newapi_channel_conn"` JSON | 将 `key` / `url` 映射为 API Key provider |
| 自定义 provider | 手动填写 Base URL、Key、环境变量与模型 | 创建可直连或代理的 provider |

<details>
<summary><strong>API Key 与 New API JSON 示例</strong></summary>

标准 API Key JSON：

```json
{
  "auth_mode": "apikey",
  "OPENAI_API_KEY": "sk-...",
  "api_base_url": "https://api.example.com/v1",
  "api_provider_id": "example",
  "api_provider_name": "Example API"
}
```

New API 连接 JSON：

```json
{
  "_type": "newapi_channel_conn",
  "key": "sk-...",
  "url": "https://api.example.com"
}
```

New API 根地址会自动规范为 OpenAI-compatible 的 `/v1` 基地址。只有 `_type` 明确为 `newapi_channel_conn` 时，普通 `key` / `url` 字段才会被当作连接信息。

</details>

## 工作方式

```mermaid
flowchart LR
    Manager["Codex Companion<br/>App / CLI / TUI"] --> Registry["账号与分组"]
    Codex["ChatGPT / Codex CLI"] -->|"直连：写入 Codex 配置"| Direct["选定的单个上游"]
    Codex -->|"本地代理"| API["127.0.0.1:17687/v1"]
    Registry --> API
    API --> Router["路由 · 会话亲和 · 故障切换"]
    Router --> Official["官方 Codex OAuth"]
    Router --> Keys["API Key / 中转"]
    Router --> Compatible["OpenAI-compatible"]
```

| 模式 | 行为 | 是否需要重新载入 Codex |
| --- | --- | --- |
| `自动` | 单账号可直连时优先直连，否则使用 Companion 代理 | 取决于最终模式 |
| `直连` | 把 provider 配置和需要的账号材料安装到 Codex | 需要 |
| `本地代理` | Codex 固定连接 Companion，由 Companion 注入凭据并路由 | 首次接入需要；之后切换账号/分组不需要 |

开启 `保留官方 Codex 登录` 后，第三方 API Key 直连会写入 provider 配置，官方 ChatGPT OAuth 继续保留在 `auth.json`。Companion 在修改 Codex 配置前会创建备份，并尽量保留修改后产生的用户内容。

## 本地 API 服务

当前分组默认暴露为 `http://127.0.0.1:17687/v1`：

```bash
curl http://127.0.0.1:17687/v1/responses \
  -H "Authorization: Bearer YOUR_CODEX_COMPANION_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"your-model","input":"hello","stream":false}'
```

- 支持 `POST /v1/responses`、`POST /v1/responses/compact`、WebSocket `GET /v1/responses` 和 `GET /v1/models`。
- API client 密钥只显示一次；SQLite 仅保存 SHA-256 哈希和短前缀。
- 可为每个 client 配置模型白名单，并独立停用、轮换或删除。
- 浏览器跨域请求始终要求有效 client 密钥；本机非浏览器请求可选择兼容模式或强制密钥。
- Provider 可配置独立 `websocket_url`；官方账号和 Agent Identity 会使用各自所需的动态请求头。
- 请求日志保存 provider、模型、状态、尝试次数与延迟，不保存提示词、响应正文或完整密钥。

## CLI 与 TUI

常用命令：

```bash
codex-companion status
codex-companion provider import --json-file ./account.json
codex-companion provider import-local
codex-companion provider refresh-all
codex-companion relay start
codex-companion relay self-test
codex-companion repair --history --plugins --dry-run
codex-companion token-stats
codex-companion sessions --query "project name"
codex-companion-tui
```

| 命令 | 用途 |
| --- | --- |
| `install` / `uninstall` | 写入或恢复 Codex 配置 |
| `doctor` / `status` | 检查接入状态或输出完整状态 |
| `provider add/import/import-local/list/remove/test/refresh` | 管理与检查账号/provider |
| `group create/list/use/set` | 创建、切换和编排分组 |
| `relay start/status/self-test` | 启动与诊断本地 API |
| `relay client ...` | 创建、查询、修改、轮换和删除 API client |
| `relay logs/clear-logs/settings` | 管理请求审计与运行策略 |
| `repair` | Dry-run 或修复历史会话与插件状态 |
| `token-stats` | 扫描本地会话并输出 token 统计 |
| `sessions` | 搜索本地 Codex 会话索引 |

所有命令都支持 `--help`。TUI 内按 `?` 查看快捷键。

## 本地数据与安全边界

默认数据位置：

| 路径 | 内容 |
| --- | --- |
| `~/.codex-companion/config.json` | provider、分组、应用与代理设置 |
| `~/.codex-companion/auth/` | 导入后的账号与 API Key 私有文件 |
| `~/.codex-companion/relay/api-service.sqlite3` | API client 哈希、请求审计、会话亲和与转换历史 |
| `~/.codex-companion/logs/` | 已脱敏的 Companion JSONL 诊断日志 |
| `~/.codex-companion/cache/` | 会话索引与 token 用量缓存 |
| `~/.codex/backups/codex-companion/` | Codex 配置安装与修复备份 |

可以使用 `CODEX_COMPANION_HOME` 修改 Companion 数据目录，使用 `CODEX_COMPANION_CODEX_DIR` 指向另一个 Codex 目录。

安全边界：

- 导入的账号材料留在本机；Unix 下新建和覆盖的私有认证文件使用 `0600` 权限。
- Agent Identity 私钥只用于本地动态签名，不会写入 Codex `auth.json`。
- API client 密钥只显示一次，持久化时只保存哈希。
- 诊断日志会脱敏 token、Authorization、cookie、API Key、私钥和 `AgentAssertion`，并按 2 MB 轮转，最多保留 5 个文件。
- 修复操作先支持 dry-run；正式修复使用事务备份，任一步失败会回滚本次修改。
- Token 成本是根据本地会话和价格快照计算的估算值，不等同于 OpenAI 或中转站账单。

<details>
<summary><strong>自定义模型价格</strong></summary>

在 Companion 数据目录创建 `model-pricing.json`：

```json
{
  "models": [
    {
      "model": "custom-model",
      "aliases": ["vendor/custom-latest"],
      "inputPerMillion": "1.00",
      "cachedInputPerMillion": "0.10",
      "cacheWriteInputPerMillion": "1.25",
      "outputPerMillion": "2.00"
    }
  ],
  "providerMultipliers": {
    "paid-relay": "1.15"
  }
}
```

价格单位为每百万 Token 的美元估算值。未匹配价格的模型会显示为“未定价”，不会静默当作 `$0`。

</details>

## 常见问题

<details>
<summary><strong>切换账号后需要重启 ChatGPT / Codex 吗？</strong></summary>

直连模式需要目标应用重新读取配置和认证材料。本地代理首次接入后，后续切换账号或分组无需重启。

</details>

<details>
<summary><strong>Companion 会上传我的账号或提示词吗？</strong></summary>

Companion 没有账号云同步服务。认证材料保存在本机；请求只会发送到你配置的上游。结构化审计不保存提示词或响应正文。

</details>

<details>
<summary><strong>如何回到原来的 Codex 配置？</strong></summary>

在 `设置` 中恢复 Companion 管理前的配置，或运行 `codex-companion uninstall`。Companion 会使用安装时创建的备份，并避免覆盖安装后由用户修改的内容。

</details>

<details>
<summary><strong>为什么有些中转必须使用本地代理？</strong></summary>

Codex 使用 Responses API。地址明确指向 `/chat/completions` 的 provider 需要 Companion 转换请求、SSE、工具调用和历史消息，因此不能按原地址直连。

</details>

<details>
<summary><strong>用量页为什么和上游账单不完全一致？</strong></summary>

用量页读取本地 Codex 会话中的 token 事件，并使用内置或自定义价格快照估算。上游可能采用不同的计费分类、折扣、倍率或舍入方式。

</details>

## 从源码运行

需要 Node.js 22、pnpm 10.23、稳定版 Rust 工具链，以及当前系统对应的 [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/)。仓库提供 `.nvmrc`，使用 nvm 时先运行 `nvm use`。

```bash
git clone https://github.com/Alexlangl/codex-companion.git
cd codex-companion
corepack enable
pnpm install --frozen-lockfile
pnpm dev
```

`pnpm dev` 默认同时启动隔离的 Companion 和自动发现的 ChatGPT App（兼容旧 Codex App，并可回退到 Codex CLI）。只启动 Companion、使用本机配置、CLI 模式和所有高级参数请参考 [开发环境说明](README.devcodex.md#中文)。

提交前检查：

```bash
pnpm check
cargo test --workspace
```

构建桌面安装包：

```bash
pnpm build:tauri
```

只构建 CLI 与 TUI：

```bash
cargo build --release -p codex-companion-cli -p codex-companion-tui
```

维护者可以在 GitHub Actions 的 `Release` workflow 中输入 `0.1.1` 或 `v0.1.1` 形式的版本号。工作流会构建 macOS、Windows、Linux 桌面包与 CLI/TUI 压缩包，生成 `SHA256SUMS`，并在稳定版发布后更新 Homebrew tap。

## 许可证

Codex Companion 基于 [MIT License](LICENSE) 发布。
