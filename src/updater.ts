// 应用更新（仅主窗口）：启动后静默检查 GitHub Releases 的 latest.json，有新版本时侧栏显示标签；
// 设置面板“关于与更新”手动检查。安装只替换 GUI：安装程序把运行中的 tetherd.exe 改名让位（见 hooks.nsh），
// 旧 daemon 继续持有会话；GUI 连接时互发 Hello，daemon 版本与 GUI 不一致时显示提示条，由用户决定何时“重启后台”。
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { ask } from "./files";
import { renderMarkdown } from "./markdown";
import { notesForLocale, notesSince } from "./update-notes";
import { locale, onLocaleChange, t } from "./i18n";

const AUTO_KEY = "tether.update.auto"; // "0" 为关闭启动时自动检查
const SKIP_KEY = "tether.update.skip"; // 用户跳过的版本号
const LAST_KEY = "tether.update.last"; // 上次自动检查的时间（毫秒）
const AUTO_DELAY = 10_000;
const AUTO_INTERVAL = 24 * 3600 * 1000;
/** 连接后等待 daemon 回 Hello 的时间；超时视为不支持 Hello 的旧版 daemon（0.1.0） */
const HELLO_TIMEOUT = 1000;

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const version = getVersion();
let found: Update | undefined;
let helloTimer: ReturnType<typeof setTimeout> | undefined;
let daemon: { version: string | null; proto: number } | undefined;
let sessionCount = () => 0;
let onBanner = () => {};
/** 设置面板中更新状态文字的当前渲染，切换语言时重绘 */
let statusText: (() => string) | undefined;
/** 更新对话框中显示的版本（切换语言时重绘标题与说明） */
let shownUpdate: Update | undefined;

function load(key: string) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {}
}

/** 主窗口初始化：count 取当前会话数（提示条文案），banner 在后台版本提示条显隐变化后调用（终端区让位） */
export async function initUpdater(count: () => number, banner: () => void) {
  sessionCount = count;
  onBanner = banner;
  renderBadgeLabel();
  onLocaleChange(() => {
    renderBadgeLabel();
    if (found) $("update-badge").textContent = t("update.badgeVersion", { version: found.version });
    if (statusText) $("update-status").textContent = statusText();
    if (daemon) renderDaemon();
    if (shownUpdate && $<HTMLDialogElement>("update-dialog").open) renderDialog(shownUpdate);
  });
  $("about-version").textContent = $("brand-ver").textContent = await version;
  const auto = $<HTMLInputElement>("update-auto");
  auto.checked = load(AUTO_KEY) !== "0";
  auto.addEventListener("change", () => store(AUTO_KEY, auto.checked ? "1" : "0"));
  $("update-badge").addEventListener("click", () => found && showDialog(found));
  $("daemon-restart").addEventListener("click", restartDaemon);
  $("daemon-restart-settings").addEventListener("click", restartDaemon);
  if (await invoke<boolean>("is_debug")) {
    $<HTMLButtonElement>("update-check").disabled = true;
    setStatus(() => t("update.devBuild"));
    return;
  }
  $("update-check").addEventListener("click", () => checkNow(true));
  setTimeout(() => {
    if (auto.checked && Date.now() - Number(load(LAST_KEY) ?? 0) > AUTO_INTERVAL) checkNow(false);
  }, AUTO_DELAY);
}

/** manual：设置面板手动检查，显示结果与错误，且不受“跳过此版本”影响；自动检查失败只写控制台（网络不通时不打扰） */
async function checkNow(manual: boolean) {
  if (manual) setStatus(() => t("update.checking"));
  try {
    const update = await check();
    store(LAST_KEY, String(Date.now()));
    found = update ?? undefined;
    $("update-badge").hidden = true;
    if (!update) {
      if (manual) setStatus(() => t("update.upToDate"));
      return;
    }
    setStatus(() => t("update.found", { version: update.version }));
    if (manual) return showDialog(update);
    if (load(SKIP_KEY) === update.version) return;
    $("update-badge").textContent = t("update.badgeVersion", { version: update.version });
    $("update-badge").hidden = false;
  } catch (err) {
    if (manual) setStatus(() => t("update.checkFailed", { error: String(err) }));
    else console.warn("Update check failed", err);
  }
}

function showDialog(update: Update) {
  const dlg = $<HTMLDialogElement>("update-dialog");
  if (dlg.open) return;
  renderDialog(update);
  $("upd-progress").hidden = true;
  for (const b of dlg.querySelectorAll("button")) b.disabled = false;
  dlg.returnValue = "";
  dlg.showModal();
  dlg.addEventListener(
    "close",
    () => {
      if (dlg.returnValue === "skip") {
        store(SKIP_KEY, update.version);
        $("update-badge").hidden = true;
      }
    },
    { once: true },
  );
  $("upd-install").onclick = () => install(update);
}

function setStatus(text: () => string) {
  statusText = text;
  $("update-status").textContent = text();
}

/** 侧栏更新标签右侧的“更新”字样（CSS ::after 取 data-label） */
function renderBadgeLabel() {
  $("update-badge").dataset.label = t("update.badge");
}

function renderDialog(update: Update) {
  shownUpdate = update;
  $("upd-title").textContent = t("update.found", { version: update.version });
  // 更新说明取自 CHANGELOG（含历次版本段，按语言分段），按界面语言选段后只显示当前版本之后的各版本，按 Markdown 渲染
  const notes = notesSince(notesForLocale(update.body?.trim() ?? "", locale()), update.currentVersion);
  $("upd-notes").innerHTML = renderMarkdown(notes || t("update.noNotes"));
}

/** 下载并安装：Windows 上安装程序启动后应用自动退出；安装期间会话由 daemon 继续持有 */
async function install(update: Update) {
  const dlg = $<HTMLDialogElement>("update-dialog");
  const bar = $<HTMLProgressElement>("upd-progress");
  for (const b of dlg.querySelectorAll("button")) b.disabled = true;
  bar.hidden = false;
  bar.removeAttribute("value");
  let total = 0;
  let done = 0;
  try {
    await update.downloadAndInstall((ev) => {
      if (ev.event === "Started") total = ev.data.contentLength ?? 0;
      else if (ev.event === "Progress") {
        done += ev.data.chunkLength;
        if (total) bar.value = done / total;
      }
    });
    await relaunch();
  } catch (err) {
    dlg.close();
    alert(t("update.failed", { error: String(err) }));
  }
}

/** 每次连接 daemon 前调用：清掉上一连接的版本信息（Hello 回复可能早于 connect 返回，故须在连接前重置） */
export function beforeConnect() {
  clearTimeout(helloTimer);
  daemon = undefined;
}

/** 连接成功后调用：GUI 已在连接时发出 Hello，尚未收到回复则等待，超时按旧版 daemon 处理 */
export function afterConnect() {
  if (!daemon) helloTimer = setTimeout(() => onHello(null, 0), HELLO_TIMEOUT);
}

export async function onHello(daemonVersion: string | null, proto: number) {
  clearTimeout(helloTimer);
  daemon = { version: daemonVersion, proto };
  const own = await version;
  const banner = $("daemon-old-banner");
  const show = daemonVersion !== own;
  renderDaemon();
  if (banner.hidden === !show) return;
  banner.hidden = !show;
  onBanner();
}

/** 设置面板的后台版本与旧版本提示条文字 */
function renderDaemon() {
  const v = daemon?.version ?? t("update.daemonOld");
  $("daemon-version").textContent = v;
  const n = sessionCount();
  $("daemon-old-text").textContent = t("update.daemonBanner", { version: v }) + (n ? t("update.daemonBannerSessions", { n }) : "");
}

export function daemonBannerShown() {
  return !$("daemon-old-banner").hidden;
}

/** 新版 daemon 收到 Shutdown 后保存会话并退出；旧版不支持，按进程 id 结束（会话列表已持续写入 sessions.json）。
 *  随后 GUI 的断线重连会拉起新的 tetherd.exe 并恢复会话 */
async function restartDaemon() {
  const n = sessionCount();
  const msg = n ? t("update.restartConfirmN", { n }) : t("update.restartConfirm");
  if ((await ask(msg, t("daemon.restart"))) === null) return;
  try {
    if ((daemon?.proto ?? 0) >= 1) await invoke("send", { msg: { type: "shutdown" } });
    else await invoke("kill_daemon");
  } catch (err) {
    alert(t("update.restartFailed", { error: String(err) }));
  }
}
