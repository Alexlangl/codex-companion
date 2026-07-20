#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";

const version = (process.argv[2] || "").replace(/^v/, "");
if (!/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error("Usage: node scripts/set-release-version.mjs <semver>");
}

updateWorkspaceVersion("Cargo.toml", version);
updateJsonVersion("package.json", version);
updateJsonVersion("apps/desktop/package.json", version);
updateJsonVersion("apps/desktop/src-tauri/tauri.conf.json", version);

function updateWorkspaceVersion(file, nextVersion) {
  const source = readFileSync(file, "utf8");
  const sectionPattern = /(\[workspace\.package\][\s\S]*?\nversion\s*=\s*")[^"]+("[\s\S]*?)(?=\n\[|$)/;
  if (!sectionPattern.test(source)) {
    throw new Error(`Could not find workspace.package version in ${file}`);
  }
  writeFileSync(file, source.replace(sectionPattern, `$1${nextVersion}$2`));
}

function updateJsonVersion(file, nextVersion) {
  const source = JSON.parse(readFileSync(file, "utf8"));
  source.version = nextVersion;
  writeFileSync(file, `${JSON.stringify(source, null, 2)}\n`);
}
