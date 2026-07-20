#!/usr/bin/env node
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

const version = requiredEnvironmentVariable("CODEX_COMPANION_VERSION").replace(/^v/, "");
const repository = process.env.GITHUB_REPOSITORY || "Alexlangl/codex-companion";
const tag = `v${version}`;
const assetRoot = path.resolve("release-assets");
const baseUrl = `https://github.com/${repository}/releases/download/${tag}`;

const targets = [
  { platform: "darwin-aarch64", suffix: "macos-arm64", kind: "updater", extension: ".tar.gz" },
  { platform: "darwin-x86_64", suffix: "macos-x64", kind: "updater", extension: ".tar.gz" },
  { platform: "linux-aarch64", suffix: "linux-arm64", kind: "appimage", extension: ".AppImage" },
  { platform: "linux-x86_64", suffix: "linux-x64", kind: "appimage", extension: ".AppImage" },
  { platform: "windows-x86_64", suffix: "windows-x64", kind: "setup", extension: ".exe" },
];

const platforms = Object.fromEntries(targets.map((target) => {
  const assetName = `Codex-Companion-${version}-${target.suffix}-${target.kind}${target.extension}`;
  const signatureName = `${assetName}.sig`;
  const assetPath = path.join(assetRoot, assetName);
  const signaturePath = path.join(assetRoot, signatureName);
  requireFile(assetPath);
  requireFile(signaturePath);

  return [
    target.platform,
    {
      signature: readFileSync(signaturePath, "utf8").trim(),
      url: `${baseUrl}/${assetName}`,
    },
  ];
}));

const manifest = {
  version,
  notes: `Codex Companion ${tag}`,
  pub_date: new Date().toISOString(),
  platforms,
};

writeFileSync(path.join(assetRoot, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`);

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
