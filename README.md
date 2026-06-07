# Codex Companion

中文 | [English](./README.en.md)

`codex-companion` 是给 Codex Desktop / CLI 使用的本地账号与转发工具。

它让 Codex 只需要接入一个 Companion，本地就可以管理官方 Codex 账号、API Key 中转、第三方 OpenAI-compatible provider，并支持账号分组、失败切换、健康刷新、历史会话修复和 token 用量统计。

它提供：

- 桌面 App。
- CLI 命令行。
- TUI 终端界面。

> 截图使用的是脱敏 demo 数据，不包含真实账号 token 或真实 Codex 用量。

## 它做什么

- 导入官方 Codex 账号 JSON，例如 CPA、sub2api、cockpit 导出的账号。
- 导入 API Key 账号 JSON，或手动添加 OpenAI-compatible provider。
- 把多个账号编排成 Provider Group，按优先级失败切换。
- 单个 API Key 账号可以选择直连中转站，也可以选择走本地代理。
- 官方 Codex 账号由 Companion 负责 token 刷新和请求头处理。
- 自动刷新账号健康度、订阅状态和可识别的额度信息。
- 启动 Codex 前修复历史会话和插件状态，减少切换 provider 后上下文丢失的问题。
- 从本地 Codex 会话记录里统计 token 用量。

## 截图

### 总览

查看当前分组、本地转发地址、可用账号和 Codex 接入状态。

<img src="assets/readme/dashboard.jpg" alt="Codex Companion dashboard" width="720">

### 账号

集中管理官方账号、API Key 账号和中转 provider。每个账号可以刷新状态，也可以选择启动方式。

<img src="assets/readme/providers-compact.jpg" alt="Provider compact list" width="720">

### 添加账号

支持 API Key、Token / JSON、本机 Codex 账号导入，也支持多个 JSON 批量导入。

<img src="assets/readme/provider-add-dialog.jpg" alt="Add provider dialog" width="720">

### 分组

把多个账号放进一个分组，按顺序执行失败切换。

<img src="assets/readme/groups.jpg" alt="Provider groups" width="720">

### 本地转发

查看 Companion 本地转发服务、请求记录、上游错误和切换事件。

<img src="assets/readme/relay.jpg" alt="Relay page" width="720">

### 修复

预览或修复 Codex 历史会话和插件状态，执行修复前会创建备份。

<img src="assets/readme/repair.jpg" alt="Repair page" width="720">

### 用量

从 Codex 本地会话记录中统计 token 使用量。

<img src="assets/readme/token-usage.jpg" alt="Token usage page" width="720">

## App 用法

打开桌面 App 后：

- 使用 `账号` 添加或导入 provider。
- 使用 `分组` 编排 fallback 顺序。
- 使用 `转发` 查看本地转发状态和请求记录。
- 使用 `修复` 预览或修复 Codex 历史会话和插件状态。
- 使用 `用量` 查看本地 token 统计。
- 使用 `设置` 写入或恢复 Codex 配置。

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

启动本地转发：

```bash
codex-companion relay start
```

## TUI 用法

```bash
codex-companion-tui
```

在 TUI 里按 `?` 查看快捷键。

## 支持的账号

- 官方 Codex 账号 JSON。
- sub2api `accounts[]` OpenAI OAuth 账号。
- cockpit / CPA 风格 Codex OAuth 账号。
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
- 正式修复前会备份本地 Codex 数据。
- 导入的账号材料只保存在本机。
- Token 用量统计只读取本地 Codex 会话记录。

## 开发者文档

如果你想从源码运行或参与开发，可以查看 [ARCHITECTURE.md](./ARCHITECTURE.md)。
