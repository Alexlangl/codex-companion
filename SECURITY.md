# Security Policy

## Supported versions

Security fixes are applied to the latest published version of Codex Companion.

## Threat model

Codex Companion is a local desktop application. It runs with the permissions of
the current user, manages Codex configuration and credential files, and exposes
a local HTTP/WebSocket relay. It has no project-operated cloud backend and is
not a privilege boundary between the application and the user who launched it.

The bundled renderer is inside the trust boundary only while all of the
following remain true:

- executable frontend assets are bundled with the application;
- the Content Security Policy restricts script execution to bundled assets;
- no remote page, iframe, webview, `eval`, or `new Function` is introduced;
- untrusted strings do not reach HTML execution sinks; and
- Tauri IPC is not exposed to an unbundled origin.

If any condition changes, the renderer trust decision must be reviewed again.

## In scope

Inputs that cross a trust boundary include:

- inbound HTTP and WebSocket relay requests;
- imported provider, account, and authentication JSON;
- upstream API responses processed or persisted by the relay;
- live Codex configuration and credential files;
- paths and identifiers restored from Companion-managed state;
- any path by which credentials can reach logs, diagnostics, exports, shared
  configuration, or a provider URL; and
- the build, signing, release, and updater pipeline.

The data path matters more than the final API call. A filesystem write or
network request is security-sensitive when an untrusted source controls its
path, destination, or contents.

## Security invariants

- The relay binds to loopback by default. A non-loopback bind must require a
  valid Client API key.
- Browser-originated relay traffic always requires a valid Client API key.
- Credential values are redacted before every persistent log sink.
- Credential files are replaced atomically and use owner-only permissions on
  Unix.
- User-facing provider JSON imports show the credential type, network
  destination, and overwrite behavior before persistence. Non-interactive CLI
  imports require an explicit confirmation flag after the same validation.
- Updater artifacts must pass Tauri signature verification.

## Out of scope

- Direct IPC invocation from DevTools or a locally modified renderer, unless an
  untrusted input can first reach that renderer.
- File operations for which the user explicitly selected both the path and the
  contents, with no untrusted input in the data path.
- Denial of service against the user's own local instance without a wider
  security impact.
- Automated scanner output without a reproducible path on the latest version.

## Reporting a vulnerability

Do not open a public issue containing credential material or an unpatched
vulnerability. Use GitHub Security Advisories for the repository and include:

- the untrusted input source and complete data path;
- reproduction steps against the latest version;
- impact and affected versions; and
- logs or fixtures with all credentials removed.
