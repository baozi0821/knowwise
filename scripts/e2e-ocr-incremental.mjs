// Focused incremental-OCR e2e for the real Windows desktop app.
//
// Covers: 1–3 -> 1–10 -> repeat 1–3 -> page 11 -> restart -> automatic OCR.
// Uses only synthetic PDFs under C:\Temp and an isolated KNOWWISE_DATA_DIR.
//
// Usage: node scripts/e2e-ocr-incremental.mjs
// Prerequisite: a desktop build exists (release/debug), or set KNOWWISE_EXE.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";

const ROOT = process.cwd();
const STAMP = Date.now();
const TEMP_DIR = path.join("C:\\Temp", `knowwise-ocr-e2e-${STAMP}`);
const DATA_DIR = path.join("C:\\Temp", `knowwise-ocr-e2e-data-${STAMP}`);
let APP_PID = null;
let cdp = null;
const RESULTS = [];
const createdDocuments = [];

function record(name, pass, detail = null) {
  RESULTS.push({ name, pass, detail });
  console.log(`${pass ? "PASS" : "FAIL"} ${name} :: ${JSON.stringify(detail)}`);
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function ensureApp() {
  try {
    const version = await (await fetch("http://127.0.0.1:9222/json/version")).json();
    if (version.webSocketDebuggerUrl) {
      throw new Error("9222 端口已有应用;请先关闭 Knowwise 再运行 e2e");
    }
  } catch (error) {
    if (error instanceof Error && error.message.includes("9222")) throw error;
  }
  // 仅在 KNOWWISE_EXE 是不同 bundle identifier 的测试构建时设为 1；
  // 不能用于用户已安装/正在使用的应用实例。
  const allowRunning = process.env.KNOWWISE_E2E_ALLOW_RUNNING === "1";
  const running = spawnSync("tasklist", ["/FI", "IMAGENAME eq knowwise.exe", "/NH"], {
    encoding: "utf8",
  });
  if (
    !allowRunning &&
    running.status === 0 &&
    /knowwise\.exe/i.test(running.stdout ?? "")
  ) {
    // 重启场景：给刚被 taskkill 的进程一点退出时间。
    for (let attempt = 0; attempt < 20; attempt += 1) {
      await sleep(500);
      const again = spawnSync("tasklist", ["/FI", "IMAGENAME eq knowwise.exe", "/NH"], {
        encoding: "utf8",
      });
      if (!(again.status === 0 && /knowwise\.exe/i.test(again.stdout ?? ""))) return ensureApp();
    }
    throw new Error("检测到 Knowwise 正在运行;请先关闭,避免读写用户数据");
  }
  const candidates = [
    process.env.KNOWWISE_EXE,
    path.join(ROOT, "src-tauri", "target", "release", "knowwise.exe"),
    path.join(ROOT, "src-tauri", "target", "debug", "knowwise.exe"),
  ].filter((candidate) => typeof candidate === "string" && candidate.length > 0);
  const exe = candidates.find((candidate) => existsSync(candidate));
  if (exe === undefined) {
    throw new Error("未找到 knowwise.exe;请先构建,或设置 KNOWWISE_EXE");
  }
  mkdirSync(DATA_DIR, { recursive: true });
  const webviewDir = path.join(DATA_DIR, `webview2-${Date.now()}`);
  mkdirSync(webviewDir, { recursive: true });
  const child = spawn(exe, [], {
    cwd: path.dirname(exe),
    detached: true,
    stdio: "ignore",
    env: {
      ...process.env,
      KNOWWISE_DATA_DIR: DATA_DIR,
      WEBVIEW2_USER_DATA_FOLDER: webviewDir,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:
        "--remote-debugging-port=9222 --remote-allow-origins=*",
    },
  });
  child.unref();
  APP_PID = child.pid ?? null;
  for (let attempt = 0; attempt < 60; attempt += 1) {
    await sleep(500);
    try {
      const version = await (await fetch("http://127.0.0.1:9222/json/version")).json();
      if (version.webSocketDebuggerUrl) {
        cdp = await Cdp.connect();
        return;
      }
    } catch {
      // retry
    }
  }
  throw new Error("应用未在 9222 端口暴露 CDP");
}

async function killApp() {
  const current = cdp;
  cdp = null;
  if (current) {
    try {
      // 优先正常关闭窗口，让应用自己 flush/关闭 SQLite；失败再强杀。
      await current.send("Page.close");
      await sleep(1200);
    } catch {
      // ignore
    }
    try {
      current.close();
    } catch {
      // ignore
    }
  }
  if (APP_PID !== null) {
    spawnSync("taskkill", ["/PID", String(APP_PID), "/T", "/F"], { stdio: "ignore" });
    const pid = String(APP_PID);
    APP_PID = null;
    for (let attempt = 0; attempt < 40; attempt += 1) {
      const out = spawnSync("tasklist", ["/FI", `PID eq ${pid}`, "/NH"], {
        encoding: "utf8",
      });
      if (!(out.status === 0 && out.stdout?.includes(pid))) return;
      await sleep(250);
    }
  }
}

async function dumpUiState(label) {
  try {
    return await cdp.eval(`({
      title: document.title,
      url: location.href,
      body: (document.body?.innerText ?? "").slice(0, 800),
      hasWorkspace: !!document.querySelector('.workspace-body'),
      hasSidebar: !!document.querySelector('.doc-sidebar'),
      hasRail: !!document.querySelector('.sidebar-collapsed-rail'),
      hasReader: !!document.querySelector('.reader-region'),
      ipc: !!window.__TAURI_INTERNALS__,
    })`);
  } catch (error) {
    return { label, error: error instanceof Error ? error.message : String(error) };
  }
}

class Cdp {
  constructor(ws, pageId) {
    this.ws = ws;
    this.pageId = pageId;
    this.nextId = 1;
    this.pending = new Map();
    ws.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      const entry = this.pending.get(message.id);
      if (entry === undefined) return;
      this.pending.delete(message.id);
      if (message.error) entry.reject(new Error(JSON.stringify(message.error)));
      else entry.resolve(message.result);
    });
  }
  static async connect() {
    const targets = await (await fetch("http://127.0.0.1:9222/json")).json();
    const page = targets.find((target) => target.type === "page" && !target.url.includes("worker"));
    if (page === undefined) throw new Error("没有可用的页面 target");
    const ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      ws.addEventListener("open", resolve, { once: true });
      ws.addEventListener("error", reject, { once: true });
    });
    return new Cdp(ws, page.id);
  }
  send(method, params = {}) {
    const id = this.nextId++;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
  async eval(expression) {
    const out = await this.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
      userGesture: true,
    });
    if (out.exceptionDetails) {
      throw new Error(out.exceptionDetails.exception?.description ?? JSON.stringify(out.exceptionDetails));
    }
    return out.result?.value;
  }
  async invoke(command, request) {
    const payload = request === undefined ? "undefined" : JSON.stringify(request);
    return this.eval(
      `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, { request: ${payload} })`,
    );
  }
  close() {
    this.ws.close();
  }
}

async function waitFor(predicate, timeoutMs, label) {
  const started = Date.now();
  let lastError = null;
  while (Date.now() - started < timeoutMs) {
    try {
      if (await predicate()) return true;
    } catch (error) {
      lastError = error;
    }
    await sleep(500);
  }
  throw new Error(`等待超时:${label}${lastError ? ` (${lastError.message})` : ""}`);
}

async function ensureSidebarOpen() {
  const has = await cdp.eval(`document.querySelector('.doc-sidebar')!==null`);
  if (!has) {
    await cdp.eval(
      `(()=>{const b=[...document.querySelectorAll('button')].find(x=>x.getAttribute('aria-label')==='展开文档库'); if(b)b.click(); return true;})()`,
    );
    await sleep(400);
  }
}

async function openDocumentByName(name) {
  await ensureSidebarOpen();
  const found = await cdp.eval(`(()=>{
    const items=[...document.querySelectorAll('.doc-sidebar button.doc-item-main')]
      .filter(b=>b.textContent?.trim().startsWith(${JSON.stringify(name)}));
    const target=items[0];
    if(!target) return false;
    target.click();
    return true;
  })()`);
  if (!found) throw new Error(`侧栏找不到文档:${name}`);
  await sleep(2500);
  await waitFor(() => cdp.eval(`!!document.querySelector('.reader-toolbar')`), 30000, "打开阅读器");
  await waitFor(
    () => cdp.eval(`!!document.querySelector('.pdf-canvas, .pdf-page-frame')`),
    30000,
    "PDF 页渲染",
  );
}

async function statusMap(documentId, versionId) {
  const rows = await cdp.invoke("page_ocr_get_status", { documentId, versionId });
  return new Map(rows.map((row) => [row.pageNumber, row]));
}

function pageRow(rows, pageNumber) {
  return rows.get(pageNumber);
}

function assertDone(rows, pageNumber, name) {
  const row = pageRow(rows, pageNumber);
  const ok =
    row !== undefined &&
    (row.status === "completed" || row.status === "low_confidence") &&
    row.blockCount > 0;
  if (!ok) {
    throw new Error(`${name}: 第 ${pageNumber} 页未完成:${JSON.stringify(row)}`);
  }
}

function assertPending(rows, pageNumber, name) {
  const row = pageRow(rows, pageNumber);
  if (row === undefined || row.status !== "pending") {
    throw new Error(`${name}: 第 ${pageNumber} 页应为 pending,实际 ${JSON.stringify(row)}`);
  }
}

async function blockIds(documentId, versionId, pageNumber) {
  const text = await cdp.invoke("page_ocr_get_text", { documentId, versionId, pageNumber });
  return text.blocks.map((block) => block.blockId);
}

async function setAutoOcr(enabled) {
  const view = await cdp.invoke("settings_get", {});
  await cdp.invoke("settings_update", {
    patch: { autoOcr: enabled },
    expectedRevision: view.revision,
  });
  const after = await cdp.invoke("settings_get", {});
  if (after.settings.autoOcr !== enabled) {
    throw new Error(`自动 OCR 设置未生效:期望 ${enabled},实际 ${after.settings.autoOcr}`);
  }
  await sleep(300);
}

async function makeScanPdf(filePath, pageCount) {
  const images = [];
  for (let page = 1; page <= pageCount; page += 1) {
    const dataUrl = await cdp.eval(`(()=>{
      const c=document.createElement('canvas');c.width=1200;c.height=1600;
      const ctx=c.getContext('2d');
      ctx.fillStyle='#fdfdfb';ctx.fillRect(0,0,c.width,c.height);
      ctx.fillStyle='#111';ctx.font='72px Georgia, "Times New Roman", serif';
      ctx.fillText('SCAN PAGE ${page}',90,280);
      ctx.font='58px Georgia, serif';
      ctx.fillText('Incremental OCR e2e page ${page}.',90,900);
      return c.toDataURL('image/jpeg',0.92);
    })()`);
    images.push(Buffer.from(dataUrl.split(",")[1], "base64"));
  }
  const parts = [];
  let length = 0;
  const offsets = [0];
  const push = (buf) => {
    const body = Buffer.isBuffer(buf) ? buf : Buffer.from(buf, "latin1");
    parts.push(body);
    length += body.length;
  };
  const startObj = (n) => {
    offsets[n] = length;
    push(`${n} 0 obj\n`);
  };
  const endObj = () => push("\nendobj\n");
  push("%PDF-1.4\n");
  startObj(1);
  push("<< /Type /Catalog /Pages 2 0 R >>");
  endObj();
  const kids = images.map((_, index) => `${3 + index * 3} 0 R`).join(" ");
  startObj(2);
  push(`<< /Type /Pages /Kids [${kids}] /Count ${images.length} >>`);
  endObj();
  images.forEach((jpeg, index) => {
    const pageNo = 3 + index * 3;
    const contentNo = pageNo + 1;
    const imageNo = pageNo + 2;
    startObj(pageNo);
    push(
      `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents ${contentNo} 0 R /Resources << /XObject << /Im0 ${imageNo} 0 R >> >> >>`,
    );
    endObj();
    const content = "q\n612 0 0 792 0 0 cm\n/Im0 Do\nQ\n";
    startObj(contentNo);
    push(`<< /Length ${Buffer.byteLength(content)} >>\nstream\n${content}endstream`);
    endObj();
    startObj(imageNo);
    push(
      `<< /Type /XObject /Subtype /Image /Width 1200 /Height 1600 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length ${jpeg.length} >>\nstream\n`,
    );
    push(jpeg);
    push("\nendstream");
    endObj();
  });
  const objCount = 2 + images.length * 3;
  const xref = length;
  push(`xref\n0 ${objCount + 1}\n0000000000 65535 f \n`);
  for (let n = 1; n <= objCount; n += 1) push(`${String(offsets[n]).padStart(10, "0")} 00000 n \n`);
  push(`trailer\n<< /Size ${objCount + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`);
  writeFileSync(filePath, Buffer.concat(parts));
}

async function openOcrDialog() {
  const opened = await cdp.eval(
    `(()=>{const b=document.querySelector('.reader-ocr-pages-btn'); if(!b) return false; b.click(); return true;})()`,
  );
  if (!opened) throw new Error("找不到阅读器 OCR 按钮");
  await waitFor(
    () => cdp.eval(`!!document.querySelector('.ocr-recognize-dialog')`),
    10000,
    "打开 OCR 对话框",
  );
}

async function setOcrSpec(spec) {
  const ok = await cdp.eval(`(()=>{
    const input=document.querySelector('.ocr-recognize-dialog input[aria-label="OCR 页码"]');
    if(!input) return false;
    const setter=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set;
    setter.call(input, ${JSON.stringify(spec)});
    input.dispatchEvent(new Event('input',{bubbles:true}));
    return true;
  })()`);
  if (!ok) throw new Error("找不到 OCR 页码输入框");
  await sleep(150);
}

async function clickDialogText(label) {
  const ok = await cdp.eval(`(()=>{
    const buttons=[...document.querySelectorAll('.ocr-recognize-dialog button')];
    const target=buttons.find((b)=>b.textContent?.includes(${JSON.stringify(label)}));
    if(!target || target.disabled) return false;
    target.click();
    return true;
  })()`);
  if (!ok) throw new Error(`OCR 对话框按钮不可用:${label}`);
  await sleep(200);
}

async function waitDialogDone(timeoutMs) {
  await waitFor(
    () =>
      cdp.eval(`(()=>{
        const d=document.querySelector('.ocr-recognize-dialog');
        if(!d) return true;
        const progress=d.querySelector('.ocr-recognize-progress');
        const summary=d.querySelector('.ocr-recognize-summary');
        const error=d.querySelector('.settings-error');
        return progress===null && (summary!==null || error!==null);
      })()`),
    timeoutMs,
    "OCR 对话框完成",
  );
  const error = await cdp.eval(
    `document.querySelector('.ocr-recognize-dialog .settings-error')?.textContent?.trim() ?? null`,
  );
  if (error !== null) throw new Error(`OCR 失败:${error}`);
  return cdp.eval(
    `document.querySelector('.ocr-recognize-dialog .ocr-recognize-summary')?.textContent?.trim() ?? null`,
  );
}

async function closeDialog() {
  await cdp.eval(
    `(()=>{const b=document.querySelector('.ocr-recognize-dialog .kw-confirm-head button'); if(b)b.click(); return true;})()`,
  );
  await waitFor(
    () => cdp.eval(`!document.querySelector('.ocr-recognize-dialog')`),
    5000,
    "关闭 OCR 对话框",
  );
}

async function recognizeSpec(spec, timeoutMs = 300000) {
  await openOcrDialog();
  await setOcrSpec(spec);
  await clickDialogText("识别所选页");
  const summary = await waitDialogDone(timeoutMs);
  await closeDialog();
  return summary;
}

async function quickRecognize(label, timeoutMs = 300000) {
  await openOcrDialog();
  await clickDialogText(label);
  const summary = await waitDialogDone(timeoutMs);
  await closeDialog();
  return summary;
}

async function waitParseTerminal(documentId, timeoutMs = 120000) {
  await waitFor(async () => {
    const status = await cdp.invoke("parse_get_status", { documentId });
    return status.parseState !== "in_progress" && status.parseState !== "not_started";
  }, timeoutMs, "解析终态");
}

async function importAndOpen(filePath, name) {
  const doc = await cdp.invoke("document_import", {
    path: filePath,
    idempotencyKey: `ocr-e2e-${STAMP}-${name}`,
  });
  createdDocuments.push(doc);
  await cdp.send("Page.reload");
  await sleep(3500);
  await waitFor(() => cdp.eval(`!!document.querySelector('.doc-sidebar')`), 20000, "侧栏加载");
  await openDocumentByName(name);
  return doc;
}

async function main() {
  mkdirSync(TEMP_DIR, { recursive: true });
  await ensureApp();
  await waitFor(() => cdp.eval(`!!window.__TAURI_INTERNALS__`), 20000, "Tauri IPC");
  await waitFor(
    () => cdp.eval(`!!document.querySelector('.workspace-body, .doc-sidebar')`),
    30000,
    "工作区加载",
  );
  await setAutoOcr(false);

  const scanName = `ocr-scan-11-${STAMP}`;
  const scanPath = path.join(TEMP_DIR, `${scanName}.pdf`);
  await makeScanPdf(scanPath, 11);
  const doc = await importAndOpen(scanPath, scanName);
  await waitParseTerminal(doc.documentId);

  const initial = await statusMap(doc.documentId, doc.versionId);
  for (let page = 1; page <= 11; page += 1) assertPending(initial, page, "自动 OCR 关闭时");

  const summary1 = await recognizeSpec("1-3");
  record("手动识别 1-3", summary1?.includes("本次识别并保存 3 页") === true, summary1);
  const after1 = await statusMap(doc.documentId, doc.versionId);
  for (let page = 1; page <= 3; page += 1) assertDone(after1, page, "手动 1-3");
  for (let page = 4; page <= 11; page += 1) assertPending(after1, page, "手动 1-3 之后");
  const ids1 = await blockIds(doc.documentId, doc.versionId, 1);

  const summary2 = await quickRecognize("识别前 10 页");
  record("快捷识别前 10 页", summary2?.includes("本次识别并保存 7 页") === true, summary2);
  const after2 = await statusMap(doc.documentId, doc.versionId);
  for (let page = 1; page <= 10; page += 1) assertDone(after2, page, "识别 1-10");
  assertPending(after2, 11, "识别 1-10 之后");
  const ids2 = await blockIds(doc.documentId, doc.versionId, 1);
  record("补识别 4-10 保留 1-3 的块 ID", JSON.stringify(ids1) === JSON.stringify(ids2), {
    before: ids1.length,
    after: ids2.length,
  });
  record(
    "补识别 4-10 保留 1-3 的状态/块数",
    after2.get(1)?.blockCount === after1.get(1)?.blockCount &&
      after2.get(2)?.blockCount === after1.get(2)?.blockCount &&
      after2.get(3)?.blockCount === after1.get(3)?.blockCount,
    {
      before: [after1.get(1), after1.get(2), after1.get(3)].map((row) => row?.blockCount),
      after: [after2.get(1), after2.get(2), after2.get(3)].map((row) => row?.blockCount),
    },
  );

  const summary3 = await recognizeSpec("1-3");
  record("重复请求 1-3 幂等", summary3?.includes("已幂等跳过") === true, summary3);
  const after3 = await statusMap(doc.documentId, doc.versionId);
  const ids3 = await blockIds(doc.documentId, doc.versionId, 1);
  record("重复 1-3 不新增/不改块", JSON.stringify(ids1) === JSON.stringify(ids3), {
    before: ids1.length,
    after: ids3.length,
  });
  record(
    "重复 1-3 不改变 4-10",
    [4, 5, 6, 7, 8, 9, 10].every(
      (page) => after3.get(page)?.blockCount === after2.get(page)?.blockCount,
    ),
    null,
  );

  const summary4 = await recognizeSpec("11");
  record("新增第 11 页", summary4?.includes("本次识别并保存 1 页") === true, summary4);
  const after4 = await statusMap(doc.documentId, doc.versionId);
  assertDone(after4, 11, "新增第 11 页");
  for (let page = 1; page <= 10; page += 1) assertDone(after4, page, "第 11 页之后");
  const ids4 = await blockIds(doc.documentId, doc.versionId, 1);
  record("新增 11 页不改前 10 页块 ID", JSON.stringify(ids1) === JSON.stringify(ids4), null);

  // 重启：同一 KNOWWISE_DATA_DIR，重新打开文档，状态与块 ID 必须持久化。
  await killApp();
  await sleep(1500);
  await ensureApp();
  try {
    await waitFor(
      () => cdp.eval(`!!document.querySelector('.workspace-body')`),
      30000,
      "重启后工作区",
    );
    await ensureSidebarOpen();
    await waitFor(
      () => cdp.eval(`!!document.querySelector('.doc-sidebar')`),
      20000,
      "重启后侧栏",
    );
  } catch (error) {
    throw new Error(
      `${error instanceof Error ? error.message : String(error)} :: ${JSON.stringify(
        await dumpUiState("restart"),
      )}`,
    );
  }
  await openDocumentByName(scanName);
  const afterRestart = await statusMap(doc.documentId, doc.versionId);
  for (let page = 1; page <= 11; page += 1) assertDone(afterRestart, page, "重启后");
  const idsRestart = await blockIds(doc.documentId, doc.versionId, 1);
  record("重启后状态/块 ID 保留", JSON.stringify(ids1) === JSON.stringify(idsRestart), {
    before: ids1.length,
    after: idsRestart.length,
  });

  // 自动 OCR：打开开关后导入新的 2 页扫描件，解析完成时页面应已自动识别。
  await setAutoOcr(true);
  const autoName = `ocr-auto-2-${STAMP}`;
  const autoPath = path.join(TEMP_DIR, `${autoName}.pdf`);
  await makeScanPdf(autoPath, 2);
  const autoDoc = await importAndOpen(autoPath, autoName);
  await waitParseTerminal(autoDoc.documentId, 240000);
  await waitFor(async () => {
    const rows = await statusMap(autoDoc.documentId, autoDoc.versionId);
    return [1, 2].every((page) => {
      const row = rows.get(page);
      return (
        row !== undefined &&
        (row.status === "completed" || row.status === "low_confidence") &&
        row.blockCount > 0
      );
    });
  }, 240000, "自动 OCR 完成");
  const autoRows = await statusMap(autoDoc.documentId, autoDoc.versionId);
  record("自动 OCR 默认策略可用", [1, 2].every((page) => autoRows.get(page)?.blockCount > 0), {
    pages: [autoRows.get(1), autoRows.get(2)],
  });

  // 解析完成后开启自动 OCR：已终态文档的缺失页应由阅读器补识别，
  // 而不是只对“开启后新导入”的文档生效。
  await setAutoOcr(false);
  const lateName = `ocr-late-2-${STAMP}`;
  const latePath = path.join(TEMP_DIR, `${lateName}.pdf`);
  await makeScanPdf(latePath, 2);
  const lateDoc = await importAndOpen(latePath, lateName);
  await waitParseTerminal(lateDoc.documentId, 240000);
  const latePending = await statusMap(lateDoc.documentId, lateDoc.versionId);
  record(
    "自动 OCR 关闭时已解析文档保持 pending",
    [1, 2].every((page) => latePending.get(page)?.status === "pending"),
    { pages: [latePending.get(1), latePending.get(2)] },
  );

  await setAutoOcr(true);
  await cdp.send("Page.reload");
  await sleep(3500);
  await waitFor(() => cdp.eval(`!!document.querySelector('.doc-sidebar')`), 20000, "侧栏加载");
  await openDocumentByName(lateName);
  await waitFor(async () => {
    const rows = await statusMap(lateDoc.documentId, lateDoc.versionId);
    return [1, 2].every((page) => {
      const row = rows.get(page);
      return (
        row !== undefined &&
        (row.status === "completed" || row.status === "low_confidence") &&
        row.blockCount > 0
      );
    });
  }, 120000, "解析完成后自动 OCR 完成");
  const lateRows = await statusMap(lateDoc.documentId, lateDoc.versionId);
  record(
    "解析完成后开启自动 OCR 可补识别",
    [1, 2].every((page) => lateRows.get(page)?.blockCount > 0),
    { pages: [lateRows.get(1), lateRows.get(2)] },
  );

  const failed = RESULTS.filter((entry) => !entry.pass);
  console.log(`\n${RESULTS.length - failed.length}/${RESULTS.length} checks passed`);
  if (failed.length > 0) {
    throw new Error(`失败 ${failed.length} 项`);
  }
}

try {
  await main();
} catch (error) {
  record("e2e 执行", false, error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
} finally {
  try {
    for (const doc of createdDocuments) {
      await cdp?.invoke("document_delete", { documentId: doc.documentId }).catch(() => undefined);
    }
  } catch {
    // ignore cleanup errors
  }
  await killApp();
  await sleep(500);
  try {
    rmSync(TEMP_DIR, { recursive: true, force: true });
    rmSync(DATA_DIR, { recursive: true, force: true });
  } catch {
    // ignore cleanup errors
  }
  const failed = RESULTS.filter((entry) => !entry.pass);
  if (failed.length > 0) process.exitCode = 1;
}
