#!/usr/bin/env node
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

const version = requiredEnvironmentVariable("CODEX_COMPANION_VERSION").replace(/^v/, "");
const repository = process.env.GITHUB_REPOSITORY || "Alexlangl/codex-companion";
const tag = `v${version}`;
const assetRoot = path.resolve("release-assets");
const baseUrl = `https://github.com/${repository}/releases/download/${tag}`;

const assetSpecs = {
  macArm: {
    target: "darwin-aarch64-app",
    suffix: "macos-arm64",
    kind: "updater",
    extension: ".tar.gz",
  },
  macX64: {
    target: "darwin-x86_64-app",
    suffix: "macos-x64",
    kind: "updater",
    extension: ".tar.gz",
  },
  macUniversal: {
    target: null,
    suffix: "macos-universal",
    kind: "updater",
    extension: ".tar.gz",
  },
  linuxArmAppImage: {
    target: "linux-aarch64-appimage",
    suffix: "linux-arm64",
    kind: "appimage",
    extension: ".AppImage",
  },
  linuxX64AppImage: {
    target: "linux-x86_64-appimage",
    suffix: "linux-x64",
    kind: "appimage",
    extension: ".AppImage",
  },
  linuxArmDeb: {
    target: "linux-aarch64-deb",
    suffix: "linux-arm64",
    kind: "deb",
    extension: ".deb",
  },
  linuxX64Deb: {
    target: "linux-x86_64-deb",
    suffix: "linux-x64",
    kind: "deb",
    extension: ".deb",
  },
  linuxArmRpm: {
    target: "linux-aarch64-rpm",
    suffix: "linux-arm64",
    kind: "rpm",
    extension: ".rpm",
  },
  linuxX64Rpm: {
    target: "linux-x86_64-rpm",
    suffix: "linux-x64",
    kind: "rpm",
    extension: ".rpm",
  },
  windowsMsi: {
    target: "windows-x86_64-msi",
    suffix: "windows-x64",
    kind: "msi",
    extension: ".msi",
  },
  windowsNsis: {
    target: "windows-x86_64-nsis",
    suffix: "windows-x64",
    kind: "setup",
    extension: ".exe",
  },
};

const entries = Object.fromEntries(
  Object.entries(assetSpecs).map(([key, spec]) => [key, readAssetEntry(spec)]),
);

const platforms = {
  "darwin-aarch64": entries.macArm,
  "darwin-x86_64": entries.macX64,
  "darwin-aarch64-app": entries.macArm,
  "darwin-x86_64-app": entries.macX64,
  "darwin-universal": entries.macUniversal,
  "windows-x86_64": entries.windowsMsi,
  "windows-x86_64-msi": entries.windowsMsi,
  "windows-x86_64-nsis": entries.windowsNsis,
  "linux-aarch64": entries.linuxArmAppImage,
  "linux-aarch64-appimage": entries.linuxArmAppImage,
  "linux-aarch64-deb": entries.linuxArmDeb,
  "linux-aarch64-rpm": entries.linuxArmRpm,
  "linux-x86_64": entries.linuxX64AppImage,
  "linux-x86_64-appimage": entries.linuxX64AppImage,
  "linux-x86_64-deb": entries.linuxX64Deb,
  "linux-x86_64-rpm": entries.linuxX64Rpm,
};

const manifest = {
  version,
  notes: `Codex Companion ${tag}`,
  pub_date: process.env.CODEX_COMPANION_PUBLISHED_AT || new Date().toISOString(),
  platforms,
};

writeManifest("latest.json", manifest);

for (const [key, spec] of Object.entries(assetSpecs)) {
  if (!spec.target) {
    continue;
  }
  writeManifest(`latest-${spec.target}.json`, {
    version: manifest.version,
    notes: manifest.notes,
    pub_date: manifest.pub_date,
    ...entries[key],
  });
}

function readAssetEntry(spec) {
  const assetName = `Codex-Companion-${version}-${spec.suffix}-${spec.kind}${spec.extension}`;
  const signatureName = `${assetName}.sig`;
  const assetPath = path.join(assetRoot, assetName);
  const signaturePath = path.join(assetRoot, signatureName);
  requireFile(assetPath);
  requireFile(signaturePath);

  return {
    signature: readFileSync(signaturePath, "utf8").trim(),
    url: `${baseUrl}/${encodeURIComponent(assetName)}`,
  };
}

function writeManifest(fileName, value) {
  writeFileSync(path.join(assetRoot, fileName), `${JSON.stringify(value, null, 2)}\n`);
}

function requiredEnvironmentVariable(name) {
  const value = process.env[name]?.trim();
  if (!value) {
    throw new Error(`Missing environment variable: ${name}`);
  }
  return value;
}

function requireFile(file) {
  if (!existsSync(file)) {
    throw new Error(`Missing updater artifact: ${file}`);
  }
}
