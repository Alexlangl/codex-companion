# Codex Companion

> 中文版本: [README.md](README.md)

Codex Companion is a local provider runtime for Codex Desktop and Codex CLI.

Codex Desktop / CLI only needs one stable local `base_url`. Companion handles official Codex accounts, API-key based OpenAI-compatible providers, relay providers, provider groups, fallback, cooldown, health refresh, token usage, and Codex history/plugin namespace repair behind that local runtime.

> Screenshots below use sanitized demo data from the browser preview. They do not contain real account tokens or real Codex usage.

### What It Solves

Codex's native provider config works well for a single provider, but it is not enough for hot switching between official accounts, API gateways, and third-party OpenAI-compatible providers. Codex Companion moves that routing logic into a local runtime:

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

Core goals:

- In Provider Group mode, Codex points to the local `codex-companion` `base_url`.
- Providers inside a group can fallback by priority, enter cooldown, refresh health, and hot switch without restarting Codex.
- A single provider can start in direct mode or local relay mode.
- Official Codex accounts must use Companion relay because Companion maintains OAuth token refresh, `ChatGPT-Account-Id`, and Codex-specific request headers.
- When Codex is launched through Companion, history sessions and plugin provider namespaces can be repaired before startup to preserve continuity.

### Screenshots

#### Dashboard

View the current group, local relay URL, available accounts, and Codex integration status.

<img src="assets/readme/dashboard.jpg" alt="Codex Companion dashboard" width="720">

#### Providers

Manage many providers in a compact list with health refresh, launch mode selection, direct mode, and local relay mode.

<img src="assets/readme/providers-compact.jpg" alt="Provider compact list" width="720">

#### Add Provider

Import API Key accounts, Token / JSON accounts, CPA/sub2api/cockpit JSON batches, or local Codex accounts.

<img src="assets/readme/provider-add-dialog.jpg" alt="Add provider dialog" width="720">

#### Groups

Compose ordered provider groups. Fallback follows `provider_order`.

<img src="assets/readme/groups.jpg" alt="Provider groups" width="720">

#### Relay

Inspect the local relay address, launch mode explanation, request logs, fallback logs, and upstream errors.

<img src="assets/readme/relay.jpg" alt="Relay page" width="720">

#### Repair

Run dry-run, backup, and namespace repair for Codex history and plugin state.

<img src="assets/readme/repair.jpg" alt="Repair page" width="720">

#### Token Usage

Scan Codex session JSONL and aggregate token usage by day, model, and provider.

<img src="assets/readme/token-usage.jpg" alt="Token usage page" width="720">

### Provider Model

| kind | Purpose | Direct mode |
| --- | --- | --- |
| `official_codex` | Official Codex account imported from CPA/sub2api/cockpit OAuth JSON or local Codex auth | No. It requires Companion relay. |
| `openai_compatible` | OpenRouter, New API, custom gateways, or other OpenAI-compatible APIs | Yes, if Codex can express the provider with `base_url + env api key`. It can also use relay mode. |
| `relay_provider` | Another local or remote relay | Depends on its auth mode. |

API Key JSON import supports data shaped like:

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

### Launch Modes

| Mode | Codex config | Repair target | Use case |
| --- | --- | --- | --- |
| Provider Group / Relay | local `codex-companion` provider | `codex-companion` | Multi-provider fallback, cooldown, hot switching |
| Single Provider / Direct | provider's own `base_url + env api key` | provider id | Direct launch for API-key providers |
| Single Provider / Relay | local `codex-companion` provider | `codex-companion` | Official accounts or providers whose key is injected by Companion |

Fallback only happens before the response stream starts. After the first stream chunk is written back to Codex, Companion will not silently switch providers.

### Repository Layout

```text
apps/desktop          Tauri v2 + React + Vite + Radix UI
crates/core           Shared types, config store, paths, errors
crates/provider       Provider registry, JSON import, account refresh, group selection
crates/relay          Local HTTP relay, proxy, stream passthrough, fallback
crates/health         Error classification, health state, cooldown
crates/state          Codex install, doctor, history/plugin repair, token usage
crates/daemon         Shared runtime facade for App / CLI / TUI
crates/cli            CLI entry
crates/tui            ratatui terminal entry
ARCHITECTURE.md       Architecture details
```

### Development

Use Node 22:

```bash
source ~/.nvm/nvm.sh
nvm use 22
pnpm install
```

Start the desktop app in development mode:

```bash
pnpm dev
```

`pnpm dev` prefers `127.0.0.1:1420`. If the port is already used, the dev script finds the next available port and injects the same dev URL into Tauri and Vite. Production builds use bundled frontend assets and do not occupy 1420. The local relay defaults to `17687`.

Set a preferred dev port:

```bash
CODEX_COMPANION_DEV_PORT=15000 pnpm dev
```

Checks and tests:

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

The TUI supports provider import/refresh, group composition, repair, token scanning, and launch actions. Press `?` inside the TUI for shortcuts.

### Data And Safety Boundaries

- Companion stores its own configuration in its own data directory.
- Provider config stores `auth_ref`; it does not inline full tokens into the provider list.
- API keys can use `env:<VAR>` or be saved into Companion's local auth files and injected by relay.
- Repair produces a plan first. Real repair creates backups before writing.
- Token usage only reads local Codex session JSONL. It does not estimate cost.

### Architecture

The repository is a Rust workspace plus a pnpm desktop app. Desktop, CLI, and TUI all call the same Rust daemon/core/provider/state APIs, so business logic is not copied into each UI entry.

See [ARCHITECTURE.md](ARCHITECTURE.md) for module-level details.

### Scope

Codex Companion is its own runtime. It is not a cc-switch wrapper and does not try to import or patch cc-switch state.
