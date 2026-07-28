<div align="center">

<img src="apps/desktop/src-tauri/icons/icon.png" width="112" alt="Codex Companion logo" />

# Codex Companion

**A local account pool, compatibility gateway, and session continuity runtime built for Codex.**

Connect Codex in the ChatGPT desktop app, legacy Codex Desktop, or Codex CLI to one local endpoint. Companion manages official accounts, API-key gateways, OpenAI-compatible providers, group routing, failover, and usage reporting behind it.

<p>
  <a href="https://github.com/Alexlangl/codex-companion/releases"><img alt="GitHub Release" src="https://img.shields.io/github/v/release/Alexlangl/codex-companion?style=flat" /></a>
  <a href="https://github.com/Alexlangl/codex-companion/releases"><img alt="GitHub Downloads" src="https://img.shields.io/github/downloads/Alexlangl/codex-companion/total?style=flat" /></a>
  <img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-5b6573?style=flat" />
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/github/license/Alexlangl/codex-companion?style=flat" /></a>
  <a href="https://tauri.app/"><img alt="Built with Tauri 2" src="https://img.shields.io/badge/Tauri-2-24C8DB?style=flat" /></a>
</p>

[简体中文](README.md) · English

</div>

> Codex Companion is local-first. Account material, request audits, and session usage data stay on your device; request bodies are never written to audit logs.

<img src="assets/readme/dashboard.jpg" alt="Codex Companion dashboard" width="100%" />

## Why Codex Companion

Many tools solve “switch the active API configuration.” Codex Companion focuses on what happens after that switch: one Codex client can use an account pool, select a route before each request, fail over when an upstream breaks, preserve session affinity, and expose health, quota, audit, and token-cost data locally.

- **Focused on Codex**: works with the real configuration, authentication material, Responses API, and session files used by ChatGPT / Codex Desktop and Codex CLI.
- **An account pool, not one active profile**: official OAuth, Agent Identity, API keys, and third-party gateways can participate in the same group.
- **Request-level reliability**: priority, round-robin, random, weighted, least-loaded, and manual routing with cooldowns and failover.
- **Continuity across switches**: session affinity, history namespace repair, plugin-state repair, and direct `codex resume` entry points.
- **Local observability**: account health, recognizable quotas, structured request audits, and token usage across main sessions and subagents.
- **Three interfaces**: a desktop app for daily work, a CLI for automation, and a TUI for terminal-only environments.

### From configuration switching to runtime management

| Concern | Switching Codex configuration only | Using the Companion local relay |
| --- | --- | --- |
| Upstream | One active provider | A group can contain multiple accounts and providers |
| Switch point | Rewrite configuration, then restart Codex | Route before a request and try a fallback on failure |
| Sessions | The caller handles continuity | Stable sessions keep affinity and rebind after failure |
| Protocols | The upstream must directly support Codex | Companion can bridge Responses and Chat Completions |
| Runtime state | Usually only the final error is visible | Health, quota, cooldowns, attempts, and audits are centralized |

If you use one fixed `base_url` and API key, configuring Codex directly may be all you need. Companion becomes useful when you need multiple accounts, hot switching, failure recovery, or a reusable local API.

## Quick Start

1. Install the desktop app from [GitHub Releases](https://github.com/Alexlangl/codex-companion/releases), or run `brew install --cask Alexlangl/tap/codex-companion` on macOS.
2. Open `账号` (Accounts) → `添加账号` (Add Account), then import the local Codex login, paste Token / JSON, or enter an API key and Base URL.
3. Choose `自动` (Auto), `直连` (Direct), or `本地代理` (Local Relay) on an account card and launch it. For multiple accounts, create a group, choose its routing policy, and launch the group.

Recommended modes:

| Scenario | Launch mode |
| --- | --- |
| One official account or standard API key, shortest possible path | `Auto` or `Direct` |
| Multi-account failover, hot switching, or session affinity | `Local Relay` / launch a group |
| Upstream only exposes `/chat/completions` | `Local Relay`, with protocol translation |
| Agent Identity | `Local Relay`, with dynamic signing |

Direct mode updates Codex configuration and may write `auth.json`; ChatGPT / Codex must reload afterward. Local Relay keeps Codex connected to Companion, so later account and group switches do not require a restart.

## Core Capabilities

### Accounts and Providers

- Import official Codex, Codex Companion / CPA, sub2api, Agent Identity, API Key, and New API connection JSON.
- Batch-import multiple files, report each result independently, and add successful entries to the active group.
- Detect supported subscription, quota-window, and reset metadata; transient refresh failures keep the last successful snapshot.
- Choose Direct or Local Relay per account, and export accounts as portable JSON.

### Group Routing and Reliability

- Priority fallback, round-robin, random, weighted, least-loaded, and manual policies.
- Stable session IDs keep account affinity; failed bindings switch providers and rebind automatically.
- Model-level 404 / 429 failures cool down only the affected provider-model pair.
- SSE requests retry only before meaningful output reaches the client, preventing duplicate text or tool calls.

### Local Responses API

- HTTP and WebSocket `/v1/responses`, `/v1/responses/compact`, and `/v1/models` endpoints.
- Independent API clients with model allowlists, disable, rotate, and delete controls.
- Responses-to-Chat-Completions translation with streaming events, tool calls, and multi-turn history.
- Request audits retain routing metadata without prompts, response bodies, or complete keys.

### Sessions, Repair, and Usage

- Search local Codex sessions and copy or run `codex resume SESSION_ID`.
- Preview history and plugin repairs with dry-run; real repairs use transactional backups and roll back on failure.
- Scan `token_count` events from main sessions and subagents, filtered by date, provider, and model.
- Separate fresh input, cache reads, cache writes, and output, then estimate cost with an overridable price table.

## Interface Preview

<table>
  <tr>
    <td width="50%"><strong>Account pool</strong><br />Import, refresh, export, and select launch modes.</td>
    <td width="50%"><strong>Groups</strong><br />Arrange priority, weights, fallback, and failback.</td>
  </tr>
  <tr>
    <td><img src="assets/readme/providers-compact.jpg" alt="Codex Companion account list" /></td>
    <td><img src="assets/readme/groups.jpg" alt="Codex Companion group routing" /></td>
  </tr>
  <tr>
    <td><strong>Local API</strong><br />Inspect the listener, API clients, cooldowns, and request audits.</td>
    <td><strong>Token usage</strong><br />Analyze local sessions by time, provider, and model.</td>
  </tr>
  <tr>
    <td><img src="assets/readme/relay.jpg" alt="Codex Companion local API" /></td>
    <td><img src="assets/readme/token-usage.jpg" alt="Codex Companion token usage" /></td>
  </tr>
  <tr>
    <td><strong>Batch import</strong><br />Import Token / JSON, local accounts, and group members.</td>
    <td><strong>Session repair</strong><br />Preview with dry-run, then apply a rollback-safe repair.</td>
  </tr>
  <tr>
    <td><img src="assets/readme/provider-add-dialog.jpg" alt="Codex Companion add account dialog" /></td>
    <td><img src="assets/readme/repair.jpg" alt="Codex Companion session repair" /></td>
  </tr>
</table>

> Screenshots use sanitized demo data and contain no real account tokens or Codex usage.

## Download and Installation

Release artifacts are published on [GitHub Releases](https://github.com/Alexlangl/codex-companion/releases). Each release includes desktop installers, CLI/TUI archives, updater artifacts, and `SHA256SUMS`.

If no downloadable release is available yet, follow [Build from Source](#build-from-source).

### Desktop App

| System | Architecture | Package |
| --- | --- | --- |
| macOS | Apple Silicon / Intel / Universal | `.dmg` |
| Windows | x64 | NSIS `.exe` or `.msi` |
| Linux | x64 / ARM64 | `.AppImage`, `.deb`, or `.rpm` |

Install and upgrade the macOS app with Homebrew Cask:

```bash
brew install --cask Alexlangl/tap/codex-companion
brew upgrade --cask Alexlangl/tap/codex-companion
```

### CLI and TUI

Homebrew is the recommended installation path on macOS and Linux:

```bash
brew install Alexlangl/tap/codex-companion
```

You can also download `codex-companion-<version>-<platform>.tar.gz`, or the Windows `.zip`, from a release. Each archive contains both `codex-companion` and `codex-companion-tui`.

<details>
<summary><strong>First install, checksum verification, and system warnings</strong></summary>

Download only from this repository's GitHub Releases and verify files against `SHA256SUMS` from the same release. Use `shasum -a 256 FILE` on macOS, `sha256sum FILE` on Linux, or `Get-FileHash FILE -Algorithm SHA256` in Windows PowerShell.

Current macOS packages use ad-hoc signing and do not yet have an Apple Developer ID or notarization. Windows installers do not yet have Authenticode signing. Manual downloads may therefore trigger Gatekeeper or SmartScreen.

After confirming the source and SHA-256 on macOS, run:

```bash
xattr -dr com.apple.quarantine "/Applications/Codex Companion.app"
open "/Applications/Codex Companion.app"
```

On Windows, after verifying the hash, choose `More info` → `Run anyway` in SmartScreen. Do not disable Defender or SmartScreen globally.

Run `chmod +x Codex-Companion-*.AppImage` before opening a Linux AppImage. Use the `.deb` or `.rpm` package if FUSE is unavailable.

Tauri updater artifacts are verified with `.sig` files, but updater signatures do not replace Apple Developer ID, notarization, or Windows Authenticode. Trust for the first download still comes from the GitHub Release source and `SHA256SUMS`.

</details>

## Supported Accounts and Import Formats

| Source | Import path | Result |
| --- | --- | --- |
| Local Codex login | `导入本机 Codex 账号` (Import Local Codex Account) | Reads the existing `~/.codex/auth.json` and provider configuration |
| Official Codex / ChatGPT OAuth | Token / JSON | Creates an official-account provider |
| Codex Companion / CPA / sub2api | One JSON file, `accounts[]`, or multiple files | Extracts identity, tokens, and supported metadata |
| Agent Identity | Token / JSON | Stores private credentials and generates `AgentAssertion` dynamically |
| API-key provider | Form or API Key JSON | Creates an OpenAI-compatible or relay provider |
| New API connection | `_type: "newapi_channel_conn"` JSON | Maps `key` / `url` into an API-key provider |
| Custom provider | Enter Base URL, key, environment variable, and model manually | Creates a provider usable in Direct or Relay mode |

<details>
<summary><strong>API Key and New API JSON examples</strong></summary>

Standard API Key JSON:

```json
{
  "auth_mode": "apikey",
  "OPENAI_API_KEY": "sk-...",
  "api_base_url": "https://api.example.com/v1",
  "api_provider_id": "example",
  "api_provider_name": "Example API"
}
```

New API connection JSON:

```json
{
  "_type": "newapi_channel_conn",
  "key": "sk-...",
  "url": "https://api.example.com"
}
```

A New API root URL is normalized to an OpenAI-compatible `/v1` base. Generic `key` / `url` fields are treated as connection data only when `_type` is exactly `newapi_channel_conn`.

</details>

## How It Works

```mermaid
flowchart LR
    Manager["Codex Companion<br/>App / CLI / TUI"] --> Registry["Accounts and groups"]
    Codex["ChatGPT / Codex CLI"] -->|"Direct: install Codex config"| Direct["Selected upstream"]
    Codex -->|"Local Relay"| API["127.0.0.1:17687/v1"]
    Registry --> API
    API --> Router["Routing · affinity · failover"]
    Router --> Official["Official Codex OAuth"]
    Router --> Keys["API keys / gateways"]
    Router --> Compatible["OpenAI-compatible"]
```

| Mode | Behavior | Must Codex reload? |
| --- | --- | --- |
| `Auto` | Prefer Direct for a compatible single account, otherwise use Companion | Depends on the resolved mode |
| `Direct` | Install provider configuration and required account material into Codex | Yes |
| `Local Relay` | Keep Codex connected to Companion; Companion injects credentials and routes requests | Once for initial setup; not for later account/group switches |

With `保留官方 Codex 登录` (Preserve Official Codex Login) enabled, third-party API keys are written to provider configuration while the official ChatGPT OAuth login remains in `auth.json`. Companion creates backups before changing Codex configuration and preserves newer user changes where possible.

## Local API Service

The active group is exposed at `http://127.0.0.1:17687/v1` by default:

```bash
curl http://127.0.0.1:17687/v1/responses \
  -H "Authorization: Bearer YOUR_CODEX_COMPANION_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"your-model","input":"hello","stream":false}'
```

- Supports `POST /v1/responses`, `POST /v1/responses/compact`, WebSocket `GET /v1/responses`, and `GET /v1/models`.
- API client keys are shown once; SQLite stores only a SHA-256 hash and short prefix.
- Each client can have a model allowlist and can be disabled, rotated, or deleted independently.
- Cross-origin browser requests always require a valid client key; local non-browser requests can use compatibility or strict-key mode.
- Providers may define a separate `websocket_url`; official accounts and Agent Identity receive their required dynamic headers.
- Request logs store provider, model, status, attempts, and latency without prompts, response bodies, or complete keys.

## CLI and TUI

Common commands:

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

| Command | Purpose |
| --- | --- |
| `install` / `uninstall` | Install or restore Codex configuration |
| `doctor` / `status` | Check integration health or print full status |
| `provider add/import/import-local/list/remove/test/refresh` | Manage and inspect accounts/providers |
| `group create/list/use/set` | Create, activate, and arrange groups |
| `relay start/status/self-test` | Start and diagnose the local API |
| `relay client ...` | Create, inspect, update, rotate, and delete API clients |
| `relay logs/clear-logs/settings` | Manage request audits and runtime policy |
| `repair` | Preview or repair session history and plugin state |
| `token-stats` | Scan local sessions and print token usage |
| `sessions` | Search the local Codex session index |

Every command supports `--help`. Press `?` inside the TUI for shortcuts.

## Local Data and Security Boundaries

Default data locations:

| Path | Contents |
| --- | --- |
| `~/.codex-companion/config.json` | Provider, group, app, and relay settings |
| `~/.codex-companion/auth/` | Private imported account and API-key files |
| `~/.codex-companion/relay/api-service.sqlite3` | API client hashes, request audits, affinity, and translation history |
| `~/.codex-companion/logs/` | Redacted Companion JSONL diagnostics |
| `~/.codex-companion/cache/` | Session-index and token-usage caches |
| `~/.codex/backups/codex-companion/` | Codex installation and repair backups |

Set `CODEX_COMPANION_HOME` to move Companion's data directory and `CODEX_COMPANION_CODEX_DIR` to target another Codex directory.

Security boundaries:

- Imported account material remains local; new and overwritten private auth files use `0600` permissions on Unix.
- Agent Identity private keys are used only for local dynamic signing and are never written to Codex `auth.json`.
- API client keys are shown once and persisted only as hashes.
- Diagnostic logs redact tokens, Authorization, cookies, API keys, private keys, and `AgentAssertion`; they rotate at 2 MB and retain at most five files.
- Repair supports dry-run first; real repairs use transactional backups and roll back the current operation on failure.
- Token cost is an estimate based on local sessions and price snapshots, not an OpenAI or gateway invoice.

<details>
<summary><strong>Custom model pricing</strong></summary>

Create `model-pricing.json` in the Companion data directory:

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

Prices are USD estimates per million tokens. Models without a matching price are marked unpriced instead of silently appearing as `$0`.

</details>

## FAQ

<details>
<summary><strong>Must I restart ChatGPT / Codex after switching accounts?</strong></summary>

Direct mode requires the target app to reload configuration and authentication material. After the initial Local Relay setup, switching accounts or groups does not require a restart.

</details>

<details>
<summary><strong>Does Companion upload my accounts or prompts?</strong></summary>

Companion has no account cloud-sync service. Credentials stay on your machine, and requests go only to the upstreams you configure. Structured audits do not retain prompts or response bodies.

</details>

<details>
<summary><strong>How do I return to my previous Codex configuration?</strong></summary>

Restore the pre-Companion configuration from `设置` (Settings), or run `codex-companion uninstall`. Companion uses its installation backup and avoids overwriting newer user changes.

</details>

<details>
<summary><strong>Why must some gateways use Local Relay?</strong></summary>

Codex uses the Responses API. A provider URL that explicitly targets `/chat/completions` needs Companion to translate requests, SSE events, tool calls, and history, so it cannot be used as a direct endpoint.

</details>

<details>
<summary><strong>Why can the Usage page differ from my upstream invoice?</strong></summary>

Usage reads token events from local Codex sessions and applies built-in or custom price snapshots. Upstreams may use different billing categories, discounts, multipliers, or rounding.

</details>

## Build from Source

You need Node.js 22, pnpm 10.23, the stable Rust toolchain, and the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform. The repository includes `.nvmrc`; run `nvm use` first when using nvm.

```bash
git clone https://github.com/Alexlangl/codex-companion.git
cd codex-companion
corepack enable
pnpm install --frozen-lockfile
pnpm dev
```

By default, `pnpm dev` starts an isolated Companion instance and the automatically discovered ChatGPT app. Legacy Codex apps and Codex CLI fallback remain supported. See the [development guide](README.devcodex.md#english) for Companion-only, local-state, CLI, and advanced launch modes.

Before submitting changes, run:

```bash
pnpm check
cargo test --workspace
```

Build a desktop bundle for the current platform:

```bash
pnpm build:tauri
```

Build only the CLI and TUI:

```bash
cargo build --release -p codex-companion-cli -p codex-companion-tui
```

Maintainers can run the `Release` workflow in GitHub Actions with a version such as `0.1.1` or `v0.1.1`. It builds macOS, Windows, and Linux desktop packages plus CLI/TUI archives, generates `SHA256SUMS`, and updates the Homebrew tap after a stable release.

## License

Codex Companion is released under the [MIT License](LICENSE).
