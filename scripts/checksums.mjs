#!/usr/bin/env node
// Write SHA256SUMS.txt for release installers. Only top-level artifact types
// are included; macOS .app directories are represented by their .dmg.

import { createHash } from "node:crypto";
import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";

const root = path.resolve(process.argv[2] ?? ".");
const artifactPattern = /\.(exe|msi|dmg|zip|tar\.gz)$/i;
const files = [];

function walk(directory) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const full = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      walk(full);
    } else if (artifactPattern.test(entry.name)) {
      files.push(full);
    }
  }
}

walk(root);
files.sort((left, right) => left.localeCompare(right));
const lines = files.map((file) => {
  const hash = createHash("sha256").update(readFileSync(file)).digest("hex");
  return `${hash}  ${path.relative(root, file).split(path.sep).join("/")}`;
});
writeFileSync(path.join(root, "SHA256SUMS.txt"), `${lines.join("\n")}\n`, "utf8");
for (const line of lines) console.log(line);
if (files.length === 0) {
  console.error(`no installers found under ${root}`);
  process.exitCode = 1;
}
