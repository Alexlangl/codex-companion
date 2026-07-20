# Codex Companion

[中文](./README.md) | English

`codex-companion` is a local account and proxy tool for Codex inside the ChatGPT desktop app (with legacy Codex Desktop compatibility) and the Codex CLI.

The current macOS and Windows client is displayed as `ChatGPT` while continuing to use Codex configuration and CLI state. Companion prefers the ChatGPT client for lifecycle control and falls back to the legacy Codex app.

It lets Codex connect to one Companion runtime while you manage official Codex accounts, API-key gateways, and third-party OpenAI-compatible providers locally. It supports provider groups, fallback, health refresh, history repair, plugin state repair, and token usage stats.

It provides:

- Desktop app.
- CLI.
- TUI.

> Screenshots use sanitized demo data. They do not contain real account tokens or real Codex usage.

## Download and Installation

All official artifacts are published on [GitHub Releases](https://github.com/Alexlangl/codex-companion/releases). Each release includes desktop installers, CLI/TUI archives, desktop updater files, and `SHA256SUMS`.

If the Releases page is empty, the project does not yet have a downloadable release and the Homebrew formula may not exist yet. Use the [build-from-source instructions](#build-from-source) in the meantime.

> Current distribution status: the macOS desktop bundles use an ad-hoc signature, not an Apple Developer ID, and are not notarized by Apple. The Windows installers do not yet have an Authenticode certificate. Gatekeeper or SmartScreen may therefore warn on first launch. Verify the downloaded file as described below before deciding whether to allow it.

### Desktop App

Download the file that matches your machine. `<version>` represents the release version.

| System | Architecture | Recommended file | Notes |
| --- | --- | --- | --- |
| macOS | Apple Silicon (M1/M2/M3/M4 and newer) | `Codex-Companion-<version>-macos-arm64-dmg.dmg` | `macos-universal` also works |
| macOS | Intel | `Codex-Companion-<version>-macos-x64-dmg.dmg` | `macos-universal` also works |
| Windows | x64 | `Codex-Companion-<version>-windows-x64-setup.exe` | NSIS installer; an `.msi` is also published |
| Linux | x64 | `Codex-Companion-<version>-linux-x64-appimage.AppImage` | `.deb` and `.rpm` are also published |
| Linux | ARM64 | `Codex-Companion-<version>-linux-arm64-appimage.AppImage` | `.deb` and `.rpm` are also published |

Files ending in `.sig`, `latest.json`, and `*-updater.tar.gz` are used by desktop auto-update. They are not first-install packages and normally should not be opened manually.

### CLI and TUI

Homebrew is recommended on macOS and Linux. The tap updates after every successful stable release:

```bash
brew install Alexlangl/tap/codex-companion
```

You can also download a command-line archive directly:

| System | File |
| --- | --- |
| macOS Apple Silicon | `codex-companion-<version>-macos-arm64.tar.gz` |
| macOS Intel | `codex-companion-<version>-macos-x64.tar.gz` |
| Linux x64 | `codex-companion-<version>-linux-x64.tar.gz` |
| Linux ARM64 | `codex-companion-<version>-linux-arm64.tar.gz` |
| Windows x64 | `codex-companion-<version>-windows-x64.zip` |

On macOS or Linux, extract the archive and place both binaries on `PATH`:

```bash
VERSION="0.1.0" # Replace with the release version you are installing
tar -xzf "codex-companion-${VERSION}-macos-arm64.tar.gz"
sudo install -m 0755 codex-companion codex-companion-tui /usr/local/bin/
```

On Windows, extract the ZIP and run the binaries from PowerShell:

```powershell
.\codex-companion.exe status
.\codex-companion-tui.exe
```

## First Launch and System Security Warnings

### Verify the Download First

Download only from this repository's GitHub Releases and download `SHA256SUMS` from the same release. If the calculated value does not match `SHA256SUMS`, delete the file and stop the installation.

macOS:

```bash
VERSION="0.1.0" # Replace with the downloaded release version
shasum -a 256 "Codex-Companion-${VERSION}-macos-arm64-dmg.dmg"
```

Windows PowerShell:

```powershell
$Version = "0.1.0" # Replace with the downloaded release version
Get-FileHash ".\Codex-Companion-$Version-windows-x64-setup.exe" -Algorithm SHA256
```

Linux:

```bash
VERSION="0.1.0" # Replace with the downloaded release version
sha256sum "Codex-Companion-${VERSION}-linux-x64-appimage.AppImage"
```

### macOS: Developer Cannot Be Verified

The current macOS bundles do not have a Developer ID or Apple notarization, so Gatekeeper may block the first launch. After verifying the source and SHA-256:

1. Try to open `Codex Companion` once so macOS records the block.
2. Open `System Settings` → `Privacy & Security`.
3. Scroll to `Security`, find the blocked Codex Companion entry, and click `Open Anyway`.
4. Authenticate with your login password or Touch ID, then confirm `Open`.

`Open Anyway` is normally available for about one hour after the failed launch. A company-managed Mac may prevent users from overriding the policy; contact your administrator in that case. Do not disable Gatekeeper globally, and do not bypass warnings for a file from an unknown source or with a mismatched SHA-256.

Button labels can vary slightly between macOS versions. See Apple's official [Open a Mac app from an unknown developer](https://support.apple.com/guide/mac-help/open-a-mac-app-from-an-unknown-developer-mh40616/mac) guide for the current system instructions.

If macOS explicitly says the app will damage your computer, reports malicious content, or says the file is damaged, do not treat it as a normal unidentified-developer warning. Download it again and verify the checksum; if it still happens, open an issue with the version and file hash.

### Windows: Windows Protected Your PC

The current Windows installers do not have an Authenticode publisher certificate. SmartScreen may show `Windows protected your PC`, and the publisher may be listed as unknown. After verifying the source and SHA-256:

1. Select `More info` in the SmartScreen dialog.
2. Confirm the application name, then select `Run anyway`.
3. If UAC subsequently shows an unknown publisher, approve it only after the checksum has been verified.

If `Run anyway` is unavailable, Smart App Control, enterprise policy, or an administrator restriction may be enforcing the block. Do not disable Microsoft Defender or SmartScreen globally just to install this project; contact the administrator or build from source in a trusted environment. If Defender reports a specific threat instead of an unknown/low-reputation app, do not blindly add an exclusion. Download again, verify the checksum, and open an issue.

Microsoft documents the current unsigned-app behavior and publisher reputation model in [SmartScreen reputation for Windows app developers](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation).

### Linux: AppImage, DEB, and RPM

Make the AppImage executable before first launch:

```bash
VERSION="0.1.0" # Replace with the downloaded release version
APPIMAGE="Codex-Companion-${VERSION}-linux-x64-appimage.AppImage"
chmod +x "${APPIMAGE}"
"./${APPIMAGE}"
```

If AppImage or FUSE support is unavailable, use the native package instead:

```bash
VERSION="0.1.0" # Replace with the downloaded release version

# Debian / Ubuntu
sudo apt install "./Codex-Companion-${VERSION}-linux-x64-deb.deb"

# Fedora / RHEL
sudo dnf install "./Codex-Companion-${VERSION}-linux-x64-rpm.rpm"
```

The current first-install Linux artifacts do not have a separate GPG release signature. Verify them against `SHA256SUMS` from the release.

## What It Does

- Import official Codex account JSON, including Codex Companion, CPA, and sub2api formats.
- Import API Key account JSON or manually add OpenAI-compatible providers.
- Compose multiple accounts into a Provider Group and fallback by priority; stable sessions keep account affinity when identifiable.
- Expose the active account group as a local OpenAI-compatible API with `/v1/responses` and `/v1/models`.
- Create independent API clients with one-time secrets, model allowlists, disable, rotate, and delete controls.
- Single accounts default to direct mode, and you can switch them to local proxy mode from the account card.
- Relay providers whose URL explicitly targets `/chat/completions` are forced through the local relay so Companion can translate Responses requests, tool calls, and multi-turn history.
- Let Companion handle token refresh and request headers for official Codex accounts.
- Refresh account health, subscription state, and recognizable 5-hour, weekly, 30-day, and model-specific quotas; transient failures retry while keeping the last successful value.
- Repair Codex history and plugin state before launching Codex, reducing provider-switch continuity issues.
- Scan main and subagent Codex session logs and estimate fresh-input, cache-read, cache-write, and output costs by model.

## Screenshots

### Dashboard

View the current group, local proxy address, available accounts, and Codex integration status.

<img src="assets/readme/dashboard.jpg" alt="Codex Companion dashboard" width="720">

### Providers

Manage official accounts, API Key accounts, and gateway providers in one place. Each account can be refreshed and launched with a selected mode.

<img src="assets/readme/providers-compact.jpg" alt="Provider compact list" width="720">

### Add Account

Add API Key accounts, paste Token / JSON, import local Codex accounts, or batch import multiple JSON files.

<img src="assets/readme/provider-add-dialog.jpg" alt="Add provider dialog" width="720">

### Groups

Put multiple accounts into one group and fallback in order. Requests with stable session identifiers keep account affinity and rebind after a provider failure.

<img src="assets/readme/groups.jpg" alt="Provider groups" width="720">

### Local Proxy

Inspect the local API service, verified listener state, clients, request logs, model cooldowns, upstream errors, and fallback events.

<img src="assets/readme/relay.jpg" alt="Relay page" width="720">

### Repair

Preview or repair Codex history and plugin state. Real repair uses an isolated transactional backup, restores files changed by the current run if any later step fails, and keeps the 10 newest repair backups after a successful run.

<img src="assets/readme/repair.jpg" alt="Repair page" width="720">

### Usage

Read token usage and estimated cost from local Codex session logs. Models without a matching price are shown as unpriced instead of being reported as a real `$0`.

<img src="assets/readme/token-usage.jpg" alt="Token usage page" width="720">

## App Usage

After opening the desktop app:

- Use `Providers` to add or import accounts.
- Use `Groups` to arrange fallback order.
- Use `Relay` to inspect local proxy status and request logs.
- Use `Repair` to preview or repair Codex history and plugin state.
- Use `Usage` to view local token stats.
- Use `Settings` to install or restore Codex configuration.

### Local API Service

The active account group is exposed at `http://127.0.0.1:17687/v1`. Clients use the Responses API while Companion handles official OAuth, Responses / Chat Completions translation, session affinity, account fallback, and stream termination checks.

```bash
curl http://127.0.0.1:17687/v1/responses \
  -H "Authorization: Bearer YOUR_CODEX_COMPANION_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"your-model","input":"hello","stream":false}'
```

- Supports `POST /v1/responses` and `GET /v1/models`.
- API client secrets are shown once; SQLite stores only a SHA-256 hash and short prefix.
- Each client can have its own model allowlist and can be disabled, rotated, or deleted independently.
- Cross-origin browser requests always require a valid client key. Local non-browser requests can use compatibility mode or enforce keys.
- Request logs store routing metadata only, never prompts, response bodies, or complete keys.
- If an upstream stream ends without a terminal event, Companion emits `response.failed` and updates provider health and request audit state.

### Custom Model Pricing

Built-in prices are dated estimation snapshots, not upstream billing records. Create `model-pricing.json` in the Companion data directory (default `~/.codex-companion`) to override or add model prices:

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

Prices are USD estimates per one million tokens. `cacheWriteInputPerMillion` is optional and falls back to the normal input price; use `providerMultipliers` to represent a gateway markup or discount.

## CLI Usage

See [Download and Installation](#download-and-installation) for installation options. The following are common operations; every command supports `--help` for its current arguments.

Show status:

```bash
codex-companion status
```

Import account JSON:

```bash
codex-companion provider import --json-file ./account.json
```

Import an existing local Codex account:

```bash
codex-companion provider import-local
```

Refresh account status:

```bash
codex-companion provider refresh-all
```

Preview repair:

```bash
codex-companion repair --history --plugins --dry-run
```

Start local proxy:

```bash
codex-companion relay start
```

Manage the local API:

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

### Command Coverage

| Command | Purpose |
| --- | --- |
| `install` / `uninstall` | Write or restore the Codex configuration |
| `doctor` / `status` | Check integration health or print full status |
| `daemon start` | Start the Companion daemon in the foreground |
| `provider add` | Add a provider manually |
| `provider import` / `import-local` | Import accounts from JSON or the local Codex installation |
| `provider list` / `remove` / `test` / `refresh` / `refresh-all` | Inspect, remove, test, and refresh providers |
| `group create` / `list` / `use` / `set` | Create, switch, and reorder groups |
| `relay start` / `status` / `self-test` | Start and diagnose the local API service |
| `relay client create/list/update/rotate/delete` | Manage local API clients and secrets |
| `relay logs` / `clear-logs` / `settings` | Inspect audit records and change relay policies |
| `repair` | Preview or repair history and plugin state |
| `token-stats` | Scan local sessions and report token usage |

## TUI Usage

```bash
codex-companion-tui
```

Press `?` inside the TUI to view shortcuts.

## Build from Source

You need Node.js 22, pnpm 10.23, a stable Rust toolchain, and the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```bash
git clone https://github.com/Alexlangl/codex-companion.git
cd codex-companion
pnpm install --frozen-lockfile
pnpm check
cargo test --workspace
pnpm dev:app
```

Build desktop packages for the current platform:

```bash
pnpm build:tauri
```

Build only the CLI and TUI:

```bash
cargo build --release -p codex-companion-cli -p codex-companion-tui
```

## Manual Remote Packaging

The repository provides a `Release` GitHub Actions workflow. Open `Actions` → `Release` → `Run workflow` on GitHub and enter a version such as `0.1.1` or `v0.1.1`.

The workflow builds desktop bundles for macOS Universal / Intel / Apple Silicon, Windows x64, and Linux x64 / ARM64, plus command-line archives containing both the CLI and TUI. It creates the GitHub Release only after every platform succeeds and includes `SHA256SUMS`.

After a successful stable release, the workflow updates `Alexlangl/homebrew-tap`. The desktop app checks stable releases quietly at startup, lets the user choose `Update now` or `Later` when one is available, and also supports manual check, download, and restart installation from `Settings`; Tauri signatures protect update artifacts.

## Update and Signing Boundaries

These mechanisms protect different parts of the distribution process. The presence of a `.sig` file does not mean that the operating system trusts the publisher.

| Mechanism | Current status | Scope |
| --- | --- | --- |
| Release `SHA256SUMS` | Available | Lets users compare a download with the file published in that release |
| Tauri updater `.sig` | Enabled and mandatory | Verifies the source and integrity of updates downloaded by an installed desktop app |
| macOS Developer ID + notarization | Not configured | Establishes an Apple-recognized publisher identity and reduces Gatekeeper blocks once configured |
| Windows Authenticode | Not configured | Displays publisher identity once configured; SmartScreen reputation may still take time to build |
| Linux GPG/AppImage release signing | Not configured | First installs currently rely on the GitHub Release source and SHA-256 verification |

The Tauri updater private key exists only in the release environment; the repository contains only the public key. An updater signature does not replace macOS Developer ID signing, Apple notarization, or Windows Authenticode, and cannot remove the operating-system reputation warning shown for a first browser download.

Removing those first-install warnings requires separate Apple Developer ID/notarization credentials and a Windows code-signing certificate in CI. Certificates and private keys must never be committed to the repository.

## Supported Accounts

- Official Codex account JSON.
- sub2api `accounts[]` OpenAI OAuth accounts.
- Codex Companion / CPA style Codex OAuth accounts.
- API Key account JSON.
- Manually added OpenAI-compatible providers.
- Existing local Codex accounts.

API Key JSON example:

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

## Safety Notes

- You can run dry-run before writing repair changes.
- Real repair uses transactional backups under `~/.codex/backups/codex-companion/repair/`; failures roll back automatically and a successful run keeps the 10 newest repair backups.
- Imported account material stays on your machine.
- Local API client secrets are shown once and stored only as hashes; request audit records do not store request or response bodies.
- Token usage stats only read local Codex session logs.
- Cost is an estimate based on the model price snapshot and local overrides, not an OpenAI or gateway invoice.
