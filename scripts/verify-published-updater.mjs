#!/usr/bin/env node

const version = requiredEnvironmentVariable("CODEX_COMPANION_VERSION").replace(/^v/, "");
const repository = process.env.GITHUB_REPOSITORY || "Alexlangl/codex-companion";
const latestBaseUrl = process.env.CODEX_COMPANION_LATEST_BASE_URL ||
  `https://github.com/${repository}/releases/latest/download`;
const releaseBaseUrl = `https://github.com/${repository}/releases/download/v${version}/`;
const attempts = positiveInteger(process.env.CODEX_COMPANION_VERIFY_ATTEMPTS, 12);
const delayMs = positiveInteger(process.env.CODEX_COMPANION_VERIFY_DELAY_MS, 5_000);
const timeoutMs = positiveInteger(process.env.CODEX_COMPANION_VERIFY_TIMEOUT_MS, 30_000);
const manifestNames = [
  "latest.json",
  "latest-darwin-aarch64-app.json",
  "latest-darwin-x86_64-app.json",
  "latest-linux-aarch64-appimage.json",
  "latest-linux-aarch64-deb.json",
  "latest-linux-aarch64-rpm.json",
  "latest-linux-x86_64-appimage.json",
  "latest-linux-x86_64-deb.json",
  "latest-linux-x86_64-rpm.json",
  "latest-windows-x86_64-msi.json",
  "latest-windows-x86_64-nsis.json",
];

const requiredPlatforms = [
  "darwin-aarch64",
  "darwin-x86_64",
  "darwin-aarch64-app",
  "darwin-x86_64-app",
  "windows-x86_64",
  "windows-x86_64-msi",
  "windows-x86_64-nsis",
  "linux-aarch64",
  "linux-aarch64-appimage",
  "linux-aarch64-deb",
  "linux-aarch64-rpm",
  "linux-x86_64",
  "linux-x86_64-appimage",
  "linux-x86_64-deb",
  "linux-x86_64-rpm",
];

await verifyWithRetry();
console.log(`Verified ${manifestNames.length} updater manifests and ${requiredPlatforms.length} platform assets for v${version}`);

async function verifyWithRetry() {
  let lastError;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      await verifyPublishedState();
      return;
    } catch (error) {
      lastError = error;
      if (attempt >= attempts) {
        break;
      }
      console.warn(`Updater verification attempt ${attempt}/${attempts} failed: ${error.message}`);
      await sleep(delayMs);
    }
  }
  throw lastError;
}

async function verifyPublishedState() {
  const manifests = await Promise.all(
    manifestNames.map(async (name) => [name, await fetchJson(`${latestBaseUrl}/${name}`)]),
  );

  for (const [name, manifest] of manifests) {
    validateVersion(manifest, name);
    if (name !== "latest.json") {
      validateDynamicManifest(manifest, name);
    }
  }

  const legacyManifest = manifests.find(([name]) => name === "latest.json")?.[1];
  if (!legacyManifest?.platforms || typeof legacyManifest.platforms !== "object") {
    throw new Error("latest.json does not contain a platforms object");
  }

  const assetUrls = requiredPlatforms.map((target) => {
    const entry = legacyManifest.platforms[target];
    validatePlatformEntry(entry, target);
    return entry.url;
  });
  await Promise.all([...new Set(assetUrls)].map(verifyAssetReachable));
}

function validateDynamicManifest(manifest, name) {
  if (typeof manifest.signature !== "string" || !manifest.signature.trim()) {
    throw new Error(`${name} has an empty signature`);
  }
  if (typeof manifest.url !== "string" || !manifest.url.startsWith(releaseBaseUrl)) {
    throw new Error(`${name} has an unexpected asset URL: ${manifest.url}`);
  }
}

function validateVersion(manifest, name) {
  if (!manifest || typeof manifest !== "object") {
    throw new Error(`${name} is not an object`);
  }
  if (manifest.version !== version) {
    throw new Error(`${name} version mismatch: expected ${version}, got ${manifest.version}`);
  }
  if (!manifest.pub_date || Number.isNaN(Date.parse(manifest.pub_date))) {
    throw new Error(`${name} contains an invalid pub_date`);
  }
}

function validatePlatformEntry(entry, target) {
  if (!entry || typeof entry !== "object") {
    throw new Error(`latest.json is missing platform ${target}`);
  }
  if (typeof entry.signature !== "string" || !entry.signature.trim()) {
    throw new Error(`latest.json has an empty signature for ${target}`);
  }
  if (typeof entry.url !== "string" || !entry.url.startsWith(releaseBaseUrl)) {
    throw new Error(`latest.json has an unexpected asset URL for ${target}: ${entry.url}`);
  }
}

async function fetchJson(url) {
  const response = await fetchWithTimeout(url, {
    headers: { Accept: "application/json", "Cache-Control": "no-cache" },
  });
  if (!response.ok) {
    await response.body?.cancel();
    throw new Error(`${url} returned HTTP ${response.status}`);
  }
  try {
    return await response.json();
  } catch (error) {
    throw new Error(`${url} did not return valid JSON: ${error.message}`);
  }
}

async function verifyAssetReachable(url) {
  const response = await fetchWithTimeout(url, {
    headers: {
      Accept: "application/octet-stream",
      Range: "bytes=0-0",
      "Cache-Control": "no-cache",
    },
  });
  const isReachable = response.ok || response.status === 206;
  await response.body?.cancel();
  if (!isReachable) {
    throw new Error(`${url} returned HTTP ${response.status}`);
  }
}

async function fetchWithTimeout(url, options) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await fetch(url, { redirect: "follow", ...options, signal: controller.signal });
  } finally {
    clearTimeout(timer);
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function positiveInteger(raw, fallback) {
  if (!raw) {
    return fallback;
  }
  const value = Number.parseInt(raw, 10);
  if (!Number.isInteger(value) || value <= 0) {
    throw new Error(`Expected a positive integer, got ${raw}`);
  }
  return value;
}

function requiredEnvironmentVariable(name) {
  const value = process.env[name]?.trim();
  if (!value) {
    throw new Error(`Missing environment variable: ${name}`);
  }
  return value;
}
