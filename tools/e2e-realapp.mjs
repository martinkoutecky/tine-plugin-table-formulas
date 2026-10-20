// Real-app journey for the Table formulas plugin (tauri-driver + WebKitWebDriver).
// Pattern copied from tine-og/scripts/e2e-plugin-graph-ownership.mjs; nothing in tine-og is edited.
//
//   TINE_APP=<tine binary> TINE_OG=<a Tine checkout> \
//   [E2E_TMP_DIR=<missing or empty dir>] xvfb-run -a -s "-screen 0 1920x1080x24" node tools/e2e-realapp.mjs
//
// Run it under `bwrap --dev-bind / / --bind <scratch> $HOME` so a stray write can never reach real app data.
// All state (graph, XDG dirs) lives under E2E_TMP_DIR; the plugin is seeded into the app-data dir exactly as
// the ownership journey does, so no signed registry is needed.
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN = path.resolve(HERE, "..");
const OG = process.env.TINE_OG;
if (!OG) throw new Error("set TINE_OG to a Tine checkout");
const APP = process.env.TINE_APP;
if (!APP) throw new Error("set TINE_APP to the tine binary");
const req = createRequire(path.join(OG, "package.json"));
const { remote } = req("webdriverio");
const { APP_ID } = await import(path.join(OG, "scripts/lib/app-identity.mjs"));
const TD = process.env.TAURI_DRIVER || "tauri-driver";
const WD = process.env.WEBKIT_DRIVER || "/usr/bin/WebKitWebDriver";
const DRIVER_PORT = Number(process.env.E2E_DRIVER_PORT || 4510);
const NATIVE_PORT = Number(process.env.E2E_NATIVE_PORT || 4511);
// The state directory: a fresh one under the system temp dir, or E2E_TMP_DIR,
// which must be missing or empty. Nothing is ever deleted recursively: a
// configured path could hold anything.
function stateDir() {
  const dir = process.env.E2E_TMP_DIR;
  if (!dir) return fs.mkdtempSync(path.join(os.tmpdir(), "tine-table-formulas-e2e-"));
  if (fs.existsSync(dir) && fs.readdirSync(dir).length > 0) {
    throw new Error(`E2E_TMP_DIR ${dir} is not empty; give a missing or empty directory`);
  }
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}
const TMP = stateDir();
console.log(`state in ${TMP}`);
const GRAPH = path.join(TMP, "graph");
const XDG = path.join(TMP, "xdg");

const manifest = JSON.parse(fs.readFileSync(path.join(PLUGIN, "manifest.json"), "utf8"));
const wasm = fs.readFileSync(path.join(PLUGIN, manifest.entry));

const MD_SRC = [
  "- Prices",
  "  | item | qty | price | total |",
  "  |------|-----|-------|-------|",
  "  | a    | 2   | 3     |       |",
  "  | b    | 4   | 0.5   |       |",
  "  <!-- TBLFM: $4=$2*$3 -->",
  "",
].join("\n");
const MD_OUT = [
  "- Prices",
  "  | item | qty | price | total |",
  "  | ---- | --- | ----- | ----- |",
  "  | a    |   2 |     3 |     6 |",
  "  | b    |   4 |   0.5 |    2. |",
  "  <!-- TBLFM: $4=$2*$3 -->",
  "",
].join("\n");
const ORG_SRC = [
  "* Prices",
  "| item | qty | price | total |",
  "|------+-----+-------+-------|",
  "| a    | 2   | 3     |       |",
  "| b    | 4   | 0.5   |       |",
  "#+TBLFM: $4=$2*$3",
  "",
].join("\n");

for (const d of ["pages", "journals", "logseq"]) fs.mkdirSync(path.join(GRAPH, d), { recursive: true });
fs.writeFileSync(path.join(GRAPH, "logseq", "config.edn"), "{}\n");
fs.writeFileSync(path.join(GRAPH, "pages", "Md Table.md"), MD_SRC);
fs.writeFileSync(path.join(GRAPH, "pages", "Org Table.org"), ORG_SRC);
const now = new Date();
const jname = `${now.getFullYear()}_${String(now.getMonth() + 1).padStart(2, "0")}_${String(now.getDate()).padStart(2, "0")}`;
fs.writeFileSync(path.join(GRAPH, "journals", `${jname}.md`), "- [[Md Table]] [[Org Table]]\n");
for (const d of ["data", "config", "cache"]) fs.mkdirSync(path.join(XDG, d), { recursive: true });
const appData = path.join(XDG, "data", APP_ID);
const pkg = path.join(appData, "plugins", manifest.id, manifest.version);
fs.mkdirSync(pkg, { recursive: true });
// The repo manifest points `entry` at the cargo output; the installed package carries plugin.wasm.
fs.writeFileSync(path.join(pkg, "manifest.json"), `${JSON.stringify({ ...manifest, entry: "plugin.wasm" }, null, 2)}\n`);
fs.writeFileSync(path.join(pkg, "plugin.wasm"), wasm);
fs.writeFileSync(path.join(appData, "tine-settings.json"), `${JSON.stringify({
  known_graphs: [{ name: "graph", path: GRAPH }],
  last_graph_path: GRAPH,
  plugin_states: { [manifest.id]: { version: manifest.version, enabled: true } },
}, null, 2)}\n`);

const env = {
  ...process.env,
  TINE_GRAPH: GRAPH,
  XDG_DATA_HOME: path.join(XDG, "data"),
  XDG_CONFIG_HOME: path.join(XDG, "config"),
  XDG_CACHE_HOME: path.join(XDG, "cache"),
  WEBKIT_DISABLE_DMABUF_RENDERER: "1",
  WEBKIT_DISABLE_COMPOSITING_MODE: "1",
  LIBGL_ALWAYS_SOFTWARE: "1",
  GDK_BACKEND: "x11",
};
const log = fs.openSync(path.join(TMP, "tauri-driver.log"), "w");
const driver = spawn(TD, ["--port", String(DRIVER_PORT), "--native-port", String(NATIVE_PORT), "--native-driver", WD], {
  env, stdio: ["ignore", log, log], detached: true,
});
await sleep(2500);

const report = {};
let browser;
const read = (f) => fs.readFileSync(path.join(GRAPH, "pages", f), "utf8");
async function waitFile(f, want, label, timeout = 10_000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeout) {
    if (read(f) === want) return;
    await sleep(150);
  }
  throw new Error(`${label}: ${f} is\n${read(f)}\nwanted\n${want}`);
}
async function openPage(name) {
  // Open through search/switcher the way a user does.
  await browser.keys(["Control", "k"]);
  await browser.$(".switcher-input").waitForExist({ timeout: 5_000 });
  await browser.$(".switcher-input").setValue(name);
  await sleep(700);
  await browser.keys(["Enter"]);
  await browser.$(`.page-title`).waitForExist({ timeout: 10_000 });
  await browser.waitUntil(async () => (await browser.$(".page-title").getText()).toLowerCase().includes(name.toLowerCase()), { timeout: 10_000 });
}
async function editFirstBlock() {
  const block = await browser.$(".block-content");
  await block.waitForExist({ timeout: 15_000 });
  await block.click();
  await browser.$("textarea.block-editor").waitForExist({ timeout: 5_000 });
}
async function visibleText() {
  return browser.execute(() => document.querySelector(".page-blocks, .page")?.innerText ?? document.body.innerText);
}
async function renderedTable() {
  return browser.execute(() => {
    const t = document.querySelector(".block-content table");
    return t ? [...t.querySelectorAll("tr")].map((r) => [...r.children].map((c) => c.textContent.trim())) : null;
  });
}
async function exerciseFormat(label, file, src, pageName) {
  await openPage(pageName);
  // 1. rendering before: formula line must not show.
  await browser.$(".block-content").waitForExist({ timeout: 15_000 });
  const before = await visibleText();
  const tableBefore = await renderedTable();
  const formulaVisible = /TBLFM/.test(before);
  // 2. recalc via real chord inside the editor.
  await editFirstBlock();
  await browser.keys(["Control", "Alt", "r"]);
  let out;
  try {
    await waitFile(file, label === "markdown" ? MD_OUT : ORG_OUT_PLACEHOLDER(), `${label} recalc`);
  } catch (e) {
    report[`${label}_error`] = String(e.message).slice(0, 800);
    throw e;
  }
  out = read(file);
  await sleep(500);
  // 3. one undo restores the original (editor focused, Ctrl+Z).
  await browser.keys(["Escape"]);
  await sleep(400);
  await browser.keys(["Control", "z"]);
  let undone = false;
  try { await waitFile(file, src, `${label} undo`, 6_000); undone = true; } catch (e) { report[`${label}_undo_error`] = String(e.message).slice(0, 600); }
  const after = await visibleText();
  // After the recalculation, leave editing and inspect what the rendered block holds (redo is not used).
  await browser.keys(["Control", "Shift", "z"]);
  await sleep(600);
  await browser.keys(["Escape"]);
  await sleep(400);
  const dom = await browser.execute(() => {
    const c = document.querySelector(".block-content");
    return c ? { html: c.innerHTML.replace(/\s+/g, " ").slice(0, 1500), children: [...c.children].map((e) => `${e.tagName}.${e.className}:${e.textContent.trim().length}`) } : null;
  });
  report[`${label}_dom_after_redo`] = dom;
  report[`${label}_file_after_redo`] = read(file);
  Object.assign(report, {
    [`${label}_formula_line_visible_before`]: formulaVisible,
    [`${label}_table_before`]: tableBefore,
    [`${label}_file_after_recalc`]: out,
    [`${label}_undo_restored_file`]: undone,
    [`${label}_formula_visible_after_undo`]: /TBLFM/.test(after),
  });
}
// The Org expected output is filled in from the first run's oracle; see ORG_OUT below.
const ORG_OUT = process.env.ORG_OUT_FILE ? fs.readFileSync(process.env.ORG_OUT_FILE, "utf8") : null;
function ORG_OUT_PLACEHOLDER() {
  return ORG_OUT ?? [
    "* Prices",
    "| item | qty | price | total |",
    "|------+-----+-------+-------|",
    "| a    |   2 |     3 |     6 |",
    "| b    |   4 |   0.5 |    2. |",
    "#+TBLFM: $4=$2*$3",
    "",
  ].join("\n");
}

try {
  browser = await remote({
    hostname: "127.0.0.1", port: DRIVER_PORT, path: "/", logLevel: "error",
    connectionRetryCount: 1, connectionRetryTimeout: 60_000,
    capabilities: { browserName: "wry", "wdio:enforceWebDriverClassic": true, "tauri:options": { application: APP } },
  });
  await browser.$(".page-title, .journal-page, .block-content").waitForExist({ timeout: 30_000 });
  // plugin loaded? the command must be in the palette.
  await browser.keys(["Control", "Shift", "p"]);
  await browser.$(".switcher-input").waitForExist({ timeout: 5_000 });
  await browser.$(".switcher-input").setValue("Table: recalc");
  await sleep(600);
  report.palette_text = (await browser.$(".switcher").getText()).slice(0, 300);
  report.palette_html = (await browser.$(".switcher").getHTML()).replace(/\s+/g, " ").slice(0, 900);
  await browser.keys(["Escape"]);
  await sleep(300);
  await exerciseFormat("markdown", "Md Table.md", MD_SRC, "Md Table");
  await exerciseFormat("org", "Org Table.org", ORG_SRC, "Org Table");
  console.log(JSON.stringify(report, null, 2));
} catch (e) {
  console.log(JSON.stringify(report, null, 2));
  try { await browser?.saveScreenshot(path.join(TMP, "failure.png")); } catch {}
  console.error(e);
  process.exitCode = 1;
} finally {
  try { await browser?.deleteSession(); } catch {}
  try { process.kill(-driver.pid, "SIGKILL"); } catch {}
  fs.closeSync(log);
}
