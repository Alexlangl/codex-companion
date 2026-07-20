# Codex Companion

中文 | [English](./README.en.md)

`codex-companion` 是给 ChatGPT 桌面端内的 Codex（同时兼容旧版 Codex Desktop）/ Codex CLI 使用的本地账号与代理工具。

新版 macOS / Windows 官方客户端虽然显示为 `ChatGPT`，仍使用 Codex 的配置目录与 CLI。Companion 会优先发现并控制 ChatGPT 客户端，同时保留对旧版 Codex 应用的兼容。

它让 Codex 只需要接入一个 Companion，本地就可以管理官方 Codex 账号、API Key 中转、第三方 OpenAI-compatible provider，并支持账号分组、失败切换、健康刷新、历史会话修复和 token 用量统计。

它提供：

- 桌面 App。
- CLI 命令行。
- TUI 终端界面。

> 截图使用的是脱敏 demo 数据，不包含真实账号 token 或真实 Codex 用量。

## 它做什么

- 导入官方 Codex 账号 JSON，例如 Codex Companion、CPA 或 sub2api 格式。
- 导入 API Key 账号 JSON，或手动添加 OpenAI-compatible provider。
- 把多个账号编排成 Provider Group，按优先级失败切换；能识别稳定会话时会保持账号粘性。
- 把当前账号分组作为本地 OpenAI-compatible API，提供 `/v1/responses` 和 `/v1/models`。
- 为不同调用方创建独立 API client，支持一次性密钥显示、模型白名单、停用、轮换和删除。
- 单个账号默认直连，也可以在账号卡片里切换为本地代理。
- 地址明确指向 `/chat/completions` 的中转站会强制使用本地代理，由 Companion 转换 Responses、工具调用和多轮历史，避免误选直连。
- 官方 Codex 账号由 Companion 负责 token 刷新和请求头处理。
- 自动刷新账号健康度、订阅状态和可识别的 5h、周、30 天及模型专属额度；瞬时失败会重试并保留上次成功值。
- 启动 Codex 前修复历史会话和插件状态，减少切换 provider 后上下文丢失的问题。
- 从本地 Codex 会话记录里统计主会话与子代理 token，用模型价格分别估算新输入、缓存读取、缓存写入和输出成本。

## 截图

### 总览

查看当前分组、本地代理地址、可用账号和 Codex 接入状态。

<img src="assets/readme/dashboard.jpg" alt="Codex Companion dashboard" width="720">

### 账号

集中管理官方账号、API Key 账号和中转 provider。每个账号可以刷新状态，也可以选择启动方式。

<img src="assets/readme/providers-compact.jpg" alt="Provider compact list" width="720">

### 添加账号

支持 API Key、Token / JSON、本机 Codex 账号导入，也支持多个 JSON 批量导入。

<img src="assets/readme/provider-add-dialog.jpg" alt="Add provider dialog" width="720">

### 分组

把多个账号放进一个分组，按顺序执行失败切换；有稳定会话标识时会保持账号粘性，绑定账号失败后切换并重新绑定。

<img src="assets/readme/groups.jpg" alt="Provider groups" width="720">

### 本地代理

查看 Companion 本地 API 服务、真实监听状态、client、请求记录、模型冷却、上游错误和切换事件。

<img src="assets/readme/relay.jpg" alt="Relay page" width="720">

### 修复

预览或修复 Codex 历史会话和插件状态。正式修复使用独立事务备份；任一步失败会恢复本次已修改文件，成功后保留最近 10 份修复备份。

<img src="assets/readme/repair.jpg" alt="Repair page" width="720">

### 用量

从 Codex 本地会话记录中统计 token 使用量和估算成本。未匹配价格的模型会明确标记为“未定价”，不会当作真实 `$0`。

<img src="assets/readme/token-usage.jpg" alt="Token usage page" width="720">

## App 用法

打开桌面 App 后：

- 使用 `账号` 添加或导入 provider。
- 使用 `分组` 编排 fallback 顺序。
- 使用 `转发` 查看本地代理状态和请求记录。
- 使用 `修复` 预览或修复 Codex 历史会话和插件状态。
- 使用 `用量` 查看本地 token 统计。
- 使用 `设置` 写入或恢复 Codex 配置。

### 本地 API 服务

当前账号分组会暴露为 `http://127.0.0.1:17687/v1`。调用方使用 Responses API；Companion 负责官方 OAuth、Responses / Chat Completions 协议转换、会话亲和、账号失败切换和流式终止检查。

```bash
curl http://127.0.0.1:17687/v1/responses \
  -H "Authorization: Bearer YOUR_CODEX_COMPANION_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"your-model","input":"hello","stream":false}'
```

- 支持 `POST /v1/responses` 和 `GET /v1/models`。
- API client 密钥只显示一次；SQLite 只保存 SHA-256 哈希和短前缀。
- 可为每个 client 限制允许模型，并单独停用、轮换或删除。
- 浏览器跨域请求始终需要有效 client 密钥；非浏览器本机请求可选择兼容模式或强制密钥。
- 请求日志只保存路由元数据，不保存提示词、响应正文或完整密钥。
- 上游流未产生终止事件就断开时，Companion 会返回 `response.failed`，并同步更新账号健康状态和请求审计。

### 自定义模型价格

内置价格是带日期的估算快照，不代表上游账单。可以在 Companion 数据目录（默认 `~/.codex-companion`）创建 `model-pricing.json` 覆盖或补充模型价格：

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

价格单位为每百万 Token 的美元估算值。`cacheWriteInputPerMillion` 可省略，省略时按普通输入价格计算；`providerMultipliers` 可用于表达中转站加价或折扣。

## CLI 用法

查看状态：

```bash
codex-companion status
```

导入账号 JSON：

```bash
codex-companion provider import --json-file ./account.json
```

导入本机已有 Codex 账号：

```bash
codex-companion provider import-local
```

刷新账号状态：

```bash
codex-companion provider refresh-all
```

预览修复：

```bash
codex-companion repair --history --plugins --dry-run
```

启动本地代理：

```bash
codex-companion relay start
```

管理本地 API：

```bash
codex-companion relay status
codex-companion relay self-test
codex-companion relay client create --name local-script --models model-a,model-b
codex-companion relay client list
codex-companion relay client update --id CLIENT_ID --enabled false
codex-companion relay client rotate CLIENT_ID
codex-companion relay client delete CLIENT_ID
codex-companion relay logs --limit 50
codex-companion relay clear-logs
codex-companion relay settings --require-api-key true --retry-budget 2
```

## TUI 用法

```bash
codex-companion-tui
```

在 TUI 里按 `?` 查看快捷键。

## 支持的账号

- 官方 Codex 账号 JSON。
- sub2api `accounts[]` OpenAI OAuth 账号。
- Codex Companion / CPA 风格 Codex OAuth 账号。
- API Key 账号 JSON。
- 手动添加的 OpenAI-compatible provider。
- 本机已有 Codex 账号。

API Key JSON 示例：

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

## 安全说明

- 修复前可以先 dry-run 预览影响。
- 正式修复使用 `~/.codex/backups/codex-companion/repair/` 下的事务备份；失败自动回滚，成功后保留最近 10 份。
- 导入的账号材料只保存在本机。
- 本地 API client 密钥只显示一次，持久化时只保存哈希；请求审计不保存请求或响应正文。
- Token 用量统计只读取本地 Codex 会话记录。
- 成本是基于模型价格快照和本地覆盖配置的估算，不是 OpenAI 或中转站账单。
