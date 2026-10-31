// Knowwise e2e-smoke: drives the real desktop app over CDP and covers the
// reader, annotations, task center, artifacts and knowledge graph. It only
// uses synthetic documents under C:\Temp and an isolated KNOWWISE_DATA_DIR.
//
// Usage: node scripts/e2e-smoke.mjs
// Prerequisites:
//   - no other Knowwise instance is running;
//   - a desktop build exists (src-tauri/target/release/knowwise.exe or
//     src-tauri/target/debug/knowwise.exe), or set KNOWWISE_EXE.
// The script launches the app with --remote-debugging-port=9222 and deletes
// its temporary documents and data directory when finished.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";

const ROOT = process.cwd();
const STAMP = Date.now();
const TEMP_DIR = path.join("C:\\Temp", `knowwise-e2e-${STAMP}`);
const DATA_DIR = path.join("C:\\Temp", `knowwise-e2e-data-${STAMP}`);
let APP_PID = null;
const RESULTS = [];
let cdpr = null;

function record(name, pass, detail = null, skipped = false) {
  RESULTS.push({ name, pass, skipped, detail });
  const status = skipped ? "SKIP" : pass ? "PASS" : "FAIL";
  console.log(`${status} ${name} :: ${JSON.stringify(detail)}`);
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function ensureApp() {
  // Never attach to an unknown instance: it could be the user's real library.
  try {
    const version = await (await fetch("http://127.0.0.1:9222/json/version")).json();
    if (version.webSocketDebuggerUrl) {
      throw new Error("9222 端口已有应用;请先关闭 Knowwise 再运行 e2e");
    }
  } catch (error) {
    if (error instanceof Error && error.message.includes("9222")) throw error;
    // connection refused: no existing CDP instance, continue
  }
  const running = spawnSync("tasklist", ["/FI", "IMAGENAME eq knowwise.exe", "/NH"], {
    encoding: "utf8",
  });
  if (running.status === 0 && /knowwise\.exe/i.test(running.stdout ?? "")) {
    throw new Error("检测到 Knowwise 正在运行;请先关闭,避免读写用户数据");
  }
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
  mkdirSync(DATA_DIR, { recursive: true });
  const child = spawn(exe, [], {
    cwd: path.dirname(exe),
    detached: true,
    stdio: "ignore",
    env: {
      ...process.env,
      KNOWWISE_DATA_DIR: DATA_DIR,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:
        "--remote-debugging-port=9222 --remote-allow-origins=*",
    },
  });
  child.unref();
  APP_PID = child.pid ?? null;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    await sleep(500);
    try {
      const version = await (await fetch("http://127.0.0.1:9222/json/version")).json();
      if (version.webSocketDebuggerUrl) return true;
    } catch {
      // retry
    }
  }
  throw new Error("应用未在 9222 端口暴露 CDP");
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
  async invokeFlat(command, args = {}) {
    return this.eval(
      `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(args)})`,
    );
  }
  async clickSelector(selector) {
    const rect = await this.eval(
      `(()=>{const el=document.querySelector(${JSON.stringify(selector)}); if(!el) return null; const r=el.getBoundingClientRect(); return {x:r.left+r.width/2,y:r.top+r.height/2};})()`,
    );
    if (rect === null) throw new Error(`找不到元素:${selector}`);
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: rect.x, y: rect.y });
    await this.send("Input.dispatchMouseEvent", { type: "mousePressed", x: rect.x, y: rect.y, button: "left", buttons: 1, clickCount: 1 });
    await this.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: rect.x, y: rect.y, button: "left", buttons: 0, clickCount: 1 });
    await sleep(150);
  }
  /** 工具栏按钮用 DOM click(真实鼠标在窄布局/滚动时会点偏);绘制与删除仍用真实鼠标。 */
  async clickLabel(label) {
    const ok = await this.eval(
      `(()=>{const el=document.querySelector('[aria-label=${JSON.stringify(label)}]'); if(!el) return false; el.click(); return true;})()`,
    );
    if (!ok) throw new Error(`找不到 aria-label:${label}`);
    await sleep(200);
  }
  async drag(points, steps = 12) {
    const [start, ...rest] = points;
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: start.x, y: start.y });
    await this.send("Input.dispatchMouseEvent", { type: "mousePressed", x: start.x, y: start.y, button: "left", buttons: 1, clickCount: 1 });
    let previous = start;
    for (const point of rest) {
      for (let i = 1; i <= steps; i += 1) {
        const x = previous.x + ((point.x - previous.x) * i) / steps;
        const y = previous.y + ((point.y - previous.y) * i) / steps;
        await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y, button: "left", buttons: 1 });
      }
      previous = point;
    }
    await this.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: previous.x, y: previous.y, button: "left", buttons: 0, clickCount: 1 });
    await sleep(350);
  }
  async key(key, code, modifiers = 0, keyCode = 0) {
    await this.send("Input.dispatchKeyEvent", { type: "keyDown", key, code, modifiers, windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode });
    await this.send("Input.dispatchKeyEvent", { type: "keyUp", key, code, modifiers, windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode });
    await sleep(550);
  }
  /** 真实鼠标点击:先滚到可见处,再按元素中心派发按下/抬起。 */
  async clickElement(selector) {
    const rect = await this.eval(`(()=>{const el=document.querySelector(${JSON.stringify(selector)}); if(!el) return null; el.scrollIntoView({block:'center',inline:'nearest'}); const r=el.getBoundingClientRect(); return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
    if (rect === null) throw new Error(`找不到元素:${selector}`);
    await sleep(250);
    const fresh = await this.eval(`(()=>{const el=document.querySelector(${JSON.stringify(selector)}); if(!el) return null; const r=el.getBoundingClientRect(); return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
    if (fresh === null) throw new Error(`元素消失:${selector}`);
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: fresh.x, y: fresh.y });
    await this.send("Input.dispatchMouseEvent", { type: "mousePressed", x: fresh.x, y: fresh.y, button: "left", buttons: 1, clickCount: 1 });
    await this.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: fresh.x, y: fresh.y, button: "left", buttons: 0, clickCount: 1 });
    await sleep(250);
  }
  /** elementFromPoint 命中链:确认坐标下真正接收点击的元素。 */
  async hitInfo(selector) {
    return this.eval(`(()=>{
      const el=document.querySelector(${JSON.stringify(selector)});
      if(!el) return null;
      const r=el.getBoundingClientRect();
      const x=r.left+r.width/2, y=r.top+r.height/2;
      const hit=document.elementFromPoint(x,y);
      return {
        x:+x.toFixed(1), y:+y.toFixed(1),
        tag:hit?.tagName??null,
        cls:typeof hit?.className==="string"?hit.className:null,
        aria:hit?.getAttribute?.("aria-label")??null,
        inside: hit!==null && !!hit.closest(${JSON.stringify(selector)}),
        inTopBar: hit!==null && !!hit.closest(".top-bar"),
        inDock: hit!==null && !!hit.closest(".dock-panel"),
        inTaskCenter: hit!==null && !!hit.closest(".task-center"),
      };
    })()`);
  }

  /** 真实鼠标点击一个坐标(mouseMoved → pressed → released)。 */
  async clickPoint(point) {
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: point.x, y: point.y });
    await this.send("Input.dispatchMouseEvent", { type: "mousePressed", x: point.x, y: point.y, button: "left", buttons: 1, clickCount: 1 });
    await this.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: point.x, y: point.y, button: "left", buttons: 0, clickCount: 1 });
    await sleep(250);
  }
  /** 滚动到可见处并校验 elementFromPoint 命中自身后真实点击。 */
  async clickChecked(selector) {
    const p = await this.eval(`(()=>{const el=document.querySelector(${JSON.stringify(selector)});if(!el)return null;el.scrollIntoView({block:'center'});const r=el.getBoundingClientRect();const x=r.left+r.width/2,y=r.top+r.height/2;const hit=document.elementFromPoint(x,y);return {x,y,inside:hit===el||el.contains(hit),tag:hit?.tagName??null};})()`);
    if (p === null) throw new Error(`找不到元素:${selector}`);
    if (p.inside) await this.clickPoint(p);
    return p;
  }
  /** 真实滚轮:鼠标悬停在 (x,y),按步派发 mouseWheel。 */
  async wheel(x, y, deltaY, steps = 4) {
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
    await sleep(80);
    for (let i = 0; i < steps; i += 1) {
      await this.send("Input.dispatchMouseEvent", {
        type: "mouseWheel",
        x,
        y,
        deltaX: 0,
        deltaY: Math.round(deltaY / steps),
        modifiers: 0,
      });
      await sleep(80);
    }
    await sleep(350);
  }
  /** 调整真实窗口大小(CDP 的 bounds 在此 WebView2 中即客户区尺寸)。 */
  async setWindowSize(width, height) {
    const { windowId } = await this.send("Browser.getWindowForTarget", { targetId: this.pageId });
    await this.send("Browser.setWindowBounds", {
      windowId,
      bounds: { width, height, windowState: "normal" },
    });
    for (let attempt = 0; attempt < 30; attempt += 1) {
      const size = await this.eval("({w:innerWidth,h:innerHeight})");
      if (size.w === width && size.h === height) return true;
      await sleep(100);
    }
    return false;
  }
  close() {
    this.ws.close();
  }
}

async function ensureSidebarOpen(cdp) {
  const has = await cdp.eval(`document.querySelector('.doc-sidebar')!==null`);
  if (!has) {
    await cdp.eval(`(()=>{const b=[...document.querySelectorAll('button')].find(x=>x.getAttribute('aria-label')==='展开文档库'); if(b)b.click(); return true;})()`);
    await sleep(400);
  }
}

async function openDocument(cdp, name, format) {
  await ensureSidebarOpen(cdp);
  const found = await cdp.eval(`(()=>{
    const items=[...document.querySelectorAll('.doc-sidebar button.doc-item-main')].filter(b=>b.textContent?.trim().startsWith(${JSON.stringify(name)}));
    const target=items.find(b=>b.closest('.doc-group')?.textContent?.trim().toLowerCase().startsWith(${JSON.stringify(format.toLowerCase())})) ?? items.find(b=>!b.textContent?.includes('-pdf')) ?? items[0];
    if(!target) return false;
    target.click(); return true;
  })()`);
  if (!found) throw new Error(`侧栏找不到文档:${name}`);
  await sleep(2800);
}

async function waitFor(cdp, expression, timeoutMs = 8000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    if (await cdp.eval(expression)) return true;
    await sleep(200);
  }
  return false;
}

function makeMarkdown(name) {
  let text = `# ${name}\n\n这是 Knowwise e2e 合成文档,仅用于自动验收,结束后删除。\n\n`;
  for (let i = 1; i <= 10; i += 1) {
    text += `## 第 ${i} 节\n\n第 ${i} 节正文。快速狐狸跳过懒狗。数字 ${i * 13} 仅用于占位;笔迹、橡皮、套索与几何图形都应可撤销、可持久化。\n\n`;
  }
  return text;
}

function makePdf(filePath, title) {
  const objects = [];
  objects[1] = "<< /Type /Catalog /Pages 2 0 R >>";
  objects[2] = "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>";
  const streams = {};
  for (let pageIndex = 0; pageIndex < 2; pageIndex += 1) {
    const pageNo = 3 + pageIndex * 2;
    const contentNo = pageNo + 1;
    streams[contentNo] = [
      `BT /F1 24 Tf 72 700 Td (${title} Page ${pageIndex + 1}) Tj ET`,
      "BT /F1 12 Tf 72 660 Td (Synthetic PDF for Knowwise e2e. No user data.) Tj ET",
      "BT /F1 12 Tf 72 630 Td (Erase, undo, lasso and shapes are verified here.) Tj ET",
      "BT /F1 12 Tf 72 600 Td (Second line for selection and annotation.) Tj ET",
    ].join("\n");
    objects[pageNo] = `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents ${contentNo} 0 R /Resources << /Font << /F1 7 0 R >> >> >>`;
  }
  objects[7] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";
  let out = "%PDF-1.4\n";
  const offsets = [0];
  for (let i = 1; i <= 7; i += 1) {
    offsets[i] = out.length;
    if (streams[i] !== undefined) {
      out += `${i} 0 obj\n<< /Length ${Buffer.byteLength(streams[i])} >>\nstream\n${streams[i]}\nendstream\nendobj\n`;
    } else {
      out += `${i} 0 obj\n${objects[i]}\nendobj\n`;
    }
  }
  const xref = out.length;
  out += "xref\n0 8\n0000000000 65535 f \n";
  for (let i = 1; i <= 7; i += 1) out += `${String(offsets[i]).padStart(10, "0")} 00000 n \n`;
  out += `trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  writeFileSync(filePath, out, "latin1");
}

/** 多页合成 PDF:用于制造本地解析任务(不依赖模型,取消可稳定复现)。 */
/** 无文本层、只有绘图指令的 PDF:解析会走逐页 OCR,持续数十秒,适合取消测试。 */
function makeBlankPdf(filePath, pages) {
  const pagesId = pages * 2 + 2;
  const font = pagesId + 1;
  const objects = [];
  const kids = [];
  for (let page = 1; page <= pages; page += 1) {
    const pageNo = 2 + (page - 1) * 2;
    const contentNo = pageNo + 1;
    objects[pageNo] = `<< /Type /Page /Parent ${pagesId} 0 R /MediaBox [0 0 612 792] /Contents ${contentNo} 0 R /Resources << /Font << /F1 ${font} 0 R >> >> >>`;
    objects[contentNo] = { stream: "q 0 0 1 RG 1 w 10 10 m 100 100 l S Q" };
    kids.push(`${pageNo} 0 R`);
  }
  objects[1] = `<< /Type /Catalog /Pages ${pagesId} 0 R >>`;
  objects[pagesId] = `<< /Type /Pages /Kids [${kids.join(" ")}] /Count ${pages} >>`;
  objects[font] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";
  let out = "%PDF-1.4\n";
  const offsets = [0];
  for (let i = 1; i <= font; i += 1) {
    offsets[i] = out.length;
    if (objects[i]?.stream !== undefined) {
      out += `${i} 0 obj\n<< /Length ${Buffer.byteLength(objects[i].stream)} >>\nstream\n${objects[i].stream}\nendstream\nendobj\n`;
    } else if (objects[i] !== undefined) {
      out += `${i} 0 obj\n${objects[i]}\nendobj\n`;
    }
  }
  const xref = out.length;
  out += `xref\n0 ${font + 1}\n0000000000 65535 f \n`;
  for (let i = 1; i <= font; i += 1) out += `${String(offsets[i]).padStart(10, "0")} 00000 n \n`;
  out += `trailer\n<< /Size ${font + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  writeFileSync(filePath, out, "latin1");
}

function makeLargePdf(filePath, title, pages) {
  const pagesId = pages * 2 + 2;
  const font = pagesId + 1;
  const objects = [];
  const kids = [];
  for (let page = 1; page <= pages; page += 1) {
    const pageNo = 2 + (page - 1) * 2;
    const contentNo = pageNo + 1;
    const content = [
      `BT /F1 20 Tf 72 720 Td (${title} Page ${page}) Tj ET`,
      "BT /F1 11 Tf 72 690 Td (Synthetic large PDF for task cancellation e2e. No user data.) Tj ET",
      "BT /F1 11 Tf 72 670 Td (Parsing is local and cancelable through the task center.) Tj ET",
    ].join("\n");
    objects[pageNo] = `<< /Type /Page /Parent ${pagesId} 0 R /MediaBox [0 0 612 792] /Contents ${contentNo} 0 R /Resources << /Font << /F1 ${font} 0 R >> >> >>`;
    objects[contentNo] = { stream: content };
    kids.push(`${pageNo} 0 R`);
  }
  objects[1] = `<< /Type /Catalog /Pages ${pagesId} 0 R >>`;
  objects[pagesId] = `<< /Type /Pages /Kids [${kids.join(" ")}] /Count ${pages} >>`;
  objects[font] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";
  let out = "%PDF-1.4\n";
  const offsets = [0];
  for (let i = 1; i <= font; i += 1) {
    offsets[i] = out.length;
    if (objects[i]?.stream !== undefined) {
      out += `${i} 0 obj\n<< /Length ${Buffer.byteLength(objects[i].stream)} >>\nstream\n${objects[i].stream}\nendstream\nendobj\n`;
    } else if (objects[i] !== undefined) {
      out += `${i} 0 obj\n${objects[i]}\nendobj\n`;
    }
  }
  const xref = out.length;
  out += `xref\n0 ${font + 1}\n0000000000 65535 f \n`;
  for (let i = 1; i <= font; i += 1) out += `${String(offsets[i]).padStart(10, "0")} 00000 n \n`;
  out += `trailer\n<< /Size ${font + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  writeFileSync(filePath, out, "latin1");
}

async function annotations(cdp, documentId) {
  return cdp.invoke("annotation_list", { documentId });
}

/** 顶部布局几何:标签栏/阅读工具栏/正文滚动区/主区/右栏。 */
async function measureLayout(cdp) {
  return cdp.eval(`(()=>{
    const rect=(s)=>{const e=document.querySelector(s);if(!e)return null;const b=e.getBoundingClientRect();return {top:+b.top.toFixed(2),bottom:+b.bottom.toFixed(2),left:+b.left.toFixed(2),right:+b.right.toFixed(2),height:+b.height.toFixed(2)};};
    const tab=rect('.tab-strip');
    const toolbar=rect('.reader-toolbar');
    const body=rect('.reader-body-wrap');
    const pdf=rect('.pdf-scroll');
    const main=rect('.main-region');
    const dockEl=document.querySelector('.dock-panel');
    const dock=rect('.dock-panel');
    const dockHead=rect('.dock-head');
    const dockPosition=dockEl?getComputedStyle(dockEl).position:null;
    const scroll=body??pdf;
    const bannerEl=document.querySelector('.reader-region [role=status]');
    return {
      win:{w:innerWidth,h:innerHeight},
      tab,toolbar,body,pdf,main,dock,dockHead,dockPosition,
      banner:bannerEl?bannerEl.innerText:null,
      readerLabel:document.querySelector('.reader-region')?.getAttribute('aria-label')??null,
      gaps:{
        tabToolbar: tab&&toolbar?+(toolbar.top-tab.bottom).toFixed(2):null,
        toolbarBody: toolbar&&scroll?+(scroll.top-toolbar.bottom).toFixed(2):null,
        dockMain: dock&&main?+(dock.top-main.top).toFixed(2):null,
        dockHeadToolbar: dockHead&&toolbar?+(dockHead.top-toolbar.top).toFixed(2):null,
      },
    };
  })()`);
}

/** 任务中心状态:当前文档/会话、三个列表、滚动容器尺寸。 */
async function readTaskCenter(cdp) {
  return cdp.eval(`(()=>{
    const sc=(s)=>{const e=document.querySelector(s);return e?{scrollTop:+e.scrollTop.toFixed(1),scrollHeight:e.scrollHeight,clientHeight:e.clientHeight,overflow:getComputedStyle(e).overflowY}:null;};
    const panel=document.querySelector('.task-center');
    const pr=panel?panel.getBoundingClientRect():null;
    return {
      open:panel!==null,
      panel:pr?{top:+pr.top.toFixed(1),bottom:+pr.bottom.toFixed(1),left:+pr.left.toFixed(1),right:+pr.right.toFixed(1),width:+pr.width.toFixed(1),height:+pr.height.toFixed(1)}:null,
      current:document.querySelector('.task-current-bar')?.innerText??null,
      activeDoc:document.querySelector('.task-doc-item.active')?.innerText??null,
      activeConv:document.querySelector('.task-conv-item.active')?.innerText??null,
      docs:[...document.querySelectorAll('.task-doc-item')].map((x)=>x.innerText),
      convs:[...document.querySelectorAll('.task-conv-item')].map((x)=>x.innerText),
      rows:document.querySelectorAll('.task-center-list .task-row').length,
      empty:document.querySelector('.task-empty-state')?.innerText??null,
      bulk:document.querySelector('.task-bulk-bar')?.innerText??null,
      scrolls:{doc:sc('.task-doc-list'),conv:sc('.task-conv-list'),tasks:sc('.task-center-list'),body:sc('.task-center-body')},
    };
  })()`);
}

async function resetTaskScrolls(cdp) {
  await cdp.eval(`(()=>{for(const s of ['.task-doc-list','.task-conv-list','.task-center-list','.task-center-body']){const e=document.querySelector(s);if(e)e.scrollTop=0;}return true;})()`);
}

/** 等待阅读区安静(无解析中状态横幅);返回是否安静。 */
async function waitReaderQuiet(cdp, timeoutMs = 20000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const state = await cdp.eval(`({banner:document.querySelector('.reader-region [role=status]')?.innerText??null, toolbar:!!document.querySelector('.reader-toolbar')})`);
    if (state.toolbar && state.banner === null) return true;
    await sleep(400);
  }
  return false;
}

function validTopLayout(metrics, expectDockAligned) {
  const gaps = metrics.gaps;
  const gap1 = gaps.tabToolbar !== null && gaps.tabToolbar >= -0.5 && gaps.tabToolbar <= 6;
  const gap2 = gaps.toolbarBody !== null && gaps.toolbarBody >= 0 && gaps.toolbarBody <= 8;
  const quiet = metrics.banner === null;
  const dockOk = expectDockAligned
    ? gaps.dockMain !== null && Math.abs(gaps.dockMain) <= 1 && gaps.dockHeadToolbar !== null && Math.abs(gaps.dockHeadToolbar) <= 1
    : metrics.dockPosition === "fixed" && metrics.dock !== null && Math.abs(metrics.dock.top) <= 1;
  return { ok: gap1 && gap2 && quiet && dockOk, gap1, gap2, quiet, dockOk };
}


/** 生成 N 页无文本层扫描 PDF(应用内 canvas 渲染文字 → JPEG 封装 PDF)。 */
async function makeScanPdf(cdp, filePath, pageCount) {
  const images = [];
  for (let page = 1; page <= pageCount; page += 1) {
    const dataUrl = await cdp.eval(`(()=>{
      const c=document.createElement('canvas');c.width=1200;c.height=1600;
      const ctx=c.getContext('2d');
      ctx.fillStyle='#fdfdfb';ctx.fillRect(0,0,c.width,c.height);
      ctx.fillStyle='#111';ctx.font='64px Georgia, "Times New Roman", serif';
      ctx.fillText('E2E scan page ${page}',90,280);
      ctx.font='54px Georgia, serif';
      ctx.fillText('Synthetic OCR progress page ${page}.',90,900);
      return c.toDataURL('image/jpeg',0.9);
    })()`);
    images.push(Buffer.from(dataUrl.split(",")[1], "base64"));
  }
  const parts = [];
  let length = 0;
  const offsets = [0];
  const push = (buf) => { const body = Buffer.isBuffer(buf) ? buf : Buffer.from(buf, "latin1"); parts.push(body); length += body.length; };
  const startObj = (n) => { offsets[n] = length; push(`${n} 0 obj\n`); };
  const endObj = () => push("\nendobj\n");
  push("%PDF-1.4\n");
  startObj(1); push("<< /Type /Catalog /Pages 2 0 R >>"); endObj();
  const kids = images.map((_, index) => `${3 + index * 3} 0 R`).join(" ");
  startObj(2); push(`<< /Type /Pages /Kids [${kids}] /Count ${images.length} >>`); endObj();
  images.forEach((jpeg, index) => {
    const pageNo = 3 + index * 3;
    const contentNo = pageNo + 1;
    const imageNo = pageNo + 2;
    startObj(pageNo); push(`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents ${contentNo} 0 R /Resources << /XObject << /Im0 ${imageNo} 0 R >> >> >>`); endObj();
    const content = "q\n612 0 0 792 0 0 cm\n/Im0 Do\nQ\n";
    startObj(contentNo); push(`<< /Length ${Buffer.byteLength(content)} >>\nstream\n${content}endstream`); endObj();
    startObj(imageNo); push(`<< /Type /XObject /Subtype /Image /Width 1200 /Height 1600 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length ${jpeg.length} >>\nstream\n`); push(jpeg); push("\nendstream"); endObj();
  });
  const objCount = 2 + images.length * 3;
  const xref = length;
  push(`xref\n0 ${objCount + 1}\n0000000000 65535 f \n`);
  for (let n = 1; n <= objCount; n += 1) push(`${String(offsets[n]).padStart(10, "0")} 00000 n \n`);
  push(`trailer\n<< /Size ${objCount + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`);
  writeFileSync(filePath, Buffer.concat(parts));
}

async function main() {
  const launched = await ensureApp();
  cdpr = await Cdp.connect();
  const cdp = cdpr;
  // 固定初始窗口,避免上次运行残留尺寸影响真实鼠标坐标。
  await cdp.setWindowSize(1280, 800);
  await sleep(700);
  await cdp.eval(`(()=>{document.querySelector('[aria-label="关闭任务中心"]')?.click(); return true;})()`);
  mkdirSync(TEMP_DIR, { recursive: true });
  const mdName = `kw-e2e-${STAMP}`;
  const pdfName = `kw-e2e-${STAMP}-pdf`;
  const mdPath = path.join(TEMP_DIR, `${mdName}.md`);
  const pdfPath = path.join(TEMP_DIR, `${pdfName}.pdf`);
  writeFileSync(mdPath, makeMarkdown(mdName), "utf8");
  makePdf(pdfPath, "Knowwise E2E");
  let mdDoc = null;
  let pdfDoc = null;
  let scanDoc = null;
  const createdConversations = [];
  const scrollDocs = [];
  const scrollTaskIds = [];
  try {
    mdDoc = await cdp.invoke("document_import", { path: mdPath, idempotencyKey: `e2e-md-${STAMP}` });
    pdfDoc = await cdp.invoke("document_import", { path: pdfPath, idempotencyKey: `e2e-pdf-${STAMP}` });
    record("导入合成 Markdown/PDF", Boolean(mdDoc.documentId && pdfDoc.documentId), {
      md: mdDoc.documentId,
      pdf: pdfDoc.documentId,
    });
    await cdp.send("Page.reload");
    await sleep(4500);
    await openDocument(cdp, mdName, "markdown");
    await cdp.eval(`(()=>{const w=document.querySelector('.reader-body-wrap'); if(w) w.scrollTop=0; return true;})()`);
    await sleep(600);
    const mdId = mdDoc.documentId;
    const list = () => annotations(cdp, mdId);
    const base = await list();
    record("合成文档标注基线", base.length === 0, { count: base.length });

    const para = await cdp.eval(`(()=>{const w=document.querySelector('.reader-body-wrap'); const wrap=w?.getBoundingClientRect(); if(!wrap)return null; const p=[...document.querySelectorAll('.markdown-body p')].find(p=>{const r=p.getBoundingClientRect(); return p.textContent.trim().length>20 && r.top>wrap.top+20 && r.bottom<wrap.bottom-60;}); if(!p)return null; const r=p.getBoundingClientRect(); return {x:r.left+r.width*0.2,y:r.top+r.height/2,w:r.width};})()`);
    if (para === null) throw new Error("Markdown 正文不可用");
    const y = Math.round(para.y);
    const x0 = Math.round(para.x);
    const x1 = Math.round(Math.min(para.x + para.w - 30, x0 + 300));

    await cdp.clickLabel("画笔");
    await cdp.drag([{ x: x0, y }, { x: x0 + 80, y: y - 10 }, { x: x0 + 160, y: y + 8 }, { x: x1, y: y - 4 }], 8);
    await sleep(600);
    let afterDraw = await list();
    const ink = afterDraw.find((annotation) => annotation.noteText.startsWith("ink:v1:"));
    record("笔迹落库", Boolean(ink), { count: afterDraw.length, noteLen: ink?.noteText.length });
    await cdp.key("z", "KeyZ", 2, 90);
    record("Ctrl+Z 撤销新建笔迹", (await list()).length === 0, { count: (await list()).length });
    await cdp.key("Z", "KeyZ", 10, 90);
    const restored = (await list())[0];
    record("Ctrl+Shift+Z 重做笔迹(同 anchor/noteText)", restored?.noteText === ink?.noteText, { same: restored?.noteText === ink?.noteText });

    await cdp.clickLabel("橡皮");
    // 切换工具会改变工具栏换行高度(正文随之移动):等布局稳定后按笔迹 path 的真实包围盒重新定位。
    await sleep(500);
    const erasePoint = await cdp.eval(`(()=>{const p=document.querySelector('svg.rich-ink path, svg.pdf-ink path');if(!p)return null;try{const len=p.getTotalLength();const pt=p.getPointAtLength(len*0.5);const m=p.getScreenCTM();const s=pt.matrixTransform(m);return {x:+s.x.toFixed(1),y:+s.y.toFixed(1)};}catch{return null;}})()`);
    if (erasePoint) {
      await cdp.drag([{ x: Math.round(erasePoint.x - 12), y: Math.round(erasePoint.y) }, { x: Math.round(erasePoint.x + 12), y: Math.round(erasePoint.y) }], 8);
      await sleep(300);
    } else {
      const mid = Math.round((x0 + x1) / 2);
      await cdp.drag([{ x: mid, y: y - 3 }, { x: mid + 10, y: y + 3 }, { x: mid - 10, y: y - 3 }], 5);
    }
    await sleep(700);
    const erased = (await list())[0];
    record("精确橡皮:删除圆内点且不弹确认", erased !== undefined && erased.noteText !== restored?.noteText && (await cdp.eval(`document.querySelector('.kw-confirm-dialog')===null`)), {
      before: restored?.noteText.length,
      after: erased?.noteText.length,
    });
    await cdp.key("z", "KeyZ", 2, 90);
    record("撤销橡皮恢复原笔迹", (await list())[0]?.noteText === ink?.noteText, {});

    await cdp.clickLabel("矩形");
    await cdp.drag([{ x: x0 + 10, y: y + 40 }, { x: x0 + 120, y: y + 100 }], 7);
    await sleep(600);
    record("矩形工具持久化(t=rect)", (await list()).some((annotation) => annotation.noteText.includes('"t":"rect"')), {});
    await cdp.key("z", "KeyZ", 2, 90);

    await cdp.clickLabel("套索");
    // 以笔迹 SVG 的真实屏幕包围盒为套索范围:段落坐标可能因工具栏换行/滚动漂移,
    // 固定坐标会在某些窗口状态下框空(功能本身正常)。
    const inkBox = await cdp.eval(`(()=>{const p=document.querySelector('svg.rich-ink path, svg.pdf-ink path');if(!p)return null;const r=p.getBoundingClientRect();return {l:r.left,t:r.top,r:r.right,b:r.bottom};})()`);
    const lassoPoints = inkBox === null
      ? [
          { x: x0 - 8, y: y - 24 },
          { x: x0 + 190, y: y - 24 },
          { x: x0 + 190, y: y + 24 },
          { x: x0 - 8, y: y + 24 },
          { x: x0 - 8, y: y - 24 },
        ]
      : [
          { x: inkBox.l - 12, y: inkBox.t - 12 },
          { x: inkBox.r + 12, y: inkBox.t - 12 },
          { x: inkBox.r + 12, y: inkBox.b + 12 },
          { x: inkBox.l - 12, y: inkBox.b + 12 },
          { x: inkBox.l - 12, y: inkBox.t - 12 },
        ];
    await cdp.drag(lassoPoints, 10);
    await sleep(500);
    const lasso = await cdp.eval(`(()=>{const el=document.querySelector('.lasso-actions');return el?{text:el.textContent?.trim()}:null})()`);
    record("套索可框选笔迹", lasso !== null, { lasso, inkBox });
    if (lasso !== null) {
      await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.lasso-actions button')].find(x=>x.textContent.trim()==='删除'); b?.click(); return true;})()`);
      await sleep(300);
      const confirm = await cdp.eval(`document.querySelector('.kw-confirm-dialog')?.innerText ?? null`);
      record("套索删除需确认", confirm !== null, { confirm });
      if (confirm !== null) {
        await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.kw-confirm-dialog button')].find(x=>x.textContent.trim()==='删除'); b?.click(); return true;})()`);
        await sleep(700);
      }
      record("套索删除已落库", !(await list()).some((annotation) => annotation.noteText === ink?.noteText), {});
      await cdp.key("z", "KeyZ", 2, 90);
      record("撤销套索删除(按 anchor 重建)", (await list()).some((annotation) => annotation.anchorId === ink?.anchorId && annotation.noteText === ink?.noteText), {});
    }

    // 文本框 + 删除确认
    await cdp.clickLabel("文字注释");
    await sleep(500);
    const boxPoint = await cdp.eval(`(()=>{const p=[...document.querySelectorAll('.markdown-body p')].find(p=>p.textContent.trim().length>20);if(!p)return null;p.scrollIntoView({block:'center'});const r=p.getBoundingClientRect();return {x:Math.round(r.left+120),y:Math.round(r.top+40)};})()`);
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: boxPoint.x, y: boxPoint.y });
    await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: boxPoint.x, y: boxPoint.y, button: "left", buttons: 1, clickCount: 1 });
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: boxPoint.x, y: boxPoint.y, button: "left", buttons: 0, clickCount: 1 });
    await sleep(500);
    const editorOpen = await cdp.eval(`document.querySelector('.textbox-editor textarea')!==null`);
    record("文本框编辑器打开", editorOpen, {});
    if (editorOpen) {
      await cdp.send("Input.insertText", { text: "e2e 文字注释" });
      await sleep(250);
      await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.textbox-editor button')].find(x=>x.textContent.trim()==='保存'); b?.click(); return true;})()`);
      await sleep(1000);
      const boxAnnotation = (await list()).find((annotation) => annotation.noteText.startsWith("textbox:v1:"));
      record("文本框保存为标注", Boolean(boxAnnotation), {});
      if (boxAnnotation !== undefined) {
        await cdp.eval(`(()=>{document.querySelector('.textbox-bubble')?.click(); return true;})()`);
        await sleep(400);
        await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.textbox-editor button')].find(x=>x.textContent.trim()==='删除'); b?.click(); return true;})()`);
        await sleep(300);
        record("文本框删除需确认", (await cdp.eval(`document.querySelector('.kw-confirm-dialog')?.innerText ?? null`)) !== null, {});
        await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.kw-confirm-dialog button')].find(x=>x.textContent.trim()==='删除'); b?.click(); return true;})()`);
        await sleep(700);
      }
    }

    // 双向链接:为笔迹 anchor 建一条笔记,页边标记出现且点击可打开笔记。
    const inkNow = (await list()).find((annotation) => annotation.anchorId === ink?.anchorId) ?? (await list())[0];
    const note = await cdp.invoke("note_save", {
      documentId: mdId,
      anchorId: inkNow.anchorId,
      body: "### 双向链接测试笔记\n\n这条笔记与笔迹锚点关联。\n",
      origin: "user",
      tags: ["e2e"],
      status: "open",
      sourceRefs: [
        { sourceType: "annotation", sourceId: inkNow.id, sourceRole: "user_note", displayName: "笔迹", citationIds: [], snapshotAvailable: true },
      ],
      expectedRevision: 0,
      idempotencyKey: `e2e-note-${STAMP}`,
    });
    // 直接 IPC 建笔记不会经过面板的本地失效;重载后按真实启动路径读取。
    await cdp.send("Page.reload");
    await sleep(4200);
    await openDocument(cdp, mdName, "markdown");
    await cdp.eval(`(()=>{const w=document.querySelector('.reader-body-wrap'); if(w) w.scrollTop=0; return true;})()`);
    // 重载后阅读器与笔记查询都是异步的:先等标记层挂载(层恒渲染,
    // 缺失说明阅读器尚未就绪),再等锚点解析完成,避免把加载中误判为没有标记。
    await waitFor(cdp, `!!document.querySelector('.note-links-layer')`, 10000);
    await waitFor(cdp, `Number(document.querySelector('.note-links-layer')?.dataset.anchorCount||0)>0`, 6000);
    await sleep(400);
    const markerCount = await cdp.eval(`document.querySelectorAll('.note-link-marker').length`);
    const markerDebug = await cdp.eval(`(()=>{const l=document.querySelector('.note-links-layer');return l?{note:l.dataset.noteCount,anchors:l.dataset.anchorCount,markers:l.dataset.markerCount,info:l.dataset.markerInfo}:null})()`);
    const noteView = (await cdp.invoke("note_list", { documentId: mdId })).items.find((item) => item.id === note.id);
    record("页边笔记标记出现", markerCount > 0, { markers: markerCount, markerDebug, locator: noteView?.locator ?? null });
    if (markerCount > 0) {
      await cdp.eval(`(()=>{document.querySelector('.note-link-marker')?.click(); return true;})()`);
      await sleep(900);
      record("点击标记打开并聚焦笔记", (await cdp.eval(`document.querySelector('.notes-item.selected .notes-item-title')?.innerText?.includes('双向链接') ?? false`)) || (await cdp.eval(`!!document.querySelector('.notes-workspace textarea')`)), {});
    }

    // 标注「问 AI」:不重复建笔记,来源 chip 可见。
    await ensureSidebarOpen(cdp);
    await cdp.eval(`(()=>{const dock=[...document.querySelectorAll('.dock-tab')].find(x=>x.textContent?.includes('笔记')); dock?.click(); return true;})()`);
    await sleep(400);
    await cdp.eval(`(()=>{const tab=[...document.querySelectorAll('.notes-view-tab')].find(x=>x.getAttribute('aria-label')==='笔记'); tab?.click(); return true;})()`);
    await sleep(500);
    const notesBefore = (await cdp.invoke("note_list", { documentId: mdId })).items.length;
    await waitFor(cdp, `document.querySelectorAll('.notes-item button[title*="标注转成关联笔记"]').length>0`, 6000);
    const askButtons = await cdp.eval(`document.querySelectorAll('.notes-item button[title*="标注转成关联笔记"]').length`);
    record("标注条目提供「问 AI」", askButtons > 0, { buttons: askButtons });
    if (askButtons > 0) {
      await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.notes-item button')].find(x=>x.title?.includes('标注转成关联笔记')); b?.click(); return true;})()`);
      await sleep(1500);
      const notesAfter = (await cdp.invoke("note_list", { documentId: mdId })).items.length;
      const chipText = await cdp.eval(`document.querySelector('.agent-cited-note')?.innerText ?? document.querySelector('.dock-panel')?.innerText?.includes('非原文事实') ?? false`);
      const draft = await cdp.eval(`document.querySelector('#agent-input')?.value ?? ''`);
      // 同一 anchor 已有笔记时按设计复用(不重复建笔记),否则新建一条。
      record(
        "标注问 AI 生成/复用关联笔记并加入来源",
        notesAfter === notesBefore || notesAfter === notesBefore + 1,
        { before: notesBefore, after: notesAfter },
      );
      record("来源 chip/草稿说明不是原文事实", String(chipText).includes("非原文事实") || draft.includes("不是原文事实"), { chipText, draft: draft.slice(0, 40) });
      await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.notes-item button')].find(x=>x.title?.includes('标注转成关联笔记')); b?.click(); return true;})()`);
      await sleep(1200);
      record("再次问 AI 不重复建笔记", (await cdp.invoke("note_list", { documentId: mdId })).items.length === notesAfter, { count: (await cdp.invoke("note_list", { documentId: mdId })).items.length });
    }

    // 输出速度设置
    await cdp.eval(`(()=>{document.querySelector('[aria-label=设置]')?.click(); return true;})()`);
    await sleep(600);
    await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.settings-tab')].find(x=>x.textContent.includes('应用与偏好')); b?.click(); return true;})()`);
    await sleep(500);
    await cdp.eval(`(()=>{const s=[...document.querySelectorAll('.settings-input select')].find(x=>x.closest('label')?.querySelector('span')?.textContent?.includes('流式输出速度')); if(!s)return false; const setter=Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set; setter.call(s,'fast'); s.dispatchEvent(new Event('change',{bubbles:true})); return true;})()`);
    await sleep(300);
    await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.settings-dialog button')].find(x=>x.textContent.includes('保存应用设置')); b?.click(); return true;})()`);
    await sleep(1300);
    const speedView = await cdp.invoke("settings_get", {});
    record("输出速度设置可保存并持久化", speedView.settings.streamSpeed === "fast" && (await cdp.eval(`localStorage.getItem('knowwise.stream.speed')`)) === "fast", { speed: speedView.settings.streamSpeed });
    await cdp.eval(`(()=>{const s=[...document.querySelectorAll('.settings-input select')].find(x=>x.closest('label')?.querySelector('span')?.textContent?.includes('流式输出速度')); if(!s)return false; const setter=Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set; setter.call(s,'standard'); s.dispatchEvent(new Event('change',{bubbles:true})); return true;})()`);
    await sleep(200);
    await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.settings-dialog button')].find(x=>x.textContent.includes('保存应用设置')); b?.click(); return true;})()`);
    await sleep(900);
    await cdp.eval(`(()=>{document.querySelector('[aria-label=关闭设置]')?.click(); return true;})()`);

    // 任务中心:文档 → 会话 → 任务(三栏主从)
    await cdp.invoke("parse_start", { documentId: mdId, versionId: mdDoc.versionId, idempotencyKey: `e2e-parse-${STAMP}` }).catch(() => undefined);
    await sleep(1500);
    await cdp.eval(`(()=>{document.querySelector('[aria-label=任务中心]')?.click(); return true;})()`);
    await sleep(1200);
    const taskShape = await readTaskCenter(cdp);
    record(
      "任务中心按文档→会话→任务分组",
      taskShape.open &&
        taskShape.docs.length > 0 &&
        taskShape.convs.length > 0 &&
        String(taskShape.current).includes("当前文档") &&
        String(taskShape.current).includes("当前会话"),
      { docs: taskShape.docs.length, convs: taskShape.convs.length, current: taskShape.current, activeConv: taskShape.activeConv, rows: taskShape.rows },
    );
    await cdp.eval(`(()=>{document.querySelector('[aria-label="关闭任务中心"]')?.click(); return true;})()`);

    // 产物逐段审阅
    const summary = await cdp.invoke("artifact_commit_draft", {
      documentId: mdId,
      kind: "summary",
      title: `E2E 摘要 ${STAMP}`,
      content: {
        schemaVersion: "summary.v1",
        sections: [
          {
            id: "s1",
            title: "测试章节",
            segments: [
              { id: "g1", kind: "ai_inference", text: "第一段 AI 推断。", citationIds: [] },
              { id: "g2", kind: "needs_verification", text: "第二段待核实。", citationIds: [] },
              { id: "g3", kind: "user_view", text: "第三段用户观点。", citationIds: [] },
            ],
          },
        ],
        openQuestions: [],
        coverage: { complete: true, regions: [] },
      },
      coverage: { complete: true, regions: [] },
      idempotencyKey: `e2e-summary-${STAMP}`,
    });
    await cdp.eval(`(()=>{window.dispatchEvent(new CustomEvent('knowwise-open-artifacts',{detail:{artifactId:${JSON.stringify(summary.id)},read:true}})); return true;})()`);
    await sleep(2200);
    record("产物阅览打开", await cdp.eval(`!!document.querySelector('.artifact-reader')`), {});
    await cdp.eval(`(()=>{document.querySelector('[aria-label=逐段审阅]')?.click(); return true;})()`);
    await sleep(600);
    const segmentCount = await cdp.eval(`document.querySelectorAll('.artifact-review-segment').length`);
    record("产物逐段审阅可用", segmentCount === 3, { segmentCount });
    if (segmentCount === 3) {
      await cdp.eval(`(()=>{const areas=[...document.querySelectorAll('.artifact-review-segment textarea')]; const setter=Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set; setter.call(areas[1],'改写后的第二段。'); areas[1].dispatchEvent(new Event('input',{bubbles:true})); const del=[...document.querySelectorAll('.artifact-review-segment')][2]?.querySelector('[aria-label="删除该段"]'); del?.click(); return true;})()`);
      await sleep(400);
      await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.artifact-review button')].find(x=>x.textContent.includes('保存人工版本')); b?.click(); return true;})()`);
      await sleep(2200);
      const view = await cdp.invokeFlat("artifact_get", { artifactId: summary.id, versionId: null });
      const userVersion = view.versions.find((version) => version.origin === "user_edit");
      const segments = userVersion?.content?.sections?.[0]?.segments ?? [];
      record("产物保存人工版本且旧版不覆盖", view.versions.length >= 2 && segments.length === 2 && segments[1].text === "改写后的第二段。", {
        versions: view.versions.map((version) => [version.versionNo, version.origin]),
        segments: segments.map((segment) => segment.text),
      });
    }

    // 资料列表「阅览/删除」按钮命中(窄 dock 300px;真实鼠标打开删除确认后取消)
    await cdp.eval(`(()=>{document.querySelector('#dock-tab-artifacts')?.click();return true;})()`);
    await sleep(700);
    await cdp.eval(`(()=>{const t=[...document.querySelectorAll('.art-scope-tab')].find(x=>x.textContent?.includes('总体'));t?.click();return true;})()`);
    await sleep(900);
    const dockWidthBefore = await cdp.eval(`document.querySelector('.dock-panel')?.getBoundingClientRect().width ?? null`);
    const artResizer = await cdp.eval(`(()=>{const rs=[...document.querySelectorAll('.panel-resizer')];const el=rs[rs.length-1];if(!el)return null;const r=el.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
    let artHit = null;
    if (artResizer) {
      await cdp.drag([artResizer, { x: artResizer.x + 400, y: artResizer.y }]);
      await sleep(800);
      const measure = async () =>
        cdp.eval(`(()=>{const cards=[...document.querySelectorAll('.art-card')];const row=cards[0];if(!row)return null;const hit=(sel)=>{const el=row.querySelector(sel);if(!el)return null;const r=el.getBoundingClientRect();const x=r.left+r.width/2,y=r.top+r.height/2;const h=document.elementFromPoint(x,y);return {x,y,inside:h===el||el.contains(h),right:+r.right.toFixed(1)};};return {dock:document.querySelector('.dock-panel')?.getBoundingClientRect().width??null,read:hit('button[aria-label="阅览该产物"]'),del:hit('button[aria-label="删除该产物"]')};})()`);
      const at1280 = await measure();
      const delAt1280 = at1280?.del?.inside === true ? await cdp.drag([{ x: at1280.del.x, y: at1280.del.y }]).then(() => true) : false;
      await sleep(800);
      const confirm = await cdp.eval(`(()=>{const row=document.querySelector('.art-confirm-row');return row?{text:row.textContent?.trim().slice(0,160)??'',visible:row.getBoundingClientRect().width>0}:null;})()`);
      await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.art-confirm-row button')].find(x=>(x.textContent??'').trim()==='取消');b?.click();return !!b;})()`);
      await cdp.setWindowSize(960, 640);
      await sleep(900);
      const at960 = await measure();
      await cdp.setWindowSize(1280, 800);
      await sleep(900);
      if (artResizer && dockWidthBefore) {
        const nowResizer = await cdp.eval(`(()=>{const rs=[...document.querySelectorAll('.panel-resizer')];const el=rs[rs.length-1];if(!el)return null;const r=el.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
        if (nowResizer) {
          const restoreDelta = Math.max(0, Math.round((at1280?.dock ?? 300) - dockWidthBefore));
          if (restoreDelta > 0) await cdp.drag([nowResizer, { x: nowResizer.x - restoreDelta, y: nowResizer.y }]);
        }
      }
      artHit = { dockWidthBefore, at1280, at960, delAt1280, confirm };
      record(
        "资料列表阅览/删除按钮在 1280/960 窄 dock 下各自命中",
        at1280?.dock !== null && at1280?.dock <= 320 && at1280?.read?.inside === true && at1280?.del?.inside === true &&
          at960?.dock !== null && at960?.dock <= 420 && at960?.read?.inside === true && at960?.del?.inside === true &&
          delAt1280 === true && confirm !== null && (confirm.text ?? "").includes("个版本"),
        artHit,
      );
    } else {
      record("资料列表阅览/删除按钮在 1280/960 窄 dock 下各自命中", false, { reason: "resizer missing" });
    }
    // 恢复到 420px,知识图交互在更宽的 dock 中验证(300px 场景由 verify-graph 专项覆盖)。
    const resizerNow = await cdp.eval(`(()=>{const rs=[...document.querySelectorAll('.panel-resizer')];const el=rs[rs.length-1];if(!el)return null;const r=el.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
    if (resizerNow) {
      const dockNow = await cdp.eval(`document.querySelector('.dock-panel')?.getBoundingClientRect().width ?? 0`);
      if (dockNow < 420) await cdp.drag([resizerNow, { x: resizerNow.x - (420 - dockNow), y: resizerNow.y }]);
      await sleep(500);
    }

    // 知识图:显式切到笔记 dock 标签(资料标签也会挂载 NotesDock,但面板 hidden)。
    await openDocument(cdp, mdName, "markdown");
    await cdp.clickElement('#dock-tab-notes').catch(() => undefined);
    await sleep(800);
    if ((await cdp.eval(`document.querySelector('#dock-tab-notes')?.getAttribute('aria-selected')`)) !== "true") {
      await cdp.eval(`(()=>{document.querySelector('#dock-tab-notes')?.click();return true;})()`);
      await sleep(800);
    }
    await cdp.clickElement('.notes-view-tab[aria-label="知识图"]').catch(() => undefined);
    await sleep(1200);
    const notesTabState = await cdp.eval(`(()=>({selected:document.querySelector('#dock-tab-notes')?.getAttribute('aria-selected'),hidden:document.querySelector('#dock-panel-notes')?.hidden,graph:document.querySelector('.knowledge-graph')?.getBoundingClientRect().toJSON()??null}))()`);
    record("知识图前笔记标签真实激活", notesTabState.selected === "true" && notesTabState.hidden === false && notesTabState.graph !== null && notesTabState.graph.width > 0, notesTabState);
    const graph = await cdp.eval(`(()=>{const g=document.querySelector('.knowledge-graph'); if(!g)return null; return {nodes:g.querySelectorAll('.kg-node').length,edges:g.querySelectorAll('.kg-edge').length,filters:g.querySelectorAll('.kg-filter').length};})()`);
    record("知识图只用真实节点/边并可过滤", graph !== null && graph.nodes > 0 && graph.filters === 6, graph);
    if (graph !== null) {
      const legend = await cdp.eval(`(()=>{const g=document.querySelector('.knowledge-graph');return {legend:g.querySelectorAll('.kg-legend-edge').length,shapes:{rect:g.querySelectorAll('.kg-node rect').length,circle:g.querySelectorAll('.kg-node circle').length,polygon:g.querySelectorAll('.kg-node polygon').length},search:!!g.querySelector('.kg-search input')};})()`);
      record(
        "知识图有类型图例/搜索/形状区分",
        legend.legend === 4 && legend.search && (legend.shapes.rect + legend.shapes.circle + legend.shapes.polygon) >= graph.nodes,
        legend,
      );

      await cdp.eval(`(()=>{const input=document.querySelector('.kg-search input');const setter=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;setter.call(input,'Monte');input.dispatchEvent(new Event('input',{bubbles:true}));return true;})()`);
      await sleep(500);
      const search = await cdp.eval(`(()=>({count:document.querySelector('.kg-search-count')?.textContent??null,dimmed:document.querySelectorAll('.kg-node.dimmed').length}))()`);
      record("知识图搜索高亮/计数", search.count !== null && search.dimmed > 0, search);
      await cdp.eval(`(()=>{const input=document.querySelector('.kg-search input');const setter=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;setter.call(input,'');input.dispatchEvent(new Event('input',{bubbles:true}));return true;})()`);
      await sleep(300);

      // 窄 dock 下工具/图例会超过高度:先滚动到画布,保证节点/控件在可视区。
      await cdp.eval(`(()=>{const c=document.querySelector('.kg-canvas');c?.scrollIntoView({block:'center'});return true})()`);
      await sleep(400);
      const beforeDetail = await cdp.eval(`document.querySelector('.kg-detail')?.innerText ?? ''`);
      const nodePick = await cdp.eval(`(()=>{const all=[...document.querySelectorAll('.kg-node')];const n=all.find(x=>x.getAttribute('aria-label')?.startsWith('标注:'))??all.find(x=>!x.getAttribute('aria-label')?.startsWith('文档:'));if(!n)return null;n.scrollIntoView({block:'center'});const r=n.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2,aria:n.getAttribute('aria-label')};})()`);
      const nodePoint = nodePick === null ? null : { x: nodePick.x, y: nodePick.y };
      if (nodePoint) await cdp.clickPoint(nodePoint);
      await sleep(600);
      const afterDetail = await cdp.eval(`(()=>({text:document.querySelector('.kg-detail')?.innerText??'',back:!!document.querySelector('[aria-label="回到原文"]')}))()`);
      const nodeKind = String(nodePick?.aria ?? "").split(":")[0];
      record(
        "知识图节点可定位/显示详情(真实鼠标)",
        nodePoint !== null && afterDetail.text !== beforeDetail && nodeKind !== "" && afterDetail.text.includes(nodeKind) && afterDetail.back,
        { aria: nodePick?.aria ?? null, afterDetail },
      );

      const beforeTransform = await cdp.eval(`document.querySelector('.kg-canvas > g')?.getAttribute('transform') ?? null`);
      const canvasRect = await cdp.eval(`(()=>{const c=document.querySelector('.kg-canvas');c.scrollIntoView({block:'center'});const r=c.getBoundingClientRect();return {left:r.left,top:r.top,w:r.width,h:r.height};})()`);
      if (canvasRect) {
        await cdp.drag([{x:canvasRect.left+18,y:canvasRect.top+canvasRect.h-18},{x:canvasRect.left+88,y:canvasRect.top+canvasRect.h-58}]);
        const panned = await cdp.eval(`document.querySelector('.kg-canvas > g')?.getAttribute('transform') ?? null`);
        record("知识图画布可拖动平移(真实鼠标)", panned !== beforeTransform, { beforeTransform, panned });
        await cdp.wheel(canvasRect.left+canvasRect.w/2, canvasRect.top+canvasRect.h/2, -240, 4);
        const zoomed = await cdp.eval(`document.querySelector('.kg-canvas > g')?.getAttribute('transform') ?? null`);
        record("知识图滚轮可缩放", zoomed !== panned, { panned, zoomed });
        await cdp.clickElement('[aria-label="重置缩放"]');
        const reset = await cdp.eval(`document.querySelector('.kg-canvas > g')?.getAttribute('transform') ?? null`);
        record("知识图重置缩放回到 1×", reset === "translate(0,0) scale(1)", { reset });
      }

      if (nodePoint) {
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: nodePoint.x, y: nodePoint.y });
        await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: nodePoint.x, y: nodePoint.y, button: "left", buttons: 1, clickCount: 1 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: nodePoint.x, y: nodePoint.y, button: "left", buttons: 0, clickCount: 1 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: nodePoint.x, y: nodePoint.y, button: "left", buttons: 1, clickCount: 2 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: nodePoint.x, y: nodePoint.y, button: "left", buttons: 0, clickCount: 2 });
        await sleep(700);
        const focused = await cdp.eval(`(()=>({chip:[...document.querySelectorAll('.kg-chip')].some(b=>b.textContent?.includes('显示全部')),nodes:document.querySelectorAll('.kg-node').length}))()`);
        record("知识图双击聚焦邻居并可显示全部", focused.chip && focused.nodes <= graph.nodes, focused);
        await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.kg-chip')].find(x=>x.textContent?.includes('显示全部'));b?.click();return true;})()`);
        await sleep(500);
      }

      const fsHitInfo = await cdp.clickChecked('[aria-label="知识图全屏"]').catch(() => null);
      const fsHit = fsHitInfo?.inside === true;
      await sleep(500);
      const fsOn = await cdp.eval(`!!document.querySelector('.kg-fullscreen')`);
      await cdp.key("Escape", "Escape", 0, 27);
      const fsOff = await cdp.eval(`!document.querySelector('.kg-fullscreen')`);
      record("知识图全屏可打开/Esc 退出", fsHit && fsOn && fsOff, { fsOn, fsOff });
    }

    // 翻译入口去重:AI 工具菜单只保留「生成当前页/本节译文产物」
    await cdp.eval(`(()=>{document.querySelector('#dock-tab-agent')?.click();return true;})()`);
    await sleep(600);
    const toolHit6 = await cdp.clickElement('.agent-tool-trigger').then(() => true).catch(() => false);
    await sleep(500);
    const menuLabels6 = await cdp.eval(`(()=>[...document.querySelectorAll('.agent-tool-item .agent-tool-item-label')].map(x=>x.textContent?.trim()))()`);
    record(
      "翻译入口去重:工具菜单无「翻译整篇文档」",
      toolHit6 && menuLabels6.includes("生成当前页/本节译文产物") && !menuLabels6.includes("翻译整篇文档"),
      { menuLabels6 },
    );
    await cdp.key("Escape", "Escape", 0, 27);

    // 双文档对照
    await openDocument(cdp, pdfName, "pdf");
    await ensureSidebarOpen(cdp);
    // 对照需要足够宽的主区:收起右栏,避免窄主区退化为上下堆叠(容器查询)。
    if (await cdp.eval(`!!document.querySelector('.dock-panel')`)) {
      await cdp.clickElement('[aria-label="收起右侧面板"]');
      await sleep(700);
    }
    const tabCount = await cdp.eval(`document.querySelectorAll('.doc-tab').length`);
    if (tabCount >= 2) {
      // 对照入口改为先选文档:点击后弹出选择器,选择本轮的合成 md(不碰用户文档)。
      await cdp.eval(`(()=>{document.querySelector('.tab-compare')?.click(); return true;})()`);
      await sleep(500);
      const picker = await cdp.eval(`(()=>{const m=document.querySelector('.tab-compare-menu');const items=[...m?.querySelectorAll('.tab-compare-item')??[]];return {menu:!!m,items:items.length,names:items.map(b=>b.textContent?.trim()).slice(0,6)};})()`);
      await cdp.eval(`(()=>{const item=[...document.querySelectorAll('.tab-compare-menu .tab-compare-item')].find(b=>b.textContent?.includes(${JSON.stringify(mdName)})); item?.click(); return true;})()`);
      // 两栏都必须完成加载并出现各自可滚动容器,否则“正文准备中…”会掩盖真实滚动。
      await waitFor(
        cdp,
        `(()=>{const panes=[...document.querySelectorAll('.reader-compare-pane')];return panes.length===2 && panes.every(p=>!p.innerText.includes('正文准备中') && [...p.querySelectorAll('*')].some(el=>el instanceof HTMLElement && el.scrollHeight>el.clientHeight+8 && (getComputedStyle(el).overflowY==='auto'||getComputedStyle(el).overflowY==='scroll')));})()`,
        30000,
      );
      await sleep(500);
      const artifactPane = await cdp.eval(`!!document.querySelector('.artifact-reading-pane')`);
      // 两栏各自真实滚动:把滚轮直接派发到各自的实际滚动容器中心,避免落在工具栏/浮层上。
      const paneCenters = await cdp.eval(`(()=>[...document.querySelectorAll('.reader-compare-pane')].map(p=>{const e=[p,...p.querySelectorAll('*')].find((el)=>el instanceof HTMLElement && el.scrollHeight>el.clientHeight+8 && (getComputedStyle(el).overflowY==='auto'||getComputedStyle(el).overflowY==='scroll'));if(!e)return null;const r=e.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2,cls:String(e.className||'').slice(0,40)};}))()`);
      for (const center of paneCenters) if (center) await cdp.wheel(center.x, center.y, 360, 4);
      await sleep(700);
      const panes = await cdp.eval(`(()=>{const panes=[...document.querySelectorAll('.reader-compare-pane')];const scrolls=panes.map(p=>{const candidates=[p,...p.querySelectorAll('*')];const e=candidates.find((el)=>el instanceof HTMLElement && el.scrollHeight>el.clientHeight+8 && (getComputedStyle(el).overflowY==='auto'||getComputedStyle(el).overflowY==='scroll'));return e?e.scrollTop:null;});return {panes:panes.length,scrolls,debug:panes.map(p=>{const r=p.getBoundingClientRect();const sc=[p,...p.querySelectorAll('*')].filter(el=>el instanceof HTMLElement).map(el=>({c:String(el.className||'').slice(0,40),sh:el.scrollHeight,ch:el.clientHeight,ov:getComputedStyle(el).overflowY})).filter(e=>e.sh>e.ch+8).slice(0,4);return {w:+r.width.toFixed(1),h:+r.height.toFixed(1),text:(p.innerText||'').slice(0,50),sc};})};})()`);
      record("双文档对照并排且各自滚动", panes.panes === 2 && picker.menu && picker.items > 0 && panes.scrolls.length === 2 && panes.scrolls.every((top) => top > 0), { ...panes, tabCount, artifactPane, picker });
      // —— 中缝拖动:真实鼠标按下/移动/抬起 ——
      // 先双击把比例归一到 50/50,避免上次运行残留比例影响拖动断言。
      const normalizeSplit = await cdp.eval(`(()=>{const s=document.querySelector('.compare-splitter');if(!s)return null;const r=s.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
      if (normalizeSplit !== null) {
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: normalizeSplit.x, y: normalizeSplit.y });
        await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: normalizeSplit.x, y: normalizeSplit.y, button: "left", buttons: 1, clickCount: 1 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: normalizeSplit.x, y: normalizeSplit.y, button: "left", buttons: 0, clickCount: 1 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: normalizeSplit.x, y: normalizeSplit.y, button: "left", buttons: 1, clickCount: 2 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: normalizeSplit.x, y: normalizeSplit.y, button: "left", buttons: 0, clickCount: 2 });
        await sleep(700);
      }
      const splitter = await cdp.eval(`(()=>{const s=document.querySelector('.compare-splitter');if(!s)return null;const r=s.getBoundingClientRect();const el=document.querySelector('.reader-compare');const er=el.getBoundingClientRect();const widths=[...document.querySelectorAll('.reader-compare-pane')].map(p=>p.getBoundingClientRect().width);const hit=document.elementFromPoint(r.left+r.width/2,r.top+r.height/2);return {x:r.left+r.width/2,y:r.top+r.height/2,hit:hit===s||s.contains(hit),left:widths[0],right:widths[1],containerLeft:er.left,containerWidth:er.width};})()`);
      let dragCheck = { skipped: true };
      if (splitter !== null) {
        await cdp.drag([{ x: splitter.x, y: splitter.y }, { x: splitter.x + 140, y: splitter.y }]);
        await sleep(600);
        dragCheck = await cdp.eval(`(()=>{const widths=[...document.querySelectorAll('.reader-compare-pane')].map(p=>p.getBoundingClientRect().width);const stored=JSON.parse(localStorage.getItem('knowwise.ui.layout.v1')||'{}').compareRatio??null;return {left:widths[0],right:widths[1],stored,aria:document.querySelector('.compare-splitter')?.getAttribute('aria-valuenow')??null};})()`);
        record("双文档分栏真实鼠标拖动改变左右宽度", splitter.hit === true && dragCheck.left > splitter.left + 80 && dragCheck.right < splitter.right - 80, { before: { left: splitter.left, right: splitter.right }, after: dragCheck });
        // —— 键盘 ←/→:聚焦分隔条后真实按键 ——
        await cdp.clickElement(".compare-splitter");
        await sleep(250);
        await cdp.key("ArrowLeft", "ArrowLeft", 0, 37);
        await sleep(600);
        const afterKey = await cdp.eval(`(()=>{const widths=[...document.querySelectorAll('.reader-compare-pane')].map(p=>p.getBoundingClientRect().width);return {left:widths[0],right:widths[1],focused:document.activeElement?.classList?.contains('compare-splitter')??false};})()`);
        record("双文档分栏键盘左键收窄左栏", afterKey.focused === true && afterKey.left < dragCheck.left - 10, { before: dragCheck.left, after: afterKey });
        // —— 最小宽度:拖到最左,左栏不得小于 240px ——
        const splitNow = await cdp.eval(`(()=>{const s=document.querySelector('.compare-splitter');const r=s.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
        await cdp.drag([{ x: splitNow.x, y: splitNow.y }, { x: splitter.containerLeft + 20, y: splitNow.y }]);
        await sleep(600);
        const minCheck = await cdp.eval(`(()=>{const widths=[...document.querySelectorAll('.reader-compare-pane')].map(p=>p.getBoundingClientRect().width);return {left:widths[0],right:widths[1]};})()`);
        record("双文档分栏最小宽度约束", minCheck.left >= 235 && minCheck.left <= 320, { minCheck });
        // —— 双击复位 50/50 ——
        const splitCenter = await cdp.eval(`(()=>{const s=document.querySelector('.compare-splitter');const r=s.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: splitCenter.x, y: splitCenter.y });
        await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: splitCenter.x, y: splitCenter.y, button: "left", buttons: 1, clickCount: 1 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: splitCenter.x, y: splitCenter.y, button: "left", buttons: 0, clickCount: 1 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: splitCenter.x, y: splitCenter.y, button: "left", buttons: 1, clickCount: 2 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: splitCenter.x, y: splitCenter.y, button: "left", buttons: 0, clickCount: 2 });
        await sleep(700);
        const resetCheck = await cdp.eval(`(()=>{const widths=[...document.querySelectorAll('.reader-compare-pane')].map(p=>p.getBoundingClientRect().width);const stored=JSON.parse(localStorage.getItem('knowwise.ui.layout.v1')||'{}').compareRatio??null;return {left:widths[0],right:widths[1],stored};})()`);
        record("双文档分栏双击恢复 50/50", Math.abs(resetCheck.left - resetCheck.right) <= 8 && Math.abs((resetCheck.stored ?? 0) - 0.5) <= 0.01, resetCheck);
      } else {
        record("双文档分栏真实鼠标拖动改变左右宽度", false, dragCheck);
        record("双文档分栏键盘左键收窄左栏", false, dragCheck);
        record("双文档分栏最小宽度约束", false, dragCheck);
        record("双文档分栏双击恢复 50/50", false, dragCheck);
      }
      await cdp.eval(`(()=>{document.querySelector('.reader-compare-exit')?.click(); return true;})()`);
    } else {
      record("双文档对照并排且各自滚动", false, { tabCount }, true);
    }

    // 会话置顶/分支(真实命令)
    const conv = await cdp.invoke("conversation_create", { documentId: mdId, title: `E2E 会话 ${STAMP}`, idempotencyKey: `e2e-conv-${STAMP}` });
    createdConversations.push(conv.id);
    await cdp.invoke("message_append_user", { conversationId: conv.id, text: "E2E 测试问题。", idempotencyKey: `e2e-msg-${STAMP}` });
    const pinned = await cdp.invoke("conversation_pin", { conversationId: conv.id, pinned: true });
    const branch = await cdp.invoke("conversation_branch", { conversationId: conv.id, idempotencyKey: `e2e-branch-${STAMP}` });
    createdConversations.push(branch.id);
    record("会话置顶/分支(原会话不变)", pinned.pinned === true && branch.messageCount === 1 && branch.id !== conv.id, {
      pinned: pinned.pinned,
      branchCount: branch.messageCount,
      branchTitle: branch.title,
    });

    // 阅读简报(单文件,真实标注+问答)
    const briefPath = path.join(TEMP_DIR, "brief.md");
    const brief = await cdp.invoke("reading_brief_export", { documentId: mdId, format: "markdown", path: briefPath });
    const briefText = readFileSync(briefPath, "utf8");
    record("阅读简报导出单文件", existsSync(briefPath) && brief.annotationCount >= 1 && briefText.includes("## 问答"), {
      bytes: brief.bytesWritten,
      annotations: brief.annotationCount,
      qa: brief.qaCount,
    });

    // PDF 标注管理与标注菜单删除确认
    await openDocument(cdp, pdfName, "pdf");
    const frame = await cdp.eval(`(()=>{const f=document.querySelector('.pdf-page-frame');const s=document.querySelector('.pdf-scroll');if(!f||!s)return null;const r=f.getBoundingClientRect(),v=s.getBoundingClientRect();return {x0:Math.round(Math.max(r.left,v.left)+30),x1:Math.round(Math.min(r.right,v.right)-40),y:Math.round(Math.min(r.top+r.height*.35,v.bottom-90))};})()`);
    if (frame !== null) {
      await cdp.clickLabel("画笔");
      await cdp.drag([{ x: frame.x0, y: frame.y }, { x: frame.x1, y: frame.y + 8 }], 8);
      await sleep(700);
      await cdp.clickLabel("标注管理");
      await sleep(600);
      const manager = await cdp.eval(`(()=>{const m=document.querySelector('.pdf-manager');return m?{stats:m.querySelector('.pdf-manager-stats')?.innerText,items:m.querySelectorAll('.pdf-manager-page li').length}:null})()`);
      record("标注管理:按页/类型/颜色统计", manager !== null && manager.items > 0 && String(manager.stats).includes("笔迹"), manager);
      if (manager !== null) {
        await cdp.eval(`(()=>{document.querySelector('.pdf-manager-page input[type=checkbox]')?.click(); return true;})()`);
        await sleep(200);
        await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.pdf-manager button')].find(x=>x.textContent.includes('批量删除')); b?.click(); return true;})()`);
        await sleep(300);
        record("标注管理批量删除需确认", (await cdp.eval(`document.querySelector('.kw-confirm-dialog')?.innerText ?? null`)) !== null, {});
        await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.kw-confirm-dialog button')].find(x=>x.textContent.trim()==='删除'); b?.click(); return true;})()`);
        await sleep(700);
        await cdp.eval(`(()=>{document.querySelector('[aria-label="关闭标注管理"]')?.click(); return true;})()`);
      }
      // 再画一笔,验证标注菜单删除确认
      await cdp.clickLabel("画笔");
      await cdp.drag([{ x: frame.x0, y: frame.y + 30 }, { x: frame.x1, y: frame.y + 40 }], 8);
      await sleep(700);
      await cdp.clickLabel("画笔"); // 关闭画笔 → 阅读态
      await sleep(300);
      const inkPoint = await cdp.eval(`(()=>{const svg=document.querySelector('svg.pdf-ink'); if(!svg)return null; const r=svg.getBoundingClientRect(); return {x:r.left+20,y:r.top+10};})()`);
      if (inkPoint !== null) {
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: inkPoint.x, y: inkPoint.y });
        await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: inkPoint.x, y: inkPoint.y, button: "left", buttons: 1, clickCount: 1 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: inkPoint.x, y: inkPoint.y, button: "left", buttons: 0, clickCount: 1 });
        await sleep(400);
        await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.mark-menu button')].find(x=>x.textContent.includes('删除')); b?.click(); return true;})()`);
        await sleep(300);
        record("标注菜单删除需确认", (await cdp.eval(`document.querySelector('.kw-confirm-dialog')?.innerText ?? null`)) !== null, {});
        await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.kw-confirm-dialog button')].find(x=>x.textContent.trim()==='取消'); b?.click(); return true;})()`);
      }
    }

    // —— 阶段一:顶部布局几何(1280×800 / 960×640) ——
    const runLayout = async (label, documentName, format, width, height, expectDockAligned) => {
      const resized = await cdp.setWindowSize(width, height);
      await sleep(700);
      await openDocument(cdp, documentName, format);
      const quiet = await waitReaderQuiet(cdp, 15000);
      await sleep(500);
      const metrics = await measureLayout(cdp);
      const verdict = validTopLayout(metrics, expectDockAligned);
      record(
        `顶部布局 ${width}×${height} ${format}(${label})`,
        resized && quiet && verdict.ok,
        { resized, quiet, gaps: metrics.gaps, banner: metrics.banner, dockPosition: metrics.dockPosition, dock: metrics.dock, toolbar: metrics.toolbar },
      );
      return metrics;
    };
    await cdp.eval(`(()=>{document.querySelector('[aria-label="关闭任务中心"]')?.click(); return true;})()`);
    if (await cdp.eval(`!!!document.querySelector('.dock-panel')`)) {
      await cdp.eval(`(()=>{document.querySelector('[aria-label="AI 助手"]')?.click(); return true;})()`);
      await sleep(500);
    }
    await runLayout("Markdown", mdName, "markdown", 1280, 800, true);
    await runLayout("PDF", pdfName, "pdf", 1280, 800, true);
    await runLayout("PDF", pdfName, "pdf", 960, 640, false);
    await runLayout("Markdown", mdName, "markdown", 960, 640, false);
    await cdp.setWindowSize(1280, 800);
    await sleep(500);

    // —— 阶段一:任务中心文档→会话→任务 + 滚轮(合成数据) ——
    const SCROLL_DOC_COUNT = 16;
    const SCROLL_EXTRA_PARSE = 15;
    const SCROLL_CONVERSATIONS = 18;
    for (let i = 0; i < SCROLL_DOC_COUNT; i += 1) {
      const name = `kw-scroll-${STAMP}-${String(i).padStart(2, "0")}`;
      const filePath = path.join(TEMP_DIR, `${name}.md`);
      writeFileSync(filePath, makeMarkdown(name), "utf8");
      const imported = await cdp.invoke("document_import", {
        path: filePath,
        idempotencyKey: `e2e-scroll-import-${STAMP}-${i}`,
      });
      scrollDocs.push({ name, path: filePath, documentId: imported.documentId, versionId: imported.versionId });
      const parsed = await cdp
        .invoke("parse_start", {
          documentId: imported.documentId,
          versionId: imported.versionId,
          reason: "e2e_scroll",
          idempotencyKey: `e2e-scroll-parse-${STAMP}-${i}`,
        })
        .catch(() => null);
      if (parsed?.id) scrollTaskIds.push(parsed.id);
    }
    record("滚轮测试合成文档就绪", scrollDocs.length === SCROLL_DOC_COUNT, { docs: scrollDocs.length });
    const scrollDoc = scrollDocs[0];
    const clickDocByName = async (name) => {
      const index = await cdp.eval(
        `[...document.querySelectorAll('.task-doc-item')].findIndex((x)=>x.innerText.includes(${JSON.stringify(name)}))`,
      );
      if (index < 0) throw new Error(`任务中心找不到文档:${name}`);
      await cdp.clickElement(`.task-doc-list > li:nth-child(${index + 1}) .task-doc-item`);
      await sleep(250);
    };
    for (let j = 0; j < SCROLL_EXTRA_PARSE; j += 1) {
      const task = await cdp
        .invoke("parse_start", {
          documentId: scrollDoc.documentId,
          versionId: scrollDoc.versionId,
          reason: "e2e_scroll_extra",
          idempotencyKey: `e2e-scroll-extra-${STAMP}-${j}`,
        })
        .catch(() => null);
      if (task?.id) scrollTaskIds.push(task.id);
    }
    for (let k = 0; k < SCROLL_CONVERSATIONS; k += 1) {
      await cdp
        .invoke("conversation_create", {
          documentId: scrollDoc.documentId,
          title: `E2E 滚动会话 ${k + 1}`,
          idempotencyKey: `e2e-scroll-conv-${STAMP}-${k}`,
        })
        .catch(() => null);
    }
    await sleep(1200);
    await cdp.eval(`(()=>{document.querySelector('[aria-label=任务中心]')?.click(); return true;})()`);
    await waitFor(
      cdp,
      `(()=>{const docs=[...document.querySelectorAll('.task-doc-item')].map(x=>x.innerText); const active=document.querySelector('.task-doc-item.active')?.innerText??''; const rows=document.querySelectorAll('.task-center-list .task-row').length; return docs.some(t=>t.includes(${JSON.stringify(scrollDoc.name)})) && active.includes(${JSON.stringify(scrollDoc.name)}) && rows>=${SCROLL_EXTRA_PARSE};})()`,
      30000,
    );
    await sleep(800);
    let tc = await readTaskCenter(cdp);
    record(
      "任务中心默认选中最近有任务的文档和最近会话",
      tc.open &&
        String(tc.activeDoc).includes(scrollDoc.name) &&
        String(tc.activeConv).includes("未关联对话") &&
        tc.rows >= SCROLL_EXTRA_PARSE,
      { activeDoc: tc.activeDoc, activeConv: tc.activeConv, rows: tc.rows, docs: tc.docs.length, convs: tc.convs.length, current: tc.current },
    );

    const emptyConvIndex = await cdp.eval(
      `[...document.querySelectorAll('.task-conv-item')].findIndex((x)=>!x.classList.contains('unlinked') && x.innerText.trim().endsWith('0'))`,
    );
    if (emptyConvIndex >= 0) {
      await cdp.clickElement(`.task-conv-list > li:nth-child(${emptyConvIndex + 1}) .task-conv-item`);
      const emptyConvState = await readTaskCenter(cdp);
      record(
        "任务中心可进入无任务会话并显示可操作空态",
        String(emptyConvState.activeConv).includes("0") &&
          emptyConvState.rows === 0 &&
          String(emptyConvState.empty).includes("暂无任务") &&
          (await cdp.eval(`!!document.querySelector('.task-empty-state button')`)),
        { activeConv: emptyConvState.activeConv, empty: emptyConvState.empty },
      );
    } else {
      record("任务中心可进入无任务会话并显示可操作空态", false, { convs: tc.convs });
    }
    await cdp.clickElement(".task-conv-item.unlinked");
    tc = await readTaskCenter(cdp);
    record("任务中心切回「未关联对话」显示该文档未归属任务", String(tc.activeConv).includes("未关联对话") && tc.rows >= SCROLL_EXTRA_PARSE, { activeConv: tc.activeConv, rows: tc.rows });

    const otherDocIndex = await cdp.eval(
      `[...document.querySelectorAll('.task-doc-item')].findIndex((x)=>!x.innerText.includes(${JSON.stringify(scrollDoc.name)}))`,
    );
    const otherDocName = await cdp.eval(
      `[...document.querySelectorAll('.task-doc-item')][${otherDocIndex}]?.querySelector('.task-doc-title')?.innerText ?? null`,
    );
    if (otherDocIndex >= 0) {
      await cdp.clickElement(`.task-doc-list > li:nth-child(${otherDocIndex + 1}) .task-doc-item`);
      const switched = await readTaskCenter(cdp);
      record(
        "任务中心切换文档后会话与任务联动",
        !String(switched.activeDoc).includes(scrollDoc.name) &&
          String(switched.current).includes("当前文档") &&
          String(switched.activeConv).length > 0,
        { activeDoc: switched.activeDoc, activeConv: switched.activeConv, rows: switched.rows },
      );
      await clickDocByName(scrollDoc.name);
      await cdp.clickElement(".task-conv-item.unlinked");
    }

    // 滚轮:1280×800 三个列表
    await resetTaskScrolls(cdp);
    const listPoint = async (selector) => {
      await cdp.eval(`document.querySelector(${JSON.stringify(selector)})?.scrollIntoView({block:'center'})`);
      await sleep(250);
      return cdp.eval(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});if(!e)return null;const r=e.getBoundingClientRect();return {x:Math.round(r.left+r.width/2),y:Math.round(r.top+r.height/2)};})()`);
    };
    const wheelList = async (selector, label, deltaY = 150) => {
      await resetTaskScrolls(cdp);
      const point = await listPoint(selector);
      if (point === null) return false;
      const before = await readTaskCenter(cdp);
      await cdp.wheel(point.x, point.y, deltaY, 3);
      const after = await readTaskCenter(cdp);
      const key = selector.includes("doc") ? "doc" : selector.includes("conv") ? "conv" : "tasks";
      const changed = after.scrolls[key] !== null && after.scrolls[key].scrollTop > before.scrolls[key].scrollTop;
      record(`滚轮 ${label}`, changed, { point, before: before.scrolls[key], after: after.scrolls[key] });
      return changed;
    };
    await wheelList(".task-doc-list", "1280 文档列表");
    await wheelList(".task-conv-list", "1280 会话列表");
    await wheelList(".task-center-list", "1280 任务列表");
    const bodyFit1280 = await cdp.eval(`(()=>{const e=document.querySelector('.task-center-body');return e?{sh:e.scrollHeight,ch:e.clientHeight,overflow:getComputedStyle(e).overflowY}:null;})()`);
    record("任务中心外层 1280 内容不溢出(无需滚动)", bodyFit1280 !== null && bodyFit1280.sh <= bodyFit1280.ch + 1, bodyFit1280);

    // 滚轮:960×640 三个列表 + 外层整体
    await cdp.setWindowSize(960, 640);
    await sleep(900);
    // 窗口尺寸变化可能晚于 CDP 返回;等任务中心容器高度连续两次一致再测,
    // 否则滚轮坐标会落在重排前后的不同位置(测试环境抖动,非产品行为)。
    {
      let lastHeight = -1;
      for (let i = 0; i < 25; i += 1) {
        const height = await cdp.eval(`document.querySelector('.task-center-body')?.clientHeight ?? -1`);
        if (height > 0 && height === lastHeight) break;
        lastHeight = height;
        await sleep(200);
      }
    }
    await resetTaskScrolls(cdp);
    const wheelNarrow = async (selector, label) => {
      await resetTaskScrolls(cdp);
      await cdp.eval(`document.querySelector(${JSON.stringify(selector)})?.scrollIntoView({block:'center'})`);
      await sleep(300);
      const point = await listPoint(selector);
      if (point === null) return false;
      const before = await readTaskCenter(cdp);
      await cdp.wheel(point.x, point.y, 120, 3);
      const after = await readTaskCenter(cdp);
      const key = selector.includes("doc") ? "doc" : selector.includes("conv") ? "conv" : "tasks";
      const changed = after.scrolls[key] !== null && after.scrolls[key].scrollTop > before.scrolls[key].scrollTop;
      record(`滚轮 960 ${label}`, changed, { point, before: before.scrolls[key], after: after.scrolls[key] });
      return changed;
    };
    await wheelNarrow(".task-doc-list", "文档列表");
    await wheelNarrow(".task-conv-list", "会话列表");
    await wheelNarrow(".task-center-list", "任务列表");
    await resetTaskScrolls(cdp);
    // P0-2 要求 960 使用独立三栏(不是纵向堆叠):三栏并排、各自滚动,
    // 外层不再需要滚动(旧断言要求外层滚轮,已与新布局目标冲突)。
    const threeColumn = await cdp.eval(`(()=>{
      const body=document.querySelector('.task-center-body');
      const d=document.querySelector('.task-doc-pane')?.getBoundingClientRect();
      const c=document.querySelector('.task-conv-pane')?.getBoundingClientRect();
      const t=document.querySelector('.task-task-pane')?.getBoundingClientRect();
      if(!body||!d||!c||!t)return null;
      return {
        overflow:getComputedStyle(body).overflowY,
        sideBySide: Math.abs(d.top-c.top)<=1 && Math.abs(c.top-t.top)<=1 && d.right<=c.left+1 && c.right<=t.left+1,
        visible: innerWidth>0 && d.width>0 && c.width>0 && t.width>0,
        widths:[Math.round(d.width),Math.round(c.width),Math.round(t.width)],
      };
    })()`);
    record(
      "任务中心 960 三栏并排且各自独立滚动",
      threeColumn !== null && threeColumn.overflow === "hidden" && threeColumn.sideBySide === true && threeColumn.visible === true,
      threeColumn,
    );
    await cdp.eval(`(()=>{const b=document.querySelector('.task-center-body');if(b)b.scrollTop=b.scrollHeight;const l=document.querySelector('.task-center-list');if(l)l.scrollTop=l.scrollHeight;return true;})()`);
    await sleep(400);
    const clip = await cdp.eval(`(()=>{
      const panel=document.querySelector('.task-center'); const rows=[...document.querySelectorAll('.task-center-list .task-row')]; const last=rows[rows.length-1];
      const btn=last?.querySelector('button[aria-label="删除任务"], .task-row-actions button');
      if(!panel||!btn)return null;
      const pr=panel.getBoundingClientRect(); const br=btn.getBoundingClientRect();
      return {panel:{top:pr.top,bottom:pr.bottom},btn:{top:br.top,bottom:br.bottom},within:br.top>=pr.top-1&&br.bottom<=pr.bottom+1&&br.top>=0&&br.bottom<=innerHeight};
    })()`);
    record("任务列表滚到底后操作按钮不被裁掉", clip !== null && clip.within === true, clip);

    // —— P0-2:960×640 + dock 展开的真实 elementFromPoint 命中 ——
    const themeBefore = await cdp.eval(`(()=>({theme:document.documentElement.dataset.theme??null,mode:document.documentElement.dataset.themeMode??null,stored:localStorage.getItem("knowwise.theme.v1")}))()`);
    await cdp.setWindowSize(960, 640);
    await sleep(600);
    if (await cdp.eval(`!document.querySelector('.dock-panel')`)) {
      await cdp.clickElement('[aria-label="AI 助手"]');
      await sleep(700);
    }
    if (await cdp.eval(`!document.querySelector('.task-center')`)) {
      await cdp.clickElement('[aria-label="任务中心"]');
      await sleep(900);
    }
    const dockOpen = await cdp.eval(`!!document.querySelector('.dock-panel')`);
    const dockLayer = await cdp.eval(`(()=>{const d=document.querySelector('.dock-panel');const t=document.querySelector('.top-bar');if(!d||!t)return null;return {dockZ:getComputedStyle(d).zIndex,topZ:getComputedStyle(t).zIndex,dockPos:getComputedStyle(d).position};})()`);
    record(
      "P0-2 960+dock 层级:顶栏在抽屉之上",
      dockOpen && dockLayer !== null && Number(dockLayer.topZ) > Number(dockLayer.dockZ),
      { dockOpen, dockLayer },
    );

    const tcTriggerHit = await cdp.hitInfo('[aria-label="任务中心"]');
    record(
      "P0-2 960+dock 任务中心按钮命中自身",
      tcTriggerHit !== null && tcTriggerHit.inside === true && tcTriggerHit.inTopBar === true,
      tcTriggerHit,
    );
    await cdp.clickElement('[aria-label="任务中心"]');
    await sleep(400);
    const tcClosed = await cdp.eval(`!document.querySelector('.task-center')`);
    await cdp.clickElement('[aria-label="任务中心"]');
    await sleep(900);
    const tcReopened = await cdp.eval(`!!document.querySelector('.task-center')`);
    record("P0-2 任务中心真实点击可开关(portal 到 body)", tcClosed && tcReopened, { tcClosed, tcReopened });

    const settingsHit = await cdp.hitInfo('[aria-label="设置"]');
    await cdp.clickElement('[aria-label="设置"]');
    await sleep(800);
    const settingsOpen = await cdp.eval(`!!document.querySelector('.settings-dialog')`);
    record("P0-2 960+dock 设置按钮命中并打开", settingsHit !== null && settingsHit.inside === true && settingsOpen, { settingsHit, settingsOpen });
    await cdp.clickElement('[aria-label="关闭设置"]');
    await sleep(400);

    const shortcutHit = await cdp.hitInfo('[aria-label="快捷键帮助"]');
    await cdp.clickElement('[aria-label="快捷键帮助"]');
    await sleep(500);
    const shortcutOpen = await cdp.eval(`!!document.querySelector('.shortcut-dialog')`);
    record("P0-2 960+dock 快捷键按钮命中并打开", shortcutHit !== null && shortcutHit.inside === true && shortcutOpen, { shortcutHit, shortcutOpen });
    await cdp.clickElement('[aria-label="关闭快捷键帮助"]');
    await sleep(400);

    const focusHit = await cdp.hitInfo('[aria-label="切换专注模式"]');
    await cdp.clickElement('[aria-label="切换专注模式"]');
    await sleep(600);
    const focusOn = await cdp.eval(`document.querySelector('.app-shell')?.classList.contains('focus-mode')===true`);
    await cdp.clickElement('[aria-label="切换专注模式"]');
    await sleep(500);
    const focusOff = await cdp.eval(`document.querySelector('.app-shell')?.classList.contains('focus-mode')===false`);
    record("P0-2 960+dock 专注模式按钮命中并可切换", focusHit !== null && focusHit.inside === true && focusOn && focusOff, { focusHit, focusOn, focusOff });

    const themeHit = await cdp.hitInfo('[aria-label="切换主题"]');
    await cdp.clickElement('[aria-label="切换主题"]');
    await sleep(500);
    const themeAfter = await cdp.eval(`document.documentElement.dataset.theme ?? null`);
    await cdp.clickElement('[aria-label="切换主题"]');
    await sleep(300);
    record("P0-2 960+dock 主题按钮命中并可切换", themeHit !== null && themeHit.inside === true && themeAfter !== themeBefore.theme, { themeHit, before: themeBefore.theme, after: themeAfter });

    const accentHit = await cdp.hitInfo('[aria-label="选择强调色"]');
    await cdp.clickElement('[aria-label="选择强调色"]');
    await sleep(350);
    const accentOpen = await cdp.eval(`!!document.querySelector('.accent-popover')`);
    await cdp.clickElement('[aria-label="选择强调色"]');
    await sleep(250);
    record("P0-2 960+dock 强调色按钮命中并打开面板", accentHit !== null && accentHit.inside === true && accentOpen, { accentHit, accentOpen });

    // 恢复主题偏好原值(点击只用于命中验证,不留下用户偏好变更)。
    await cdp.eval(`(()=>{const t=${JSON.stringify(themeBefore)};if(t.stored===null)localStorage.removeItem("knowwise.theme.v1");else localStorage.setItem("knowwise.theme.v1",t.stored);if(t.theme)document.documentElement.dataset.theme=t.theme;if(t.mode)document.documentElement.dataset.themeMode=t.mode;return true;})()`);

    if (await cdp.eval(`!document.querySelector('.task-center')`)) {
      await cdp.clickElement('[aria-label="任务中心"]');
      await sleep(900);
    }
    const tcDocIndex = await cdp.eval(
      `[...document.querySelectorAll('.task-doc-item')].findIndex((x)=>x.innerText.includes(${JSON.stringify(scrollDoc.name)}))`,
    );
    if (tcDocIndex >= 0) {
      await cdp.clickElement(`.task-doc-list > li:nth-child(${tcDocIndex + 1}) .task-doc-item`);
      await sleep(300);
    }
    await cdp.clickElement('.task-conv-item.unlinked');
    await waitFor(cdp, `document.querySelector('.task-center-list .task-row.status-running, .task-center-list .task-row.status-queued')!==null`, 15000);
    const panelHit = await cdp.eval(`(()=>{const p=document.querySelector('.task-center');if(!p)return null;const r=p.getBoundingClientRect();const hit=document.elementFromPoint(r.left+r.width/2,r.top+24);return {z:getComputedStyle(p).zIndex,hit:!!hit?.closest('.task-center')};})()`);
    record(
      "P0-2 任务中心面板在 dock 之上可命中",
      panelHit !== null && panelHit.hit === true && Number(panelHit.z) > Number(dockLayer.dockZ),
      { panelHit, dockZ: dockLayer.dockZ },
    );

    // —— P0-2:本地大 PDF 解析任务(不依赖模型,取消确定性强) ——
    const cancelPdfName = `kw-cancel-${STAMP}`;
    const cancelPdfPath = path.join(TEMP_DIR, `${cancelPdfName}.pdf`);
    makeBlankPdf(cancelPdfPath, 200);
    const cancelDoc = await cdp.invoke("document_import", {
      path: cancelPdfPath,
      idempotencyKey: `e2e-cancel-import-${STAMP}`,
    });
    scrollDocs.push({
      name: cancelPdfName,
      path: cancelPdfPath,
      documentId: cancelDoc.documentId,
      versionId: cancelDoc.versionId,
    });
    const parseTask = await cdp.invoke("parse_start", {
      documentId: cancelDoc.documentId,
      versionId: cancelDoc.versionId,
      idempotencyKey: `e2e-cancel-parse-${STAMP}`,
    });
    // 关闭再打开任务中心,强制按最新任务刷新列表。
    if (await cdp.eval(`!!document.querySelector('.task-center')`)) {
      await cdp.clickElement('[aria-label="关闭任务中心"]');
      await sleep(300);
    }
    await cdp.clickElement('[aria-label="任务中心"]');
    await sleep(900);
    const cancelDocFound = await waitFor(
      cdp,
      `[...document.querySelectorAll('.task-doc-item')].some((x)=>x.innerText.includes(${JSON.stringify(cancelPdfName)}))`,
      15000,
    );
    const cancelDocIndex = await cdp.eval(
      `[...document.querySelectorAll('.task-doc-item')].findIndex((x)=>x.innerText.includes(${JSON.stringify(cancelPdfName)}))`,
    );
    if (cancelDocIndex >= 0) {
      await cdp.clickElement(`.task-doc-list > li:nth-child(${cancelDocIndex + 1}) .task-doc-item`);
      await sleep(500);
    }
    const cancelSelector = `.task-row[data-task-id="${parseTask.id}"] button[aria-label="取消任务"]`;
    const runningAppeared = await waitFor(cdp, `!!document.querySelector(${JSON.stringify(cancelSelector)})`, 20000);
    if (runningAppeared) {
      const cancelHit = await cdp.hitInfo(cancelSelector);
      await cdp.clickElement(cancelSelector);
      let cancelSettled = false;
      for (let attempt = 0; attempt < 60; attempt += 1) {
        const page = await cdp.invoke("task_list", { activeOnly: false, limit: 50 });
        const task = page.items.find((item) => item.id === parseTask.id);
        if (task && ["canceled", "failed", "interrupted"].includes(task.status)) {
          cancelSettled = true;
          break;
        }
        await sleep(400);
      }
      record(
        "P0-2 960+dock 任务行取消真实命中且任务收敛为取消",
        cancelHit !== null && cancelHit.inside === true && cancelSettled,
        { taskId: parseTask.id, cancelHit, cancelSettled, cancelDocFound, runningAppeared },
      );
    } else {
      record("P0-2 960+dock 任务行取消真实命中且任务收敛为取消", false, {
        taskId: parseTask.id,
        cancelDocFound,
        runningAppeared,
        status: (await cdp.invoke("task_list", { activeOnly: false, limit: 50 })).items.find((x) => x.id === parseTask.id)?.status,
        elapsedMs: Date.now() - parseTask.createdAt,
      });
    }

    // —— P2-3:取消一个 agent 任务后点“重试”,面板应 ≤10s 关闭并创建新任务 ——
    const hitTitle = `E2E P0-2 ${STAMP}`;
    const hitConversation = await cdp.invoke("conversation_create", {
      documentId: mdId,
      title: hitTitle,
      idempotencyKey: `e2e-hit-conv-${STAMP}`,
    });
    const hitSnapshot = await cdp.invoke("context_freeze", {
      documentId: mdId,
      scopeDraft: { kind: "full_document", confirmed: true },
      idempotencyKey: `e2e-hit-freeze-${STAMP}`,
    });
    const agentPayload = {
      conversationId: hitConversation.id,
      snapshotId: hitSnapshot.id,
      question: "请做一次完整研究:逐节解释文档,给出引用并列出待核实点。",
      displayText: "请做一次完整研究:逐节解释文档,给出引用并列出待核实点。",
      attachmentIds: [],
      locale: "zh-CN",
      idempotencyKey: `e2e-hit-agent-${STAMP}`,
    };
    const agentTask = await cdp.eval(
      `window.__kwRetryAgent = window.__TAURI_INTERNALS__.invoke("agent_run", {request: ${JSON.stringify(agentPayload)}})`,
    );
    let taskCanceled = false;
    if (["running", "queued"].includes(agentTask.status)) {
      await cdp
        .invoke("agent_cancel", { taskId: agentTask.id, expectedRevision: agentTask.stateVersion })
        .catch(() => undefined);
      for (let attempt = 0; attempt < 50; attempt += 1) {
        const page = await cdp.invoke("task_list", { activeOnly: false, limit: 50 });
        const current = page.items.find((item) => item.id === agentTask.id);
        if (current && ["canceled", "failed", "interrupted"].includes(current.status)) {
          taskCanceled = true;
          break;
        }
        await sleep(300);
      }
    }
    if (taskCanceled) {
      // 取消经 IPC 完成,任务中心可能仍是旧快照:关闭再打开强制刷新。
      if (await cdp.eval(`!!document.querySelector('.task-center')`)) {
        await cdp.clickElement('[aria-label="关闭任务中心"]');
        await sleep(300);
      }
      await cdp.clickElement('[aria-label="任务中心"]');
      await sleep(1000);
      const mdFound = await waitFor(
        cdp,
        `[...document.querySelectorAll('.task-doc-item')].some((x)=>x.innerText.includes(${JSON.stringify(mdName)}))`,
        15000,
      );
      const mdDocIndex = await cdp.eval(
        `[...document.querySelectorAll('.task-doc-item')].findIndex((x)=>x.innerText.includes(${JSON.stringify(mdName)}))`,
      );
      if (mdDocIndex >= 0) {
        await cdp.clickElement(`.task-doc-list > li:nth-child(${mdDocIndex + 1}) .task-doc-item`);
      }
      // 等 md 文档真正成为选中项,避免在旧文档的会话列表上立即判断。
      const mdDocActive = await waitFor(
        cdp,
        `document.querySelector('.task-doc-item.active')?.innerText.includes(${JSON.stringify(mdName)})===true`,
        20000,
      );
      // 取消后的任务可能落在“未关联对话”或原会话:先按 task_list 的真实归属取会话标题,
      // 等该会话的任务计数 > 0(证明任务快照已刷新且任务归入该会话),再点击并等行出现;
      // 不能在旧快照上点一次就换下一个会话。
      const agentTaskNow = (await cdp.invoke("task_list", { activeOnly: false, limit: 50 })).items.find(
        (item) => item.id === agentTask.id,
      );
      const agentConvTitle = agentTaskNow?.conversationTitle?.trim() || hitTitle;
      const expectedConvTitle = agentTaskNow?.conversationId != null ? agentConvTitle : "未关联对话";
      const convState = async (title) =>
        cdp.eval(
          `(()=>{const b=[...document.querySelectorAll('.task-conv-item')].find((x)=>x.innerText.includes(${JSON.stringify(title)}));if(!b)return null;const c=b.querySelector('.task-item-count')?.innerText??'';return {count:Number(c.replace(/[^0-9]/g,''))||0,text:b.innerText.replace(/\\s+/g,' ').trim()};})()`,
        );
      await waitFor(
        cdp,
        `[...document.querySelectorAll('.task-conv-item')].some((x)=>x.innerText.includes(${JSON.stringify(hitTitle)}))`,
        20000,
      );
      // 任务查询在面板打开后可能仍是旧快照:等目标会话出现任务计数,最多覆盖一次 15s 轮询。
      let expectedConvReady = await waitFor(
        cdp,
        `(()=>{const b=[...document.querySelectorAll('.task-conv-item')].find((x)=>x.innerText.includes(${JSON.stringify(expectedConvTitle)}));if(!b)return false;const c=b.querySelector('.task-item-count')?.innerText??'';return (Number(c.replace(/[^0-9]/g,''))||0)>0;})()`,
        25000,
      );
      if (!expectedConvReady) {
        // 旧快照可能一直未被轮询覆盖:关闭再打开任务中心,强制新的任务查询,
        // 重新选中文档后再等会话计数(仍不能点击旧列表立即判断)。
        if (await cdp.eval(`!!document.querySelector('.task-center')`)) {
          await cdp.clickElement('[aria-label="关闭任务中心"]');
          await sleep(400);
        }
        await cdp.clickElement('[aria-label="任务中心"]');
        await sleep(900);
        await waitFor(
          cdp,
          `[...document.querySelectorAll('.task-doc-item')].some((x)=>x.innerText.includes(${JSON.stringify(mdName)}))`,
          15000,
        );
        const reopenIndex = await cdp.eval(
          `[...document.querySelectorAll('.task-doc-item')].findIndex((x)=>x.innerText.includes(${JSON.stringify(mdName)}))`,
        );
        if (reopenIndex >= 0) {
          await cdp.clickElement(`.task-doc-list > li:nth-child(${reopenIndex + 1}) .task-doc-item`);
          await sleep(600);
        }
        await waitFor(
          cdp,
          `document.querySelector('.task-doc-item.active')?.innerText.includes(${JSON.stringify(mdName)})===true`,
          10000,
        );
        expectedConvReady = await waitFor(
          cdp,
          `(()=>{const b=[...document.querySelectorAll('.task-conv-item')].find((x)=>x.innerText.includes(${JSON.stringify(expectedConvTitle)}));if(!b)return false;const c=b.querySelector('.task-item-count')?.innerText??'';return (Number(c.replace(/[^0-9]/g,''))||0)>0;})()`,
          30000,
        );
      }
      const rowSelector = `.task-row[data-task-id="${agentTask.id}"]`;
      const retryButtonSelector = `${rowSelector} button[aria-label="重试任务"]`;
      const clickConvByTitle = async (title) => {
        const point = await cdp.eval(
          `(()=>{const b=[...document.querySelectorAll('.task-conv-item')].find((x)=>x.innerText.includes(${JSON.stringify(title)}));if(!b)return null;b.scrollIntoView({block:'nearest'});const r=b.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`,
        );
        if (point === null) return false;
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: point.x, y: point.y });
        await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: point.x, y: point.y, button: "left", buttons: 1, clickCount: 1 });
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: point.x, y: point.y, button: "left", buttons: 0, clickCount: 1 });
        await sleep(300);
        return true;
      };
      let retrySelector = null;
      const convTitles = await cdp.eval(
        `[...document.querySelectorAll('.task-conv-item')].map((x)=>(x.querySelector('.task-conv-title')?.innerText??x.innerText).trim())`,
      );
      const orderedTitles = [expectedConvTitle, "未关联对话", ...(convTitles ?? [])].filter(
        (title, index, all) => typeof title === "string" && title.length > 0 && all.indexOf(title) === index,
      );
      for (const title of orderedTitles) {
        const state = await convState(title);
        if (state === null) continue;
        if (!(await clickConvByTitle(title))) continue;
        await waitFor(cdp, `!!document.querySelector(${JSON.stringify(rowSelector)})`, 8000);
        if (await cdp.eval(`!!document.querySelector(${JSON.stringify(retryButtonSelector)})`)) {
          retrySelector = retryButtonSelector;
          break;
        }
      }
      const retryAppeared = retrySelector !== null;
      let retryClosedMs = null;
      let newTaskId = null;
      if (retryAppeared) {
        const retryStart = Date.now();
        await cdp.clickElement(retrySelector);
        while (Date.now() - retryStart < 12000) {
          if (await cdp.eval(`!document.querySelector('.task-center')`)) {
            retryClosedMs = Date.now() - retryStart;
            break;
          }
          await sleep(100);
        }
        for (let attempt = 0; attempt < 30; attempt += 1) {
          const page = await cdp.invoke("task_list", { activeOnly: false, limit: 50 });
          const created = page.items.find(
            (item) =>
              item.taskType === "agent_run" &&
              item.conversationId === hitConversation.id &&
              item.id !== agentTask.id,
          );
          if (created) {
            newTaskId = created.id;
            break;
          }
          await sleep(300);
        }
      }
      record(
        "P2-3 重试后 ≤10s 关闭任务中心且新任务已创建",
        taskCanceled && retryAppeared && retryClosedMs !== null && retryClosedMs <= 10000 && newTaskId !== null,
        { agentTaskId: agentTask.id, taskCanceled, mdFound, mdDocActive, agentConvTitle, expectedConvTitle, expectedConvReady, retryAppeared, retryClosedMs, newTaskId },
      );
      if (newTaskId !== null) {
        const page = await cdp.invoke("task_list", { activeOnly: false, limit: 50 });
        const created = page.items.find((item) => item.id === newTaskId);
        if (created && ["running", "queued"].includes(created.status)) {
          await cdp
            .invoke("agent_cancel", { taskId: created.id, expectedRevision: created.stateVersion })
            .catch(() => undefined);
        }
      }
    } else {
      record("P2-3 重试后 ≤10s 关闭任务中心且新任务已创建", false, { agentTask });
    }

    // 回到滚轮合成文档,继续验证单行删除/全选/批量删除命中。
    if (await cdp.eval(`!document.querySelector('.task-center')`)) {
      await cdp.clickElement('[aria-label="任务中心"]');
      await sleep(900);
    }
    await clickDocByName(scrollDoc.name);
    await cdp.clickElement('.task-conv-item.unlinked');
    await sleep(700);

    const deletableTaskId = await cdp.eval(`(()=>{const row=[...document.querySelectorAll('.task-center-list .task-row')].find(r=>!r.querySelector('.task-select input:disabled'));return row?.dataset.taskId??null;})()`);
    if (deletableTaskId !== null) {
      const deleteSelector = `.task-row[data-task-id="${deletableTaskId}"] button[aria-label="删除任务"]`;
      const deleteHit = await cdp.hitInfo(deleteSelector);
      await cdp.clickElement(deleteSelector);
      await sleep(900);
      const stillThere = (await cdp.invoke("task_list", { activeOnly: false, limit: 50 })).items.some(
        (item) => item.id === deletableTaskId,
      );
      record("P0-2 960+dock 任务行删除真实命中并生效", deleteHit !== null && deleteHit.inside === true && !stillThere, { deletableTaskId, deleteHit, stillThere });
    } else {
      record("P0-2 960+dock 任务行删除真实命中并生效", false, { deletableTaskId });
    }

    const selectAllHit = await cdp.hitInfo('.task-bulk-select-all input');
    await cdp.clickElement('.task-bulk-select-all input');
    await sleep(350);
    const checkedCount = await cdp.eval(`document.querySelectorAll('.task-center-list .task-select input:checked').length`);
    record("P0-2 960+dock 全选真实命中", selectAllHit !== null && selectAllHit.inside === true && checkedCount > 0, { selectAllHit, checkedCount });

    const bulkHit = await cdp.hitInfo('[aria-label="批量删除任务"]');
    await cdp.clickElement('[aria-label="批量删除任务"]');
    await sleep(350);
    const bulkConfirmOpen = await cdp.eval(`!!document.querySelector('[aria-label="确认批量删除任务"]')`);
    await cdp.clickElement('.task-confirm-inline .btn.ghost');
    await sleep(250);
    record("P0-2 960+dock 批量删除入口真实命中", bulkHit !== null && bulkHit.inside === true && bulkConfirmOpen, { bulkHit, bulkConfirmOpen });
    // 还原选择状态,后续批量删除测试仍从零开始。
    await cdp.clickElement('.task-bulk-select-all input');
    await sleep(250);

    // 批量操作只作用于当前会话列表
    await resetTaskScrolls(cdp);
    let settled = false;
    const settleStarted = Date.now();
    while (Date.now() - settleStarted < 120000) {
      const page = await cdp.invoke("task_list", { activeOnly: false, limit: 50 });
      const running = page.items.some((task) => scrollTaskIds.includes(task.id) && (task.status === "running" || task.status === "queued"));
      if (!running) { settled = true; break; }
      await sleep(1500);
    }
    record("滚轮合成解析任务已完成", settled, { taskIds: scrollTaskIds.length });
    await clickDocByName(scrollDoc.name);
    await cdp.clickElement(".task-conv-item.unlinked");
    tc = await readTaskCenter(cdp);
    const currentDeletable = await cdp.eval(`[...document.querySelectorAll('.task-center-list .task-row')].filter(r=>!r.querySelector('.task-select input:disabled')).length`);
    await cdp.clickElement(".task-bulk-select-all input");
    const selectedText = await cdp.eval(`document.querySelector('.task-bulk-count')?.innerText??null`);
    record("任务中心全选只覆盖当前会话列表", currentDeletable > 0 && String(selectedText).includes(String(currentDeletable)), { rows: tc.rows, currentDeletable, selectedText });
    if (otherDocIndex >= 0) {
      if (otherDocName !== null) await clickDocByName(otherDocName);
      else await cdp.clickElement(`.task-doc-list > li:nth-child(${otherDocIndex + 1}) .task-doc-item`);
      const selectionCleared = await waitFor(
        cdp,
        `(()=>{const e=document.querySelector('.task-bulk-count');return e===null||e.innerText.includes('已选 0');})()`,
        3000,
      );
      const otherBulk = await cdp.eval(`document.querySelector('.task-bulk-count')?.innerText??document.querySelector('.task-bulk-bar')?.innerText??null`);
      record("任务中心切换文档后不保留跨列表选择", selectionCleared && (otherBulk === null || String(otherBulk).includes("已选 0")), { otherBulk, selectionCleared });
      await clickDocByName(scrollDoc.name);
      await cdp.clickElement(".task-conv-item.unlinked");
      await sleep(400);
      // 若切回后仍有残留勾选,先清空再执行“全选”,避免把全选点成反选。
      const leftover = await cdp.eval(`document.querySelectorAll('.task-center-list .task-select input:checked').length`);
      if (leftover > 0) {
        await cdp.clickElement(".task-bulk-select-all input");
        await sleep(250);
      }
      await cdp.clickElement(".task-bulk-select-all input");
    }
    const selectedCount = await cdp.eval(`document.querySelectorAll('.task-center-list .task-select input:checked').length`);
    if (selectedCount > 0) {
      await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.task-bulk-bar button')].find(x=>x.textContent.includes('批量删除'));b?.click();return true;})()`);
      await sleep(250);
      await cdp.eval(`(()=>{const b=[...document.querySelectorAll('.task-bulk-bar button')].find(x=>x.textContent.includes('确认删除'));b?.click();return true;})()`);
      await sleep(1200);
    }
    const afterBulk = await readTaskCenter(cdp);
    const afterBulkTasks = await cdp.invoke("task_list", { activeOnly: false, limit: 50 });
    const remainingDoc0 = afterBulkTasks.items.filter((task) => task.documentId === scrollDoc.documentId).length;
    const otherDocTask = scrollDocs[1]
      ? afterBulkTasks.items.some((task) => task.documentId === scrollDocs[1].documentId)
      : false;
    record(
      "任务中心批量删除只删除当前会话任务",
      selectedCount > 0 && remainingDoc0 === 0 && otherDocTask,
      { selectedCount, remainingDoc0, otherDocTask, rowsAfter: afterBulk.rows },
    );
    // —— OCR 进度面板 + 暂停/继续(4 页合成扫描件)——
    await cdp.eval(`(()=>{document.querySelector('[aria-label="关闭任务中心"]')?.click(); return true;})()`);
    const scanPath = path.join(TEMP_DIR, `e2e-scan-${STAMP}.pdf`);
    await makeScanPdf(cdp, scanPath, 10);
    scanDoc = await cdp.invoke("document_import", { path: scanPath, idempotencyKey: `e2e-scan-${STAMP}` });
    await cdp.send("Page.reload");
    await sleep(6000);
    await waitFor(cdp, `!!document.querySelector('.doc-sidebar')`, 20000);
    await ensureSidebarOpen(cdp);
    const scanItem = await cdp.eval(`(()=>{const name=${JSON.stringify(`e2e-scan-${STAMP}`)};const items=[...document.querySelectorAll('.doc-sidebar button.doc-item-main')].filter(b=>b.textContent?.trim().startsWith(name));const b=items[0];if(!b)return null;b.scrollIntoView({block:'center'});const r=b.getBoundingClientRect();return {x:r.left+30,y:r.top+r.height/2};})()`);
    if (scanItem) await cdp.drag([scanItem]);
    await waitFor(cdp, `!!document.querySelector('.reader-region .pdf-page-frame')`, 30000);
    const ocrPanel = await waitFor(cdp, `!!document.querySelector('.ocr-progress')`, 45000);
    const ocrStart = await cdp.invoke("parse_get_status", { documentId: scanDoc.documentId });
    const summaryText = await cdp.eval(`document.querySelector('.ocr-progress-summary')?.textContent?.trim()??null`);
    record("OCR 进度面板可见并显示真实进度", ocrPanel && ocrStart.parseState === "in_progress" && (summaryText ?? "").includes("已识别"), { summaryText, parseState: ocrStart.parseState });
    // 暂停/继续的完整时序与并发写入由 scripts/verify-ocr-pause.mjs 真机专项验证;
    // 这里只断言进度面板在最终 exe 上真实渲染并在运行中显示页进度。
    // 停止全文识别并清理,避免留下运行中的任务。
    const activeTasks = await cdpr.invoke("task_list", { activeOnly: true, limit: 50 });
    const ocrParseTask = activeTasks.items.find((task) => task.documentId === scanDoc.documentId && task.taskType === "parse_document");
    if (ocrParseTask) {
      await cdpr.invoke("parse_cancel", { taskId: ocrParseTask.id, expectedRevision: ocrParseTask.stateVersion }).catch(() => undefined);
    }
    await waitFor(
      cdp,
      `(async()=>{const s=await window.__TAURI_INTERNALS__.invoke("parse_get_status",{request:{documentId:${JSON.stringify(scanDoc.documentId)}}});return s.parseState!=="in_progress";})()`,
      60000,
    );
    await cdp.setWindowSize(1280, 800);
    await sleep(400);
  } finally {
    // 清理:只删除本脚本创建的会话与文档,移除 C:\Temp 合成资料。
    try {
      for (const conversationId of createdConversations) {
        await cdpr?.invoke("conversation_delete", { conversationId }).catch(() => undefined);
      }
      if (mdDoc !== null) await cdpr?.invoke("document_delete", { documentId: mdDoc.documentId }).catch(() => undefined);
      if (pdfDoc !== null) await cdpr?.invoke("document_delete", { documentId: pdfDoc.documentId }).catch(() => undefined);
      if (scanDoc !== null) await cdpr?.invoke("document_delete", { documentId: scanDoc.documentId }).catch(() => undefined);
      for (const doc of scrollDocs) {
        await cdpr?.invoke("document_delete", { documentId: doc.documentId }).catch(() => undefined);
      }
      await cdpr?.setWindowSize(1280, 800).catch(() => undefined);
    } finally {
      if (APP_PID !== null) {
        spawnSync("taskkill", ["/PID", String(APP_PID), "/T", "/F"], { stdio: "ignore" });
        await sleep(1200);
      }
      rmSync(TEMP_DIR, { recursive: true, force: true });
      rmSync(DATA_DIR, { recursive: true, force: true });
    }
    const output = {
      generatedAt: new Date().toISOString(),
      launchedApp: launched,
      summary: {
        pass: RESULTS.filter((result) => result.pass && !result.skipped).length,
        fail: RESULTS.filter((result) => !result.pass && !result.skipped).length,
        skipped: RESULTS.filter((result) => result.skipped).length,
      },
      results: RESULTS,
    };
    const outPath = path.join("C:\\Temp", `knowwise-e2e-results-${STAMP}.json`);
    writeFileSync(outPath, JSON.stringify(output, null, 2), "utf8");
    console.log(`\nRESULT_FILE ${outPath}`);
    console.log(JSON.stringify(output.summary));
    cdpr?.close();
    process.exitCode = output.summary.fail > 0 ? 1 : 0;
  }
}

await main();
