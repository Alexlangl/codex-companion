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

## 下载与安装

所有正式产物都发布在 [GitHub Releases](https://github.com/Alexlangl/codex-companion/releases)。每个 Release 同时提供桌面安装包、CLI/TUI 压缩包、自动更新文件和 `SHA256SUMS`。

如果 Releases 页面为空，表示项目尚未发布可供下载的版本，此时 Homebrew formula 也可能尚未生成；请先按[从源码运行](#从源码运行)操作。

> 当前发行状态：macOS 桌面包使用 ad-hoc 签名，尚未使用 Apple Developer ID，也没有经过 Apple notarization；Windows 安装包尚未使用 Authenticode 证书。因此首次启动可能出现 Gatekeeper 或 SmartScreen 提示。请先按下文校验下载文件，再决定是否放行。

### 桌面 App

下载与机器架构匹配的文件；`<version>` 代表 Release 版本号。

| 系统 | 架构 | 推荐文件 | 备注 |
| --- | --- | --- | --- |
| macOS | Apple Silicon（M1/M2/M3/M4 等） | `Codex-Companion-<version>-macos-arm64-dmg.dmg` | 也可使用 `macos-universal` |
| macOS | Intel | `Codex-Companion-<version>-macos-x64-dmg.dmg` | 也可使用 `macos-universal` |
| Windows | x64 | `Codex-Companion-<version>-windows-x64-setup.exe` | NSIS 安装器；也会提供 `.msi` |
| Linux | x64 | `Codex-Companion-<version>-linux-x64-appimage.AppImage` | 同时提供 `.deb` 和 `.rpm` |
| Linux | ARM64 | `Codex-Companion-<version>-linux-arm64-appimage.AppImage` | 同时提供 `.deb` 和 `.rpm` |

Release 中的 `.sig`、`latest.json` 和 `*-updater.tar.gz` 是桌面自动更新使用的文件，普通用户首次安装时不需要手动打开。

### CLI 和 TUI

macOS 或 Linux 推荐使用 Homebrew；稳定版 Release 成功后 tap 会自动更新：

```bash
brew install Alexlangl/tap/codex-companion
```

也可以从 Release 直接下载：

| 系统 | 文件 |
| --- | --- |
| macOS Apple Silicon | `codex-companion-<version>-macos-arm64.tar.gz` |
| macOS Intel | `codex-companion-<version>-macos-x64.tar.gz` |
| Linux x64 | `codex-companion-<version>-linux-x64.tar.gz` |
| Linux ARM64 | `codex-companion-<version>-linux-arm64.tar.gz` |
| Windows x64 | `codex-companion-<version>-windows-x64.zip` |

macOS/Linux 解压后可把两个二进制放进 `PATH`：

```bash
VERSION="0.1.0" # 替换为要安装的 Release 版本
tar -xzf "codex-companion-${VERSION}-macos-arm64.tar.gz"
sudo install -m 0755 codex-companion codex-companion-tui /usr/local/bin/
```

Windows 解压 ZIP 后，可以直接在 PowerShell 中运行：

```powershell
.\codex-companion.exe status
.\codex-companion-tui.exe
```

## 首次启动与系统安全提示

### 先校验下载文件

只从本仓库的 GitHub Release 下载文件，并同时下载同一版本的 `SHA256SUMS`。如果计算结果与 `SHA256SUMS` 不一致，请删除文件并停止安装。

macOS：

```bash
VERSION="0.1.0" # 替换为下载的 Release 版本
shasum -a 256 "Codex-Companion-${VERSION}-macos-arm64-dmg.dmg"
```

Windows PowerShell：

```powershell
$Version = "0.1.0" # 替换为下载的 Release 版本
Get-FileHash ".\Codex-Companion-$Version-windows-x64-setup.exe" -Algorithm SHA256
```

Linux：

```bash
VERSION="0.1.0" # 替换为下载的 Release 版本
sha256sum "Codex-Companion-${VERSION}-linux-x64-appimage.AppImage"
```

### macOS：无法验证开发者或无法检查恶意软件

当前 macOS 包没有 Developer ID 和 Apple notarization，因此 Gatekeeper 可能阻止首次打开。确认下载来源和 SHA-256 后：

1. 先尝试打开一次 `Codex Companion`，让系统记录拦截。
2. 打开 `系统设置` → `隐私与安全性`。
3. 滚动到 `安全性`，找到刚被拦截的 Codex Companion，点击 `仍要打开`。
4. 使用登录密码或 Touch ID 确认，然后再次选择 `打开`。

`仍要打开` 通常只会在首次尝试后的约一小时内出现。受公司 MDM 管理的 Mac 可能不允许用户放行，此时请联系管理员。不要全局关闭 Gatekeeper，也不要对来源不明或 SHA-256 不匹配的文件执行绕过命令。

不同 macOS 版本的按钮文字可能略有差异，可同时参考 [Apple 官方的“打开来自未知开发者的 Mac App”说明](https://support.apple.com/zh-cn/guide/mac-help/mh40616/mac)。

如果 macOS 明确提示“将损坏你的电脑”、文件已损坏或检测到恶意内容，不要按普通“未知开发者”处理：重新下载并校验；仍然出现时请保留版本号和文件哈希提交 Issue。

### Windows：Windows 已保护你的电脑

当前 Windows 安装包没有 Authenticode 发布者证书，SmartScreen 可能显示 `Windows protected your PC` / `Windows 已保护你的电脑`，发布者会显示为未知。确认下载来源和 SHA-256 后：

1. 在 SmartScreen 窗口点击 `更多信息`（`More info`）。
2. 核对应用名称后点击 `仍要运行`（`Run anyway`）。
3. 如果随后出现 UAC 的“未知发布者”提示，只在哈希已经核对一致时确认安装。

如果没有 `仍要运行`，设备可能启用了 Smart App Control、企业策略或管理员限制。不要为安装本软件而全局关闭 Microsoft Defender 或 SmartScreen；请联系管理员，或在可信环境中从源码构建。若 Defender 报告的是明确威胁而不是“未知/低信誉”，不要直接加入白名单，请先重新下载、核对哈希并提交 Issue。

Microsoft 对未签名程序的当前行为和发布者信誉机制有单独说明，详见 [SmartScreen reputation for Windows app developers](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)。

### Linux：AppImage、DEB 和 RPM

AppImage 首次运行前需要可执行权限：

```bash
VERSION="0.1.0" # 替换为下载的 Release 版本
APPIMAGE="Codex-Companion-${VERSION}-linux-x64-appimage.AppImage"
chmod +x "${APPIMAGE}"
"./${APPIMAGE}"
```

如果系统缺少 AppImage/FUSE 支持，可以改用发行版安装包：

```bash
VERSION="0.1.0" # 替换为下载的 Release 版本

# Debian / Ubuntu
sudo apt install "./Codex-Companion-${VERSION}-linux-x64-deb.deb"

# Fedora / RHEL
sudo dnf install "./Codex-Companion-${VERSION}-linux-x64-rpm.rpm"
```

当前 Linux 首次安装包没有单独的 GPG 发布签名，请使用 Release 中的 `SHA256SUMS` 校验。

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

安装方式见[下载与安装](#下载与安装)。下面是常用操作；所有命令都支持 `--help` 查看当前参数。

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

### 命令范围

| 命令 | 用途 |
| --- | --- |
| `install` / `uninstall` | 写入或恢复 Codex 配置 |
| `doctor` / `status` | 检查接入状态或输出完整状态 |
| `daemon start` | 前台启动 Companion daemon |
| `provider add` | 手动添加 provider |
| `provider import` / `import-local` | 从 JSON 或本机 Codex 导入账号 |
| `provider list` / `remove` / `test` / `refresh` / `refresh-all` | 查询、删除、测试和刷新 provider |
| `group create` / `list` / `use` / `set` | 创建分组、切换分组和调整顺序 |
| `relay start` / `status` / `self-test` | 启动和诊断本地 API 服务 |
| `relay client create/list/update/rotate/delete` | 管理本地 API client 和密钥 |
| `relay logs` / `clear-logs` / `settings` | 查看审计记录和修改转发策略 |
| `repair` | dry-run 或修复历史会话与插件状态 |
| `token-stats` | 扫描本地会话并输出 token 统计 |

## TUI 用法

```bash
codex-companion-tui
```

在 TUI 里按 `?` 查看快捷键。

## 从源码运行

需要 Node.js 22、pnpm 10.23、稳定版 Rust 工具链，以及对应系统的 [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/)。

```bash
git clone https://github.com/Alexlangl/codex-companion.git
cd codex-companion
pnpm install --frozen-lockfile
pnpm check
cargo test --workspace
pnpm dev:app
```

构建当前平台桌面安装包：

```bash
pnpm build:tauri
```

只构建 CLI 和 TUI：

```bash
cargo build --release -p codex-companion-cli -p codex-companion-tui
```

## 远端手动打包

仓库提供 `Release` GitHub Actions 工作流。在 GitHub 的 `Actions` → `Release` 页面选择 `Run workflow`，输入 `0.1.1` 或 `v0.1.1` 形式的版本号即可开始打包。

工作流会构建 macOS Universal / Intel / Apple Silicon、Windows x64、Linux x64 / ARM64 桌面安装包，以及包含 CLI 和 TUI 的命令行压缩包。全部平台成功后才会创建对应 GitHub Release，并附带 `SHA256SUMS`。

稳定版 Release 成功后会自动更新 `Alexlangl/homebrew-tap`。桌面端启动时会静默检查稳定版本，发现更新后由用户选择“立即更新”或“稍后”，也可以在 `设置` 中手动检查、下载并重启安装；更新包使用 Tauri 签名校验。

## 更新与签名边界

这些机制保护的对象不同，不能把“有 `.sig`”理解成“操作系统已信任发布者”。

| 机制 | 当前状态 | 保护范围 |
| --- | --- | --- |
| Release `SHA256SUMS` | 已提供 | 让用户核对下载内容是否与该 Release 发布的文件一致 |
| Tauri updater `.sig` | 已启用且强制校验 | 已安装的桌面 App 下载更新时验证更新包来源和完整性 |
| macOS Developer ID + notarization | 尚未配置 | 配置后可建立 Apple 认可的发布者身份并减少 Gatekeeper 拦截 |
| Windows Authenticode | 尚未配置 | 配置后显示发布者身份；SmartScreen 信誉仍可能需要积累 |
| Linux GPG/AppImage 发布签名 | 尚未配置 | 首次下载目前依赖 GitHub Release 来源和 SHA-256 校验 |

Tauri updater 的私钥只存放在发布环境，仓库内只有公钥。自动更新签名不会替代 macOS Developer ID、Apple notarization 或 Windows Authenticode；它也不能消除首次从浏览器下载安装时的系统信誉提示。

要消除这些首次安装提示，需要为 CI 另外配置 Apple Developer ID/公证凭据和 Windows 代码签名证书。证书和私钥不得提交到仓库。

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
