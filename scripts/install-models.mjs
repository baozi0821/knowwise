#!/usr/bin/env node
// Install the optional local OPUS-MT translation models and ONNX Runtime.
//
// No model weights or native runtime binaries live in this repository. This
// script downloads pinned revisions, verifies SHA-256, and writes only into the
// Knowwise application data directory (or --data-dir).
//
// Usage:
//   node scripts/install-models.mjs
//   node scripts/install-models.mjs --data-dir "C:\Temp\knowwise-data" --source mirror
//   node scripts/install-models.mjs --pair zh-en --models-only
//   node scripts/install-models.mjs --check
//
// Licenses:
//   Helsinki-NLP OPUS-MT model weights: CC-BY-4.0
//   ONNX Runtime: MIT
//   tokenizers / ort Rust crates: Apache-2.0 / MIT

import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import path from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";

const APP_ID = "com.knowwise.app";
const MODEL_RELEASE = "opus-mt-onnx-2026-10";
const RUNTIME_VERSION = "1.23.2";
const MODEL_LICENSE = "CC-BY-4.0";

const MODEL_SOURCES = {
  "zh-en": {
    repo: "Xenova/opus-mt-zh-en",
    commit: "39d480d52a9ea3065a1f117adfe4dbc55de10e6f",
    files: {
      "tokenizer.json": {
        source: "tokenizer.json",
        sha256: "b306d0301cf280bfd647d7067b5ade2a97b987e6d678df110703c002433643ff",
      },
      "config.json": {
        source: "config.json",
        sha256: "293d318fce41dbf04114eac45037bb88a32d7c4ee21011a75e24a8b98ca45ad1",
      },
      "generation_config.json": {
        source: "generation_config.json",
        sha256: "8dc29fef0fe82109f94ef3c2e6ea6bded3215d357b226c34cf7b4630726766c9",
      },
      "source.spm": {
        source: "source.spm",
        sha256: "e27a3a1b539f4959ec72ea60e453f49156289f95d4e6000b29332efc45616203",
      },
      "target.spm": {
        source: "target.spm",
        sha256: "6a881f4717cd7265f53fea54fd3dc689c767c05338fac7a4590f3088cb2d7855",
      },
      "encoder_model.onnx": {
        source: "onnx/encoder_model_quantized.onnx",
        sha256: "84d5e171b626bc8b6b220d022ac58696e9528c25deeacca62b5cbf4364547a99",
      },
      "decoder_model.onnx": {
        source: "onnx/decoder_model_quantized.onnx",
        sha256: "debcc3054b9ff9aaed972bd6e251a459abe1e2f1402da1efa63163dc2f46ef73",
      },
    },
  },
  "en-zh": {
    repo: "Xenova/opus-mt-en-zh",
    commit: "046f55aec303cdee3e0318604406d4df20f1e8ea",
    files: {
      "tokenizer.json": {
        source: "tokenizer.json",
        sha256: "d0c7da27056e8f42adce9e76d8e792e5daa64e15f5acd2e7aabf0121877dd4c1",
      },
      "config.json": {
        source: "config.json",
        sha256: "4727d1229a04f95bf6f39abf949d8080615433d99d6ebd85f81c09edd247d5fa",
      },
      "generation_config.json": {
        source: "generation_config.json",
        sha256: "b743baabb7da4c1a2f19fe558bd6b4c0c7c3b0762fcb5ca7a48fe5a2c2219803",
      },
      "source.spm": {
        source: "source.spm",
        sha256: "5775ddc9e3ff2fae91554da56468ad35ff56edaba870fea74447bc7234bfdaa8",
      },
      "target.spm": {
        source: "target.spm",
        sha256: "81dc94efa84e4025ef38d25d5d07429fe41e3eb29d44003f1db6fe98487b0052",
      },
      "encoder_model.onnx": {
        source: "onnx/encoder_model_quantized.onnx",
        sha256: "d3b7912bf6a9bd27e4c074c2df91d4ff3d5b4bc5f7f6c8d7cc9c805c98fbafee",
      },
      "decoder_model.onnx": {
        source: "onnx/decoder_model_quantized.onnx",
        sha256: "2c66a3981099b40edbb3a0d65e015d05964d42fecb9780f8422776eff5939112",
      },
    },
  },
};

const RUNTIMES = {
  "win32-x64": {
    wheel:
      "https://files.pythonhosted.org/packages/cd/6d/738e50c47c2fd285b1e6c8083f15dac1a5f6199213378a5f14092497296d/onnxruntime-1.23.2-cp310-cp310-win_amd64.whl",
    wheelSha256:
      "0be6a37a45e6719db5120e9986fcd30ea205ac8103fd1fb74b6c33348327a0cc",
    entries: [
      {
        from: "onnxruntime/capi/onnxruntime.dll",
        to: "onnxruntime.dll",
        sha256:
          "2bc04b05efbff97049132939640963772cc16225ba1c7f1388993487c8775492",
      },
      {
        from: "onnxruntime/capi/onnxruntime_providers_shared.dll",
        to: "onnxruntime_providers_shared.dll",
        sha256:
          "6adee08db23ba190fd1061ee99c32a5a802cac1e3576defaac820c157294c138",
      },
    ],
  },
  "darwin-arm64": {
    wheel:
      "https://files.pythonhosted.org/packages/35/d6/311b1afea060015b56c742f3531168c1644650767f27ef40062569960587/onnxruntime-1.23.2-cp310-cp310-macosx_13_0_arm64.whl",
    wheelSha256:
      "a7730122afe186a784660f6ec5807138bf9d792fa1df76556b27307ea9ebcbe3",
    entries: [
      {
        from: "onnxruntime/capi/libonnxruntime.1.23.2.dylib",
        to: "libonnxruntime.dylib",
        sha256:
          "480790a978a48ad3e06ce86b6025e037bd70221637f5c104a8dde19617364cf4",
      },
    ],
  },
  "darwin-x64": {
    wheel:
      "https://files.pythonhosted.org/packages/db/db/81bf3d7cecfbfed9092b6b4052e857a769d62ed90561b410014e0aae18db/onnxruntime-1.23.2-cp310-cp310-macosx_13_0_x86_64.whl",
    wheelSha256:
      "b28740f4ecef1738ea8f807461dd541b8287d5650b5be33bca7b474e3cbd1f36",
    entries: [
      {
        from: "onnxruntime/capi/libonnxruntime.1.23.2.dylib",
        to: "libonnxruntime.dylib",
        sha256:
          "479e976d03fadcd1b408a2cf419f32fc25062df6a23d6f4fec28f35805b5a013",
      },
    ],
  },
};

const HF_BASE = "https://huggingface.co";
const MIRROR_BASE = "https://hf-mirror.com";

function arg(name, fallback = undefined) {
  const prefix = `--${name}=`;
  const withEquals = process.argv.find((value) => value.startsWith(prefix));
  if (withEquals) return withEquals.slice(prefix.length);
  const index = process.argv.indexOf(`--${name}`);
  if (index >= 0 && process.argv[index + 1] && !process.argv[index + 1].startsWith("--")) {
    return process.argv[index + 1];
  }
  return fallback;
}

function flag(name) {
  return process.argv.includes(`--${name}`);
}

function defaultDataDir() {
  if (process.platform === "win32") {
    const appData = process.env.APPDATA || path.join(homedir(), "AppData", "Roaming");
    return path.join(appData, APP_ID);
  }
  if (process.platform === "darwin") {
    return path.join(homedir(), "Library", "Application Support", APP_ID);
  }
  const xdg = process.env.XDG_DATA_HOME || path.join(homedir(), ".local", "share");
  return path.join(xdg, APP_ID);
}

function sha256File(file) {
  const hash = createHash("sha256");
  hash.update(readFileSync(file));
  return hash.digest("hex");
}

function sourceBases(mode) {
  if (mode === "hf") return [HF_BASE];
  if (mode === "mirror") return [MIRROR_BASE];
  return [HF_BASE, MIRROR_BASE];
}

async function download(url, destination, timeoutMs = 600_000) {
  let lastError;
  for (let attempt = 1; attempt <= 2; attempt += 1) {
    try {
      const response = await fetch(url, {
        redirect: "follow",
        signal: AbortSignal.timeout(timeoutMs),
        headers: { "user-agent": "knowwise-model-installer/1" },
      });
      if (!response.ok || !response.body) {
        throw new Error(`HTTP ${response.status} ${response.statusText}`);
      }
      const temporary = `${destination}.part`;
      await pipeline(Readable.fromWeb(response.body), createWriteStream(temporary));
      renameSync(temporary, destination);
      return;
    } catch (error) {
      lastError = error;
      if (attempt < 2) await new Promise((resolve) => setTimeout(resolve, 1_500));
    }
  }
  throw lastError;
}

async function downloadVerified(urls, destination, expectedSha256) {
  if (existsSync(destination) && sha256File(destination) === expectedSha256) {
    return false;
  }
  let lastError;
  for (const url of urls) {
    try {
      await download(url, destination);
      const actual = sha256File(destination);
      if (actual !== expectedSha256) {
        rmSync(destination, { force: true });
        throw new Error(`checksum mismatch: expected ${expectedSha256}, got ${actual}`);
      }
      return true;
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError;
}

function extractZip(zipPath, destination) {
  // Git Bash's GNU tar treats "C:\\..." as a remote host. Use the Windows
  // bsdtar that ships with Windows 10+ on win32; macOS ships bsdtar as tar.
  const tarBinary =
    process.platform === "win32"
      ? path.join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe")
      : "tar";
  const result = spawnSync(tarBinary, ["-xf", zipPath, "-C", destination], {
    stdio: "inherit",
  });
  if (result.status !== 0) {
    throw new Error("failed to extract the ONNX Runtime wheel (tar -xf)");
  }
}

function patchTokenizer(file) {
  const tokenizer = JSON.parse(readFileSync(file, "utf8"));
  if (tokenizer?.normalizer?.precompiled_charsmap === null) {
    tokenizer.normalizer = null;
  }
  writeFileSync(file, `${JSON.stringify(tokenizer)}\n`, "utf8");
}

function writeJson(file, value) {
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

async function installRuntime(modelsRoot, mode, checkOnly, force) {
  const key = `${process.platform}-${process.arch}`;
  const runtime = RUNTIMES[key];
  if (!runtime) {
    throw new Error(
      `no ONNX Runtime bundle for ${key}; local translation currently supports Windows x64 and macOS arm64/x64`,
    );
  }
  const runtimeDir = path.join(modelsRoot, "runtime");
  mkdirSync(runtimeDir, { recursive: true });

  if (checkOnly) {
    const missing = runtime.entries.filter((entry) => {
      const target = path.join(runtimeDir, entry.to);
      return !existsSync(target) || sha256File(target) !== entry.sha256;
    });
    console.log(
      missing.length === 0
        ? `runtime OK: ${key} (ONNX Runtime ${RUNTIME_VERSION})`
        : `runtime MISSING/CHANGED: ${missing.map((entry) => entry.to).join(", ")}`,
    );
    return missing.length === 0;
  }

  const wheelPath = path.join(tmpdir(), `knowwise-ort-${RUNTIME_VERSION}-${process.arch}.whl`);
  await downloadVerified([runtime.wheel], wheelPath, runtime.wheelSha256);
  const extractDir = path.join(tmpdir(), `knowwise-ort-${process.pid}-${Date.now()}`);
  mkdirSync(extractDir, { recursive: true });
  try {
    extractZip(wheelPath, extractDir);
    const copied = [];
    for (const entry of runtime.entries) {
      const source = path.join(extractDir, entry.from);
      if (!existsSync(source)) {
        throw new Error(`wheel is missing ${entry.from}`);
      }
      const actual = sha256File(source);
      if (actual !== entry.sha256) {
        throw new Error(`${entry.from} checksum mismatch`);
      }
      const target = path.join(runtimeDir, entry.to);
      if (force || !existsSync(target) || sha256File(target) !== entry.sha256) {
        copyFileSync(source, target);
      }
      copied.push({ file: entry.to, sha256: actual, bytes: statSync(target).size });
    }
    writeJson(path.join(runtimeDir, "manifest.json"), {
      schemaVersion: "local_mt_runtime.v1",
      runtime: "onnxruntime",
      version: RUNTIME_VERSION,
      platform: key,
      source: runtime.wheel,
      wheelSha256: runtime.wheelSha256,
      files: copied,
    });
    writeFileSync(
      path.join(runtimeDir, "NOTICE.txt"),
      [
        "ONNX Runtime",
        `Version: ${RUNTIME_VERSION}`,
        "License: MIT",
        "Project: https://github.com/microsoft/onnxruntime",
        "Downloaded from the official PyPI wheel; see the wheel metadata for the full license text.",
        "",
      ].join("\n"),
      "utf8",
    );
    console.log(`runtime installed: ${key} (ONNX Runtime ${RUNTIME_VERSION})`);
    return true;
  } finally {
    rmSync(extractDir, { recursive: true, force: true });
    rmSync(wheelPath, { force: true });
  }
}

async function installPair(pair, modelsRoot, mode, checkOnly, force) {
  const source = MODEL_SOURCES[pair];
  if (!source) {
    throw new Error(`unknown pair ${pair}; choose zh-en, en-zh, or both`);
  }
  const targetDir = path.join(modelsRoot, pair);
  mkdirSync(targetDir, { recursive: true });

  if (checkOnly) {
    const problems = [];
    for (const [targetName, file] of Object.entries(source.files)) {
      const target = path.join(targetDir, targetName);
      if (!existsSync(target)) {
        problems.push(`${targetName}: missing`);
        continue;
      }
      if (targetName === "tokenizer.json") {
        try {
          JSON.parse(readFileSync(target, "utf8"));
        } catch {
          problems.push(`${targetName}: invalid JSON`);
        }
        continue;
      }
      const actual = sha256File(target);
      if (actual !== file.sha256) {
        problems.push(`${targetName}: checksum mismatch`);
      }
    }
    console.log(
      problems.length === 0
        ? `model OK: ${pair} (${source.repo}@${source.commit.slice(0, 12)})`
        : `model NEEDS REPAIR: ${pair}: ${problems.join("; ")}`,
    );
    return problems.length === 0;
  }

  const bases = sourceBases(mode);
  const installed = [];
  for (const [targetName, file] of Object.entries(source.files)) {
    const target = path.join(targetDir, targetName);
    if (!force && existsSync(target)) {
      if (targetName === "tokenizer.json") {
        installed.push({ file: targetName, source: file.source, sha256: file.sha256 });
        continue;
      }
      if (sha256File(target) === file.sha256) {
        installed.push({ file: targetName, source: file.source, sha256: file.sha256 });
        continue;
      }
    }
    const urls = bases.map(
      (base) => `${base}/${source.repo}/resolve/${source.commit}/${file.source}`,
    );
    await downloadVerified(urls, target, file.sha256);
    if (targetName === "tokenizer.json") {
      patchTokenizer(target);
    }
    installed.push({
      file: targetName,
      source: file.source,
      sha256: file.sha256,
      bytes: statSync(target).size,
    });
    console.log(`  ${pair}/${targetName} (${Math.round(statSync(target).size / 1024 / 1024)} MiB)`);
  }

  writeJson(path.join(targetDir, "manifest.json"), {
    schemaVersion: "local_mt_model.v1",
    release: MODEL_RELEASE,
    pair,
    source: {
      repo: source.repo,
      commit: source.commit,
      url: `${HF_BASE}/${source.repo}`,
      conversion: "Xenova ONNX quantized export",
    },
    license: MODEL_LICENSE,
    licenseUrl: `${HF_BASE}/Helsinki-NLP/opus-mt-${pair}`,
    attribution: "Helsinki-NLP OPUS-MT (University of Helsinki), ONNX conversion by Xenova",
    files: installed,
    notes: [
      "tokenizer.json is normalized at install time: the Transformers.js export carries a null Precompiled charsmap, which the Rust tokenizers crate rejects.",
      "Model weights are not part of the Knowwise source repository.",
    ],
  });
  console.log(`model installed: ${pair}`);
  return true;
}

function writeLicense(modelsRoot) {
  writeFileSync(
    path.join(modelsRoot, "MODEL_LICENSES.md"),
    [
      "# Local translation model notices",
      "",
      "The optional local translation models installed by this script are:",
      "",
      "- Helsinki-NLP / OPUS-MT (University of Helsinki), translated with Marian",
      "- ONNX conversions published by Xenova",
      "",
      "License: Creative Commons Attribution 4.0 International (CC-BY-4.0)",
      "https://creativecommons.org/licenses/by/4.0/",
      "",
      "Model repositories:",
      "- https://huggingface.co/Helsinki-NLP/opus-mt-zh-en",
      "- https://huggingface.co/Helsinki-NLP/opus-mt-en-zh",
      "- https://huggingface.co/Xenova/opus-mt-zh-en",
      "- https://huggingface.co/Xenova/opus-mt-en-zh",
      "",
      "ONNX Runtime is licensed under the MIT License:",
      "https://github.com/microsoft/onnxruntime/blob/main/LICENSE",
      "",
      "These weights are downloaded at the user's request and are not redistributed",
      "inside the Knowwise source repository or installer.",
      "",
    ].join("\n"),
    "utf8",
  );
}

async function main() {
  const dataDir = path.resolve(arg("data-dir", defaultDataDir()));
  const modelsRoot = path.join(dataDir, "models", "translate");
  const mode = arg("source", "auto");
  const pairArg = arg("pair", "both");
  const checkOnly = flag("check");
  const force = flag("force");
  const modelsOnly = flag("models-only");
  const runtimeOnly = flag("runtime-only");
  const pairs =
    pairArg === "both"
      ? Object.keys(MODEL_SOURCES)
      : pairArg
          .split(",")
          .map((value) => value.trim())
          .filter(Boolean);

  console.log(`Knowwise local translation installer (${MODEL_RELEASE})`);
  console.log(`data dir: ${dataDir}`);
  console.log(`models:   ${pairs.join(", ")}`);

  mkdirSync(modelsRoot, { recursive: true });
  let ok = true;
  if (!modelsOnly) {
    ok = (await installRuntime(modelsRoot, mode, checkOnly, force)) && ok;
  }
  if (!runtimeOnly) {
    for (const pair of pairs) {
      ok = (await installPair(pair, modelsRoot, mode, checkOnly, force)) && ok;
    }
  }
  if (!checkOnly) writeLicense(modelsRoot);

  if (!ok) {
    process.exitCode = 1;
    console.error(
      "one or more local translation files are missing or invalid; rerun with --force to repair",
    );
    return;
  }
  console.log("");
  console.log(checkOnly ? "local translation installation is complete and verified." : "done.");
  console.log(`model directory: ${modelsRoot}`);
  console.log(
    "Restart Knowwise, then use inline translation (selection toolbar). Full-document translation uses the configured model provider.",
  );
}

main().catch((error) => {
  console.error(`install-models failed: ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 1;
});
