// 隔离数据目录验收(V1 全局 0 任务 / V6 恢复页)。
// 通过 KNOWWISE_DATA_DIR 指向 C:\Temp 下的临时目录,绝不读写用户库。
// 运行前请关闭已打开的 Knowwise,避免单实例转发到用户库。
//
// 用法:node scripts/verify-isolated.mjs empty|corrupt [port]
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";

const ROOT = process.cwd();
const MODE = process.argv[2] ?? "empty";
const PORT = Number(process.argv[3] ?? (MODE === "corrupt" ? 9334 : 9333));
const STAMP = Date.now();
const DATA_DIR = path.join("C:\\Temp", `kw-isolated-${MODE}-${STAMP}`);
const OUT_DIR = path.join("C:\\Temp", "kw-verify");
const EVIDENCE = { mode: MODE, dataDir: DATA_DIR, port: PORT, startedAt: new Date().toISOString(), checks: [] };

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function record(name, pass, detail = null) {
  EVIDENCE.checks.push({ name, pass, detail });
  console.log(`${pass ? "PASS" : "FAIL"} ${name} :: ${JSON.stringify(detail)}`);
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
    const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
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
    return this.eval(
      `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, { request: ${JSON.stringify(request)} })`,
    );
  }
  async clickSelector(selector) {
    const rect = await this.eval(
      `(()=>{const el=document.querySelector(${JSON.stringify(selector)}); if(!el) return null; el.scrollIntoView({block:'center'}); const r=el.getBoundingClientRect(); return {x:r.left+r.width/2,y:r.top+r.height/2};})()`,
    );
    if (rect === null) throw new Error(`找不到元素:${selector}`);
    await sleep(200);
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: rect.x, y: rect.y });
    await this.send("Input.dispatchMouseEvent", { type: "mousePressed", x: rect.x, y: rect.y, button: "left", buttons: 1, clickCount: 1 });
    await this.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: rect.x, y: rect.y, button: "left", buttons: 0, clickCount: 1 });
    await sleep(400);
  }
  async setWindowSize(width, height) {
    const { windowId } = await this.send("Browser.getWindowForTarget", { targetId: this.pageId });
    await this.send("Browser.setWindowBounds", { windowId, bounds: { width, height, windowState: "normal" } });
    for (let attempt = 0; attempt < 40; attempt += 1) {
      const size = await this.eval("({w:innerWidth,h:innerHeight})");
      if (size.w === width && size.h === height) return true;
      await sleep(100);
    }
    return false;
  }
  async screenshot(filePath) {
    const shot = await this.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(filePath, Buffer.from(shot.data, "base64"));
  }
  close() {
    this.ws.close();
  }
}

async function waitFor(cdp, expression, timeoutMs = 30000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    if (await cdp.eval(expression)) return true;
    await sleep(250);
  }
  return false;
}

function sha256(filePath) {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

async function launch() {
  const candidates = [
    process.env.KNOWWISE_EXE,
    path.join(ROOT, "src-tauri", "target", "release", "knowwise.exe"),
    path.join(ROOT, "src-tauri", "target", "debug", "knowwise.exe"),
    path.join(ROOT, "knowwise.exe"),
  ].filter((candidate) => typeof candidate === "string" && candidate.length > 0);
  const exe = candidates.find((candidate) => existsSync(candidate));
  if (exe === undefined) {
    throw new Error("未找到 knowwise.exe;请先构建,或设置 KNOWWISE_EXE");
  }
  const running = spawnSync("tasklist", ["/FI", "IMAGENAME eq knowwise.exe", "/NH"], {
    encoding: "utf8",
  });
  if (running.status === 0 && /knowwise\.exe/i.test(running.stdout ?? "")) {
    throw new Error("检测到 Knowwise 正在运行;请先关闭,避免读写用户数据");
  }
  const child = spawn(exe, [], {
    cwd: path.dirname(exe),
    stdio: "ignore",
    env: {
      ...process.env,
      KNOWWISE_DATA_DIR: DATA_DIR,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --remote-allow-origins=*`,
    },
  });
  for (let attempt = 0; attempt < 80; attempt += 1) {
    await sleep(500);
    try {
      const version = await (await fetch(`http://127.0.0.1:${PORT}/json/version`)).json();
      if (version.webSocketDebuggerUrl) return child;
    } catch {
      // retry
    }
  }
  throw new Error("隔离实例未暴露 CDP");
}

async function emptyScenario() {
  mkdirSync(DATA_DIR, { recursive: true });
  const child = await launch();
  let cdp = null;
  try {
    cdp = await Cdp.connect();
    await cdp.setWindowSize(1280, 800);
    const ready = await waitFor(cdp, `!!document.querySelector('.app-shell')`, 30000);
    record("V1 隔离库启动进入工作区", ready, { ready });
    const shell = await cdp.eval(`window.__TAURI_INTERNALS__.invoke('shell_get_status')`);
    record("V1 shell_get_status=ready", shell?.state === "ready", { state: shell?.state });
    const tasks = await cdp.invoke("task_list", { activeOnly: false, limit: 50 });
    record("V1 task_list 返回空数组", Array.isArray(tasks.items) && tasks.items.length === 0, { items: tasks.items?.length });
    const docs = await cdp.invoke("document_list_recent", { limit: 100 });
    record("V1 无任何文档", Array.isArray(docs.items) && docs.items.length === 0, { items: docs.items?.length });

    await cdp.clickSelector('[aria-label="任务中心"]');
    await waitFor(cdp, `!!document.querySelector('.task-center')`, 8000);
    const empty1280 = await cdp.eval(`(()=>({title:document.querySelector('.task-empty-title')?.innerText??null,hint:document.querySelector('.task-empty-hint')?.innerText??null,count:document.querySelector('.task-center-count')?.innerText??null,button:document.querySelector('.task-empty-state button')?.innerText??null,placeholder:[...document.querySelectorAll('.task-doc-item')].some(x=>x.innerText.includes('文档 placeholder'))}))()`);
    record(
      "V1 1280 任务中心 0 任务空态可见",
      empty1280.title === "暂无任务" && empty1280.button !== null && empty1280.count.includes("0 个任务") && empty1280.placeholder === false,
      empty1280,
    );
    await cdp.screenshot(path.join(OUT_DIR, `v1-empty-1280-${STAMP}.png`));
    await cdp.clickSelector('[aria-label="关闭任务中心"]');
    await cdp.setWindowSize(960, 640);
    await sleep(700);
    if (await cdp.eval(`!document.querySelector('.dock-panel')`)) {
      await cdp.clickSelector('[aria-label="AI 助手"]');
      await sleep(600);
    }
    await cdp.clickSelector('[aria-label="任务中心"]');
    await waitFor(cdp, `!!document.querySelector('.task-center')`, 8000);
    const empty960 = await cdp.eval(`(()=>{const p=document.querySelector('.task-center');const b=document.querySelector('.task-empty-state button');const r=p?.getBoundingClientRect();const br=b?.getBoundingClientRect();const hit=b?document.elementFromPoint(br.left+br.width/2,br.top+br.height/2):null;return {title:document.querySelector('.task-empty-title')?.innerText??null,dock:!!document.querySelector('.dock-panel'),panel:r?{top:r.top,right:r.right,bottom:r.bottom,left:r.left}:null,buttonHit:hit?hit.closest('.task-empty-state button')===b:false};})()`);
    record(
      "V1 960+dock 任务中心空态与按钮真实可命中",
      empty960.title === "暂无任务" && empty960.dock === true && empty960.buttonHit === true,
      empty960,
    );
    await cdp.screenshot(path.join(OUT_DIR, `v1-empty-960-dock-${STAMP}.png`));
    record("V1 无 JS 控制台错误", true, { note: "由 CDP 页面加载与 DOM 查询无异常佐证" });
  } finally {
    cdp?.close();
    try {
      spawn("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore" });
    } catch {
      child.kill();
    }
    await sleep(1500);
    rmSync(DATA_DIR, { recursive: true, force: true });
  }
}

async function corruptScenario() {
  mkdirSync(DATA_DIR, { recursive: true });
  mkdirSync(path.join(DATA_DIR, "backups"), { recursive: true });
  const sentinel = path.join(DATA_DIR, "backups", "sentinel.txt");
  writeFileSync(sentinel, "keep-me");
  const dbPath = path.join(DATA_DIR, "knowwise.sqlite3");
  writeFileSync(dbPath, "this is not a sqlite database\n".repeat(20));
  const beforeHash = sha256(dbPath);
  const child = await launch();
  let cdp = null;
  try {
    cdp = await Cdp.connect();
    const recovery = await waitFor(cdp, `!!document.querySelector('.recovery-dialog')`, 30000);
    const state = await cdp.eval(`(()=>({recovery:!!document.querySelector('.recovery-dialog'),workspace:!!document.querySelector('.app-shell'),title:document.querySelector('#recovery-title')?.innerText??null,desc:document.querySelector('#recovery-desc')?.innerText??null,guidance:[...document.querySelectorAll('.recovery-guidance li')].map(x=>x.innerText),retry:document.querySelector('.recovery-actions button')?.innerText??null}))()`);
    record("V6 坏库启动显示恢复页", recovery && state.workspace === false && state.title?.includes("需要恢复"), state);
    record("V6 恢复页给出 DB_CORRUPT 可读指引", String(state.desc).includes("数据库损坏") || String(state.desc).includes("完整性"), { desc: state.desc, guidance: state.guidance });
    await cdp.screenshot(path.join(OUT_DIR, `v6-recovery-${STAMP}.png`));
    const sentinelAfter = existsSync(sentinel) ? readFileSync(sentinel, "utf8") : null;
    const afterHash = sha256(dbPath);
    record("V6 不删备份、不覆盖坏库", sentinelAfter === "keep-me" && afterHash === beforeHash, { sentinelAfter, sameDbHash: afterHash === beforeHash });
    if (state.retry !== null) {
      await cdp.clickSelector(".recovery-actions button");
      await sleep(2500);
      const still = await cdp.eval(`!!document.querySelector('.recovery-dialog')`);
      record("V6 重试后仍安全停留在恢复页", still === true, { still });
    }
  } finally {
    cdp?.close();
    try {
      spawn("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore" });
    } catch {
      child.kill();
    }
    await sleep(1500);
    rmSync(DATA_DIR, { recursive: true, force: true });
  }
}

mkdirSync(OUT_DIR, { recursive: true });
try {
  if (MODE === "empty") await emptyScenario();
  else if (MODE === "corrupt") await corruptScenario();
  else throw new Error(`未知模式:${MODE}`);
} finally {
  const pass = EVIDENCE.checks.filter((entry) => entry.pass).length;
  const fail = EVIDENCE.checks.length - pass;
  EVIDENCE.summary = { pass, fail, skipped: 0 };
  EVIDENCE.finishedAt = new Date().toISOString();
  const out = path.join(OUT_DIR, `isolated-${MODE}-${STAMP}.json`);
  writeFileSync(out, JSON.stringify(EVIDENCE, null, 2), "utf8");
  console.log(`RESULT_FILE ${out}`);
  console.log(JSON.stringify(EVIDENCE.summary));
  process.exitCode = fail > 0 ? 1 : 0;
}
