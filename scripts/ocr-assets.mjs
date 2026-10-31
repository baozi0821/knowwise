// 2D OCR 离线资产:从已安装的 npm 包复制 worker/core/lang 到 public/ocr,
// 并生成带版本、体积与 SHA-256 的清单。运行时不访问 CDN。
import { createHash } from "node:crypto";
import { copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(root, "public", "ocr");

const files = [
  {
    from: "node_modules/tesseract.js/dist/worker.min.js",
    to: "worker.min.js",
    role: "worker",
  },
  {
    from: "node_modules/tesseract.js-core/tesseract-core-simd-lstm.wasm.js",
    to: "core/tesseract-core-simd-lstm.wasm.js",
    role: "core",
  },
  {
    from: "node_modules/tesseract.js-core/tesseract-core-simd-lstm.wasm",
    to: "core/tesseract-core-simd-lstm.wasm",
    role: "core",
  },
  {
    from: "node_modules/tesseract.js-core/tesseract-core-lstm.wasm.js",
    to: "core/tesseract-core-lstm.wasm.js",
    role: "core",
  },
  {
    from: "node_modules/tesseract.js-core/tesseract-core-lstm.wasm",
    to: "core/tesseract-core-lstm.wasm",
    role: "core",
  },
  {
    from: "node_modules/@tesseract.js-data/chi_sim/4.0.0_best_int/chi_sim.traineddata.gz",
    to: "lang/chi_sim.traineddata.gz",
    role: "lang",
  },
  {
    from: "node_modules/@tesseract.js-data/eng/4.0.0_best_int/eng.traineddata.gz",
    to: "lang/eng.traineddata.gz",
    role: "lang",
  },
];

await rm(out, { recursive: true, force: true });
await mkdir(join(out, "core"), { recursive: true });
await mkdir(join(out, "lang"), { recursive: true });

const entries = [];
for (const file of files) {
  const source = join(root, file.from);
  const target = join(out, file.to);
  await copyFile(source, target);
  const body = await readFile(target);
  entries.push({
    path: file.to,
    role: file.role,
    bytes: body.byteLength,
    sha256: createHash("sha256").update(body).digest("hex"),
  });
}

const manifest = {
  schemaVersion: "ocr_assets.v1",
  engine: "tesseract.js",
  engineVersion: "6.0.1",
  coreVersion: "6.1.2",
  languages: ["chi_sim", "eng"],
  languageDataVersion: "4.0.0_best_int",
  files: entries,
};
await writeFile(join(out, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
console.log(`OCR assets ready: ${entries.length} files, ${entries.reduce((n, e) => n + e.bytes, 0)} bytes`);
