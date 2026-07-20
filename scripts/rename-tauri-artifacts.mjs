#!/usr/bin/env node
import { copyFileSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import path from "node:path";

const version = process.env.CODEX_COMPANION_VERSION || "dev";
const suffix = process.env.CODEX_COMPANION_ARTIFACT_SUFFIX || "unknown";
const root = path.resolve("target");
const output = path.resolve("release-assets");
const base = `Codex-Companion-${version}-${suffix}`;
const candidates = [];

rmSync(output, { force: true, recursive: true });
mkdirSync(output, { recursive: true });
walk(root);

const used = new Map();
for (const file of candidates) {
  const kind = artifactKind(file);
  const ext = artifactExtension(file);
  const stem = kind ? `${base}-${kind}` : base;
  const count = used.get(stem) ?? 0;
  used.set(stem, count + 1);
  const dedupe = count === 0 ? "" : `-${count + 1}`;
  copyFileSync(file, path.join(output, `${stem}${dedupe}${ext}`));
}

if (candidates.length === 0) {
  throw new Error("No Tauri bundle artifacts found under target/**/bundle");
}

function walk(dir) {
  let entries = [];
  try {
    entries = readdirSync(dir);
  } catch {
    return;
  }

  for (const entry of entries) {
    const file = path.join(dir, entry);
    const stats = statSync(file);
    if (stats.isDirectory()) {
      walk(file);
      continue;
    }
    if (file.includes(`${path.sep}bundle${path.sep}`) && isReleaseArtifact(file)) {
      candidates.push(file);
    }
  }
}

function isReleaseArtifact(file) {
  return /\.(dmg|deb|rpm|msi|exe|AppImage|zip)$/i.test(file);
}

function artifactExtension(file) {
  const name = path.basename(file);
  if (name.endsWith(".AppImage")) return ".AppImage";
  return path.extname(name);
}

function artifactKind(file) {
  const lower = path.basename(file).toLowerCase();
  if (lower.endsWith(".dmg")) return "dmg";
  if (lower.endsWith(".deb")) return "deb";
  if (lower.endsWith(".rpm")) return "rpm";
  if (lower.endsWith(".msi")) return "msi";
  if (lower.endsWith(".appimage")) return "appimage";
  if (lower.endsWith(".zip")) return "portable";
  if (lower.includes("setup")) return "setup";
  if (lower.endsWith(".exe")) return "portable";
  return "";
}
