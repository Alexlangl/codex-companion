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

## What It Does

- Import official Codex account JSON, including Codex Companion, CPA, and sub2api formats.
- Import API Key account JSON or manually add OpenAI-compatible providers.
- Compose multiple accounts into a Provider Group and fallback by priority.
- Single accounts default to direct mode, and you can switch them to local proxy mode from the account card.
- Relay providers whose URL explicitly targets `/chat/completions` are forced through the local relay so Companion can translate Responses requests, tool calls, and multi-turn history.
- Let Companion handle token refresh and request headers for official Codex accounts.
- Refresh account health, subscription state, and recognizable quota information.
- Repair Codex history and plugin state before launching Codex, reducing provider-switch continuity issues.
- Scan local Codex session logs for token usage and estimate fresh-input, cached-input, and output cost by model.

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

Put multiple accounts into one group and fallback in order.

<img src="assets/readme/groups.jpg" alt="Provider groups" width="720">

### Local Proxy

Inspect Companion local proxy status, request logs, upstream errors, and fallback events.

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
      "outputPerMillion": "2.00"
    }
  ],
  "providerMultipliers": {
    "paid-relay": "1.15"
  }
}
```

Prices are USD estimates per one million tokens. Use `providerMultipliers` to represent a gateway markup or discount.

## CLI Usage

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

## TUI Usage

```bash
codex-companion-tui
```

Press `?` inside the TUI to view shortcuts.

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
- Token usage stats only read local Codex session logs.
- Cost is an estimate based on the model price snapshot and local overrides, not an OpenAI or gateway invoice.
