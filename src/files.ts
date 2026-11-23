// 侧边文件树：显示当前激活会话的目录树（本地磁盘 / 远程 SFTP 由 daemon 按会话区分），右键菜单操作，拖入上传
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { showPreview } from "./preview";
import { editBlock, editorLang, previewKind } from "./preview-utils";
import { icon } from "./icons";
import { clampFilesWidth } from "./term-utils";
import { claudeMentions } from "./claude-paths";

// 与 proto crate 的文件树协议保持一致
export interface FsEntry {
  name: string;
  isDir: boolean;
  isLink: boolean;
  size: number;
  mtime: number;
}

export type FsServerMsg =
  | { type: "fsEntries"; reqId: number; path: string; entries: FsEntry[] }
  | ({ type: "fsContent"; reqId: number; path: string } & FsText)
  /** fsWrite 的结果：conflict 时文件已被外部修改、未写入 */
  | { type: "fsSaved"; reqId: number; conflict: boolean; mtime: number; size: number }
  | { type: "fsResult"; reqId: number; error: string | null }
  | { type: "transfer"; reqId: number; name: string; done: number; total: number };

export type FsClientMsg =
  | { type: "fsList" | "fsMkdir" | "fsCreateFile" | "fsDelete" | "fsRead"; reqId: number; sessionId: number; path: string }
  | { type: "fsOpen"; reqId: number; sessionId: number; path: string; mode: OpenMode }
  | { type: "fsRename"; reqId: number; sessionId: number; from: string; to: string }
  /** 写回文本（按原编码与换行），mtime / size 与读取时不一致则回 fsSaved conflict，force 跳过比对 */
  | ({ type: "fsWrite"; reqId: number; sessionId: number; path: string; text: string; encoding: TextEncoding; force: boolean } & Omit<FsText, "text" | "encoding">)
  | { type: "upload"; reqId: number; sessionId: number; localPaths: string[]; destDir: string }
  | { type: "download"; reqId: number; sessionId: number; paths: string[]; localDir: string }
  | { type: "fsCopy"; reqId: number; sessionId: number; source: FsSource; items: CopyItem[]; destDir: string }
  /** SSH 会话所属连接的文件树以 root 浏览（sudo 启动 sftp-server）/ 恢复普通身份，回 fsResult */
  | { type: "fsRoot"; reqId: number; sessionId: number; enabled: boolean };

type TextEncoding = "utf8" | "utf8Bom" | "utf16Le" | "utf16Be" | "ansi";
/** 读取的文本：encoding 为 null 表示有非法字节被替换（不可写回）；crlf 为原文件换行风格；mtime / size 供写回时检测外部修改 */
interface FsText {
  text: string;
  encoding: TextEncoding | null;
  crlf: boolean;
  mtime: number;
  size: number;
}

/** 复制源位置：本地，或按主机 id（源会话关闭后，主机可连接即可粘贴） */
type FsSource = { kind: "local" } | { kind: "remote"; hostId: string };
/** keepBoth：目标已有同名项时改名为 `name (1).ext`… */
type CopyItem = { path: string; keepBoth: boolean };

/** 文件树用到的会话字段（与 SessionInfo 结构兼容） */
interface FsSession {
  id: number;
  cwd: string;
  host: string | null;
  hostId?: string;
  /** 文件树正以 root 浏览（按 SSH 连接，同一连接的会话一致） */
  fsRoot?: boolean;
}

/** default：系统默认程序；browser：默认浏览器 */
type OpenMode = "default" | "browser";

type NewTarget = { kind: "local"; cwd: string } | { kind: "remote"; hostId: string; cwd?: string };

interface Deps {
  send: (msg: FsClientMsg) => void;
  /** 当前激活会话 */
  active: () => FsSession | undefined;
  newSession: (target: NewTarget) => void;
  /** 向当前激活会话粘贴文字（不回车） */
  paste: (text: string) => void;
  /** 拖放落在文件树之外（CSS 像素坐标） */
  dropElsewhere: (x: number, y: number, paths: string[]) => void;
}

/** 每个会话各自的浏览状态 */
interface Tree {
  /** 当前浏览目录；初始为会话启动目录，本地空串表示盘符列表 */
  root: string;
  expanded: Set<string>;
  /** 目录内容，字符串为读取失败的错误信息 */
  cache: Map<string, FsEntry[] | string>;
  loading: Set<string>;
  /** 展开失败、按文件处理的远程符号链接 */
  notDir: Set<string>;
  /** 选中项（Ctrl 单击增减、Shift 单击同一目录内连选）；anchor 为最近单击的项，作连选起点与 Ctrl+V 目标 */
  selected: Set<string>;
  anchor?: string;
  /** 缓存内容对应的身份（是否 root）；身份变化时清空缓存重新读取 */
  asRoot?: boolean;
}

/** 文件树内部剪贴板：按位置 + 绝对路径记录；seq 为复制时的系统剪贴板序号，之后系统剪贴板有新的资源管理器文件则优先粘贴它们 */
interface Clip {
  source: FsSource;
  paths: string[];
  seq: number;
}

type Reply = { entries?: FsEntry[]; file?: FsText; saved?: { conflict: boolean; mtime: number; size: number }; error?: string | null };
/** [文字, 动作, 是否禁用] */
export type MenuItem = [string, () => void, boolean?] | null;
type Conflict = "overwrite" | "skip" | "keep";

const STORE_KEY = "tether.files.open";
const WIDTH_KEY = "tether.files.width";
const FOLDER_SVG = icon("folder", "fs-icon");
const FOLDER_OPEN_SVG = icon("folder-open", "fs-icon");
const FILE_SVG = icon("file", "fs-icon");
const MD_SVG = icon("file-text", "fs-icon");
const ARROW_SVG = icon("chevron-right");

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const panel = $("files");
const treeEl = $("fs-tree");
const pathEl = $("fs-path");
const upBtn = $<HTMLButtonElement>("fs-up");
const toggleBtn = $<HTMLButtonElement>("fs-toggle");
const transfersEl = $("fs-transfers");
const menu = $("fs-menu");
const dlg = $<HTMLDialogElement>("fs-dialog");
const dlgMsg = $("fs-d-msg");
const dlgInput = $<HTMLInputElement>("fs-d-input");
const dlgOk = $<HTMLButtonElement>("fs-d-ok");
const conflictDlg = $<HTMLDialogElement>("fs-conflict");
const rootBtn = $<HTMLButtonElement>("fs-root");
const rootTag = $("fs-root-tag");
const rootBanner = $("fs-root-banner");
const rootHint = $("fs-root-hint");

const trees = new Map<number, Tree>();
const waiting = new Map<number, (r: Reply) => void>();
const transfers = new Map<number, HTMLElement>();
let deps: Deps;
/** 多个窗口共用一条 daemon 连接、回复广播给所有窗口：各窗口的 reqId 起点随机错开，避免误认别的窗口的回复 */
let reqSeq = Math.floor(Math.random() * 2 ** 40);
let opened = true;
/** 上次渲染的会话，切换会话时才整体重绘 */
let shownId: number | undefined;
/** 上次渲染时该会话是否以 root 浏览，变化时也重绘 */
let shownRoot = false;
/** 终端已变为 root、待显示“同步提权”提示的会话 */
const rootHints = new Set<number>();
/** 提权 / 恢复请求进行中（可能在等 sudo 密码输入） */
let rootPending = false;
let dropEl: HTMLElement | undefined;
let clip: Clip | undefined;

export function initFiles(d: Deps) {
  deps = d;
  try {
    opened = localStorage.getItem(STORE_KEY) !== "0";
  } catch {}
  applyOpened();
  initResize();

  toggleBtn.addEventListener("click", () => {
    opened = !opened;
    try {
      localStorage.setItem(STORE_KEY, opened ? "1" : "0");
    } catch {}
    applyOpened();
  });
  upBtn.addEventListener("click", () => {
    const s = deps.active();
    if (!s) return;
    const up = parentOf(s, tree(s).root);
    if (up !== null) enter(s, up);
  });
  $("fs-refresh").addEventListener("click", () => {
    const s = deps.active();
    if (s) refreshAll(s);
  });
  rootBtn.innerHTML = icon("shield");
  rootBtn.addEventListener("click", () => {
    const s = deps.active();
    if (s) setRoot(s, !s.fsRoot);
  });
  $("fs-root-hint-ok").addEventListener("click", () => {
    const s = deps.active();
    if (s) setRoot(s, true);
  });
  $("fs-root-hint-no").addEventListener("click", () => {
    const s = deps.active();
    if (s) rootHints.delete(s.id);
    render();
  });

  treeEl.addEventListener("click", (ev) => {
    const s = deps.active();
    const row = rowOf(ev.target);
    if (!s || !row) return;
    const t = tree(s);
    const path = row.dataset.path!;
    if (ev.ctrlKey) {
      if (!t.selected.delete(path)) t.selected.add(path);
      t.anchor = path;
    } else if (ev.shiftKey && t.anchor) selectRange(s, t, t.anchor, path);
    else {
      select(t, path);
      if (row.dataset.dir) {
        if (!t.expanded.delete(path)) t.expanded.add(path);
      }
    }
    render();
  });
  // 复制 / 粘贴快捷键只在文件树获得焦点时生效，终端的 Ctrl+C / Ctrl+V 不受影响
  treeEl.addEventListener("keydown", (ev) => {
    if (!ev.ctrlKey || ev.shiftKey || ev.altKey || ev.metaKey || (ev.code !== "KeyC" && ev.code !== "KeyV")) return;
    ev.preventDefault();
    const s = deps.active();
    if (!s) return;
    const t = tree(s);
    if (ev.code === "KeyC") copySelection(s, t);
    else paste(s, dirFor(s, t, rowFor(t.anchor)));
  });
  treeEl.addEventListener("dblclick", (ev) => {
    const s = deps.active();
    const row = rowOf(ev.target);
    if (!s || !row) return;
    const path = row.dataset.path!;
    if (row.dataset.dir) enter(s, path);
    else if (previewKind(path)) preview(s, path);
    else openFile(s, path, isHtml(path) ? "browser" : "default");
  });
  panel.addEventListener("contextmenu", async (ev) => {
    ev.preventDefault();
    const s = deps.active();
    if (!s || !treeEl.contains(ev.target as Node)) return;
    const row = rowOf(ev.target);
    const t = tree(s);
    // 右键已选中的项保留多选，否则只选中该项
    if (row && !t.selected.has(row.dataset.path!)) {
      select(t, row.dataset.path!);
      render();
    }
    const pasteCount = (await pasteSource())?.paths.length ?? 0;
    showMenu(ev.clientX, ev.clientY, menuItems(s, t, row, pasteCount));
  });
  document.addEventListener("click", () => (menu.hidden = true));
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") menu.hidden = true;
  });
  window.addEventListener("blur", () => (menu.hidden = true));

  // 从资源管理器拖入：坐标为相对 webview 的物理像素，需按 devicePixelRatio 换算为 CSS 像素；落在文件树上为上传，其余交给 dropElsewhere
  getCurrentWebview().onDragDropEvent(({ payload: p }) => {
    if (p.type === "leave") return highlight(undefined);
    const [x, y] = [p.position.x / devicePixelRatio, p.position.y / devicePixelRatio];
    const target = dropTarget(x, y);
    if (p.type !== "drop") return highlight(target?.el);
    highlight(undefined);
    if (!p.paths.length) return;
    if (target) upload(target.s, target.dir, p.paths);
    else deps.dropElsewhere(x, y, p.paths);
  });
}

/** daemon 的文件树消息 */
export function onFsMessage(m: FsServerMsg) {
  if (m.type === "transfer") return showTransfer(m);
  const done = waiting.get(m.reqId);
  if (!done) return;
  waiting.delete(m.reqId);
  transfers.get(m.reqId)?.remove();
  transfers.delete(m.reqId);
  if (m.type === "fsEntries") done({ entries: m.entries });
  else if (m.type === "fsContent") done({ file: m });
  else if (m.type === "fsSaved") done({ saved: m });
  else done({ error: m.error });
}

/** 激活会话变化时调用（同一会话重复调用不重绘） */
export function showFiles() {
  const s = deps.active();
  if (s?.id === shownId && !!s?.fsRoot === shownRoot) return;
  shownId = s?.id;
  shownRoot = !!s?.fsRoot;
  render();
}

/** 终端提示符变为 root（main.ts 检测）：文件树未提权时显示“同步提权”提示，不自动切换 */
export function suggestRoot(id: number) {
  rootHints.add(id);
  if (deps.active()?.id === id) render();
}

/** 文件栏当前浏览目录：仅本地会话，盘符列表除外 */
export function filesDir(): string | undefined {
  const s = deps.active();
  if (!s || isRemote(s)) return undefined;
  return tree(s).root || undefined;
}

/** 会话关闭后丢弃其浏览状态 */
export function forgetFiles(id: number) {
  trees.delete(id);
  rootHints.delete(id);
}

/** 拖动左缘调整文件栏宽度，记住上次宽度 */
function initResize() {
  const handle = $("fs-resize");
  const apply = (w: number) => {
    const px = clampFilesWidth(w, window.innerWidth);
    panel.style.setProperty("--files-w", `${px}px`);
    return px;
  };
  try {
    const saved = Number(localStorage.getItem(WIDTH_KEY));
    if (saved) apply(saved);
  } catch {}
  handle.addEventListener("pointerdown", (ev) => {
    ev.preventDefault();
    handle.setPointerCapture(ev.pointerId);
    const startX = ev.clientX;
    const startW = panel.offsetWidth;
    let w = startW;
    const move = (e: PointerEvent) => (w = apply(startW + startX - e.clientX));
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
      try {
        localStorage.setItem(WIDTH_KEY, String(w));
      } catch {}
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  });
}

function applyOpened() {
  panel.classList.toggle("collapsed", !opened);
  toggleBtn.innerHTML = icon(opened ? "panel-right-close" : "panel-right-open");
  toggleBtn.title = opened ? "收起文件树" : "展开文件树";
  if (opened) render();
}

function tree(s: FsSession): Tree {
  let t = trees.get(s.id);
  if (!t) {
    t = { root: s.cwd, expanded: new Set(), cache: new Map(), loading: new Set(), notDir: new Set(), selected: new Set() };
    trees.set(s.id, t);
  }
  return t;
}

// ---------------- 路径：远程一律 `/`，本地 `\`；本地盘符根的上一级为空串（盘符列表） ----------------

const isRemote = (s: FsSession) => s.host !== null;

function join(s: FsSession, dir: string, name: string) {
  if (dir === "") return name;
  const sep = isRemote(s) ? "/" : "\\";
  return dir.endsWith(sep) ? dir + name : dir + sep + name;
}

function parentOf(s: FsSession, p: string): string | null {
  if (isRemote(s)) {
    const t = p.replace(/\/+$/, "");
    if (!t) return null;
    const i = t.lastIndexOf("/");
    return i <= 0 ? "/" : t.slice(0, i);
  }
  if (p === "") return null;
  if (/^[A-Za-z]:\\?$/.test(p)) return "";
  const t = p.replace(/\\+$/, "");
  const i = t.lastIndexOf("\\");
  if (i < 0) return "";
  const up = t.slice(0, i);
  return /^[A-Za-z]:$/.test(up) ? up + "\\" : up;
}

const isMarkdown = (p: string) => /\.(md|markdown)$/i.test(p);
const isHtml = (p: string) => /\.html?$/i.test(p);

function baseName(p: string) {
  return p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
}

// ---------------- 请求 ----------------

const nextId = () => ++reqSeq;

function call(msg: FsClientMsg): Promise<Reply> {
  deps.send(msg);
  return new Promise((resolve) => waiting.set(msg.reqId, resolve));
}

/** 失败时提示并返回 true */
function failed(r: Reply, what: string) {
  if (!r.error) return false;
  alert(`${what}失败：${r.error}`);
  return true;
}

/** 以 root 浏览 / 恢复普通身份；需要 sudo 密码时 daemon 弹窗询问，成功后由会话列表推送的 fsRoot 刷新界面 */
async function setRoot(s: FsSession, enabled: boolean) {
  if (rootPending) return;
  rootPending = true;
  render();
  const r = await call({ type: "fsRoot", reqId: nextId(), sessionId: s.id, enabled });
  rootPending = false;
  if (!failed(r, enabled ? "以 root 浏览" : "恢复普通身份")) rootHints.delete(s.id);
  render();
}

async function load(s: FsSession, t: Tree, dir: string, link = false) {
  if (t.loading.has(dir)) return;
  t.loading.add(dir);
  const r = await call({ type: "fsList", reqId: nextId(), sessionId: s.id, path: dir });
  t.loading.delete(dir);
  if (r.entries) t.cache.set(dir, r.entries);
  else if (link) {
    // 远程符号链接按目录列出失败：当作文件
    t.notDir.add(dir);
    t.expanded.delete(dir);
  } else t.cache.set(dir, r.error ?? "读取失败");
  if (deps.active()?.id === s.id) render();
}

/** 重新读取某目录（仅当它正显示在树中） */
function refreshDir(s: FsSession, t: Tree, dir: string) {
  if (dir === t.root || t.expanded.has(dir)) load(s, t, dir);
  else t.cache.delete(dir);
}

function refreshAll(s: FsSession) {
  const t = tree(s);
  for (const dir of [...t.cache.keys()]) if (dir !== t.root && !t.expanded.has(dir)) t.cache.delete(dir);
  t.notDir.clear();
  for (const dir of [t.root, ...t.expanded]) load(s, t, dir);
}

function enter(s: FsSession, dir: string) {
  const t = tree(s);
  t.root = dir;
  t.selected.clear();
  t.anchor = undefined;
  render();
}

function select(t: Tree, path: string) {
  t.selected = new Set([path]);
  t.anchor = path;
}

/** Shift 单击：选中同一目录列表中 from 与 to 之间的项；不在同一目录时只选中 to */
function selectRange(s: FsSession, t: Tree, from: string, to: string) {
  const dir = parentOf(s, to);
  const entries = dir === null ? undefined : t.cache.get(dir);
  if (dir === null || parentOf(s, from) !== dir || !Array.isArray(entries)) return select(t, to);
  const paths = entries.map((e) => join(s, dir, e.name));
  let [i, j] = [paths.indexOf(from), paths.indexOf(to)];
  if (i < 0 || j < 0) return select(t, to);
  if (i > j) [i, j] = [j, i];
  t.selected = new Set(paths.slice(i, j + 1));
}

// ---------------- 渲染 ----------------

function render() {
  if (!opened) return;
  const s = deps.active();
  renderRoot(s);
  if (!s) {
    pathEl.textContent = "";
    pathEl.title = "";
    upBtn.disabled = true;
    treeEl.replaceChildren(note("没有会话", 0));
    return;
  }
  const t = tree(s);
  // 身份变化（提权 / 恢复 / 连接断开）后，原缓存的目录内容与权限不再准确
  const asRoot = !!s.fsRoot;
  if (t.asRoot !== undefined && t.asRoot !== asRoot) {
    t.cache.clear();
    t.notDir.clear();
  }
  t.asRoot = asRoot;
  pathEl.textContent = t.root || "此电脑";
  pathEl.title = t.root || "此电脑";
  upBtn.disabled = parentOf(s, t.root) === null;
  const scroll = treeEl.scrollTop;
  treeEl.replaceChildren(...rows(s, t, t.root, 0));
  treeEl.scrollTop = scroll;
}

/** “以 root 浏览”按钮、root 标签 / 描边 / 警示条与“同步提权”提示：仅 SSH 会话显示 */
function renderRoot(s: FsSession | undefined) {
  const remote = !!s && isRemote(s);
  const on = remote && !!s.fsRoot;
  rootBtn.hidden = !remote;
  rootBtn.disabled = rootPending;
  rootBtn.classList.toggle("on", on);
  rootBtn.title = on ? "恢复普通身份浏览" : "以 root 浏览（sudo）";
  rootTag.hidden = !on;
  rootBanner.hidden = !on;
  panel.classList.toggle("as-root", on);
  rootHint.hidden = !remote || on || !rootHints.has(s.id);
  $<HTMLButtonElement>("fs-root-hint-ok").disabled = rootPending;
}

function rows(s: FsSession, t: Tree, dir: string, depth: number): HTMLElement[] {
  const c = t.cache.get(dir);
  if (c === undefined) {
    load(s, t, dir, depth > 0 && t.expanded.has(dir) && isLinkCandidate(s, t, dir));
    return [note("加载中…", depth)];
  }
  if (typeof c === "string") return [note(c, depth, true)];
  if (!c.length) return [note("（空）", depth)];
  const out: HTMLElement[] = [];
  for (const e of c) {
    const path = join(s, dir, e.name);
    const folder = e.isDir || (e.isLink && !t.notDir.has(path));
    const expanded = folder && t.expanded.has(path);
    out.push(row(t, e, path, depth, folder, expanded));
    if (expanded) out.push(...rows(s, t, path, depth + 1));
  }
  return out;
}

/** 该路径是否为（未确认是目录的）远程符号链接 */
function isLinkCandidate(s: FsSession, t: Tree, path: string) {
  const up = parentOf(s, path);
  const entries = up === null ? undefined : t.cache.get(up);
  const e = Array.isArray(entries) ? entries.find((x) => join(s, up!, x.name) === path) : undefined;
  return !!e && e.isLink && !e.isDir;
}

function row(t: Tree, e: FsEntry, path: string, depth: number, folder: boolean, expanded: boolean) {
  const el = document.createElement("div");
  el.className = "fs-row";
  el.classList.toggle("selected", t.selected.has(path));
  el.classList.toggle("link", e.isLink);
  el.dataset.path = path;
  el.dataset.dir = folder ? "1" : "";
  el.style.paddingLeft = `${4 + depth * 14}px`;
  el.classList.toggle("open", folder && expanded);
  el.classList.toggle("md", !folder && isMarkdown(e.name));
  const ico = folder ? (expanded ? FOLDER_OPEN_SVG : FOLDER_SVG) : isMarkdown(e.name) ? MD_SVG : FILE_SVG;
  el.innerHTML = `<span class="fs-arrow">${folder ? ARROW_SVG : ""}</span>${ico}<span class="fs-name"></span>`;
  el.querySelector(".fs-name")!.textContent = e.name;
  const info = [e.name];
  if (!folder) info.push(fmtSize(e.size));
  if (e.mtime) info.push(new Date(e.mtime).toLocaleString());
  el.title = info.join("\n");
  return el;
}

function note(text: string, depth: number, error = false) {
  const el = document.createElement("div");
  el.className = error ? "fs-note error" : "fs-note";
  el.style.paddingLeft = `${18 + depth * 14}px`;
  el.textContent = text;
  return el;
}

function rowOf(target: EventTarget | null) {
  return (target as Element | null)?.closest<HTMLElement>(".fs-row") ?? undefined;
}

/** 当前显示的某路径的行 */
function rowFor(path: string | undefined) {
  if (path === undefined) return undefined;
  return [...treeEl.querySelectorAll<HTMLElement>(".fs-row")].find((r) => r.dataset.path === path);
}

function fmtSize(n: number) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${i ? n.toFixed(1) : n} ${units[i]}`;
}

// ---------------- 右键菜单 ----------------

function menuItems(s: FsSession, t: Tree, row: HTMLElement | undefined, pasteCount: number): MenuItem[] {
  const copy = (p: string): MenuItem => ["复制路径", () => navigator.clipboard.writeText(p).catch(() => {})];
  const here = (p: string): MenuItem => ["在此目录新建会话", () => newSessionAt(s, p)];
  const pasteItem: MenuItem = [pasteCount ? `粘贴（${pasteCount} 项）` : "粘贴", () => paste(s, dirFor(s, t, row)), !pasteCount];
  const n = t.selected.size;
  const copyItem: MenuItem = [n > 1 ? `复制（${n} 项）` : "复制", () => copySelection(s, t)];
  // 盘符列表：只能进入 / 在盘符根新建会话
  if (t.root === "") {
    if (!row) return [["刷新", () => refreshAll(s)]];
    return [here(row.dataset.path!), copy(row.dataset.path!)];
  }
  if (!row) {
    return [
      ["新建文件夹", () => create(s, t, t.root, false)],
      ["新建文件", () => create(s, t, t.root, true)],
      null,
      pasteItem,
      null,
      here(t.root),
      null,
      copy(t.root),
      ["刷新", () => refreshAll(s)],
    ];
  }
  const path = row.dataset.path!;
  const common: MenuItem[] = [
    [n > 1 ? `发给 Claude（${n} 项）` : "发给 Claude", () => sendToClaude(s, t)],
    ["下载到…", () => download(s, path)],
    null,
    copyItem,
    pasteItem,
    null,
    ["重命名", () => rename(s, t, path)],
    ["删除", () => remove(s, t, path, !!row.dataset.dir)],
    null,
    copy(path),
  ];
  if (!row.dataset.dir) {
    const kind = previewKind(path);
    const asText: MenuItem = ["以文本打开", () => preview(s, path, "text")];
    const open: MenuItem[] =
      kind === "md"
        ? [["预览", () => preview(s, path)], asText, ["用默认程序打开", () => openFile(s, path)]]
        : kind === "text"
          ? [["编辑", () => preview(s, path)], ["用默认程序打开", () => openFile(s, path)]]
          : isHtml(path)
            ? [["在浏览器中打开", () => openFile(s, path, "browser")], ["用默认程序打开", () => openFile(s, path)], asText]
            : [["打开", () => openFile(s, path)], asText];
    return [...open, ...common];
  }
  return [
    ["新建文件夹", () => create(s, t, path, false)],
    ["新建文件", () => create(s, t, path, true)],
    null,
    here(path),
    ...common,
    ["刷新", () => refreshDir(s, t, path)],
  ];
}

export function showMenu(x: number, y: number, items: MenuItem[]) {
  menu.replaceChildren(
    ...items.map((it) => {
      if (!it) return document.createElement("hr");
      const b = document.createElement("button");
      b.textContent = it[0];
      b.disabled = !!it[2];
      if (it[0] === "删除") b.className = "danger";
      b.addEventListener("click", () => {
        menu.hidden = true;
        it[1]();
      });
      return b;
    }),
  );
  menu.hidden = false;
  // 贴边时向内收
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, innerWidth - r.width - 4)}px`;
  menu.style.top = `${Math.min(y, innerHeight - r.height - 4)}px`;
}

// ---------------- 对话框（输入名称 / 确认删除） ----------------

/** input 为 undefined 时是确认框；确定返回输入值（确认框为 ""），取消返回 null */
export function ask(message: string, ok: string, input?: string): Promise<string | null> {
  dlgMsg.textContent = message;
  dlgOk.textContent = ok;
  dlgOk.classList.toggle("danger", ok === "删除");
  dlgInput.hidden = input === undefined;
  dlgInput.value = input ?? "";
  dlg.returnValue = "";
  dlg.showModal();
  if (input !== undefined) {
    dlgInput.focus();
    // 默认选中不含扩展名的部分
    const dot = input.lastIndexOf(".");
    dlgInput.setSelectionRange(0, dot > 0 ? dot : input.length);
  }
  return new Promise((resolve) =>
    dlg.addEventListener("close", () => resolve(dlg.returnValue === "ok" ? dlgInput.value.trim() : null), { once: true }),
  );
}

/** 读名称：空或含路径分隔符时返回 null */
async function askName(message: string, initial: string) {
  const name = await ask(message, "确定", initial);
  if (!name) return null;
  if (/[\\/]/.test(name)) {
    alert("名称不能包含 / 或 \\");
    return null;
  }
  return name;
}

// ---------------- 操作 ----------------

async function create(s: FsSession, t: Tree, dir: string, file: boolean) {
  const name = await askName(file ? "新建文件" : "新建文件夹", "");
  if (!name) return;
  const path = join(s, dir, name);
  const r = await call({ type: file ? "fsCreateFile" : "fsMkdir", reqId: nextId(), sessionId: s.id, path });
  if (failed(r, file ? "新建文件" : "新建文件夹")) return;
  if (dir !== t.root) t.expanded.add(dir);
  select(t, path);
  refreshDir(s, t, dir);
}

async function rename(s: FsSession, t: Tree, path: string) {
  const old = baseName(path);
  const name = await askName(`重命名“${old}”`, old);
  const dir = parentOf(s, path);
  if (!name || name === old || dir === null) return;
  const to = join(s, dir, name);
  const r = await call({ type: "fsRename", reqId: nextId(), sessionId: s.id, from: path, to });
  if (failed(r, "重命名")) return;
  if (t.expanded.delete(path)) t.expanded.add(to);
  select(t, to);
  refreshDir(s, t, dir);
}

async function remove(s: FsSession, t: Tree, path: string, folder: boolean) {
  const name = baseName(path);
  const msg = folder ? `确定删除文件夹“${name}”及其全部内容？此操作不可撤销。` : `确定删除“${name}”？此操作不可撤销。`;
  if ((await ask(msg, "删除")) === null) return;
  const r = await call({ type: "fsDelete", reqId: nextId(), sessionId: s.id, path });
  if (failed(r, "删除")) return;
  t.expanded.delete(path);
  const dir = parentOf(s, path);
  if (dir !== null) refreshDir(s, t, dir);
}

/** 本地直接用默认程序 / 默认浏览器打开；远程先下载到临时目录（显示传输进度）再打开 */
async function openFile(s: FsSession, path: string, mode: OpenMode = "default") {
  const reqId = nextId();
  if (isRemote(s)) startTransfer(reqId, `打开 ${baseName(path)}`);
  failed(await call({ type: "fsOpen", reqId, sessionId: s.id, path, mode }), "打开");
}

/** 终端输出中的文件路径链接：同 openFile，但失败（如文件不存在）静默忽略 */
export function openPathQuiet(s: FsSession, path: string) {
  const reqId = nextId();
  if (isRemote(s)) startTransfer(reqId, `打开 ${baseName(path)}`);
  call({ type: "fsOpen", reqId, sessionId: s.id, path, mode: "default" });
}

/** 读取文本并在应用内预览 / 编辑（kind 默认按文件类型，“以文本打开”强制 text）；读取期间已切换会话则不再显示。
 *  编码、换行、mtime 留在闭包里，保存时按原样带回 daemon，保存成功后更新 mtime / size */
async function preview(s: FsSession, path: string, kind = previewKind(path) ?? "text", edit?: boolean) {
  const r = await call({ type: "fsRead", reqId: nextId(), sessionId: s.id, path });
  if (failed(r, "打开") || !r.file || deps.active()?.id !== s.id) return;
  let meta = r.file;
  showPreview(
    {
      path,
      text: meta.text,
      kind,
      lang: editorLang(path),
      readonly: editBlock(meta.size, meta.encoding),
      openDefault: () => openFile(s, path),
      save: async (text, force) => {
        const { encoding, crlf, mtime, size } = meta;
        const w = await call({ type: "fsWrite", reqId: nextId(), sessionId: s.id, path, text, encoding: encoding!, crlf, mtime, size, force });
        if (w.error) return { error: w.error };
        if (w.saved?.conflict) return { conflict: true };
        meta = { ...meta, mtime: w.saved!.mtime, size: w.saved!.size };
        return {};
      },
      reload: (edit) => preview(s, path, kind, edit),
    },
    edit,
  );
}

async function download(s: FsSession, path: string) {
  const dir = await open({ directory: true, title: "下载到…" });
  if (typeof dir !== "string") return;
  const reqId = nextId();
  startTransfer(reqId, `下载 ${baseName(path)}`);
  failed(await call({ type: "download", reqId, sessionId: s.id, paths: [path], localDir: dir }), "下载");
}

async function upload(s: FsSession, dir: string, paths: string[]) {
  const reqId = nextId();
  startTransfer(reqId, paths.length === 1 ? `上传 ${baseName(paths[0])}` : `上传 ${paths.length} 项`);
  const r = await call({ type: "upload", reqId, sessionId: s.id, localPaths: paths, destDir: dir });
  if (failed(r, "上传")) return;
  const t = tree(s);
  if (dir !== t.root) t.expanded.add(dir);
  refreshDir(s, t, dir);
}

/** 把显示中的选中项（按显示顺序）以 @ 引用粘贴到当前会话；远程会话用远程路径 */
function sendToClaude(s: FsSession, t: Tree) {
  const paths = [...treeEl.querySelectorAll<HTMLElement>(".fs-row")].map((r) => r.dataset.path!).filter((p) => t.selected.has(p));
  if (paths.length) deps.paste(claudeMentions(paths, s.cwd, isRemote(s)));
}

function newSessionAt(s: FsSession, dir: string) {
  if (!isRemote(s)) return deps.newSession({ kind: "local", cwd: dir });
  if (s.hostId) deps.newSession({ kind: "remote", hostId: s.hostId, cwd: dir });
}

// ---------------- 复制 / 粘贴 ----------------

/** 复制当前显示中的选中项到内部剪贴板，同时把文件名（多项按行分隔）写入系统剪贴板，可直接粘贴到终端；盘符不能复制 */
async function copySelection(s: FsSession, t: Tree) {
  if (t.root === "" || (isRemote(s) && !s.hostId)) return;
  const shown = new Set([...treeEl.querySelectorAll<HTMLElement>(".fs-row")].map((r) => r.dataset.path));
  const paths = [...t.selected].filter((p) => shown.has(p));
  if (!paths.length) return;
  const source: FsSource = isRemote(s) ? { kind: "remote", hostId: s.hostId! } : { kind: "local" };
  await writeText(paths.map(baseName).join("\n")).catch(() => {});
  // 写入文件名后再取序号：之后系统剪贴板若再变化（如资源管理器复制文件），粘贴时以系统剪贴板为准
  const seq = await invoke<number>("clipboard_seq").catch(() => 0);
  clip = { source, paths, seq };
}

/** 待粘贴的内容：系统剪贴板中有资源管理器复制的文件，且内部剪贴板为空或其后系统剪贴板变化过 → 这些本地文件；否则内部剪贴板 */
async function pasteSource(): Promise<{ source: FsSource; paths: string[] } | undefined> {
  const [seq, files] = await invoke<[number, string[]]>("clipboard_files").catch((): [number, string[]] => [0, []]);
  if (files.length && (!clip || seq !== clip.seq)) return { source: { kind: "local" }, paths: files };
  return clip;
}

/** 源与目标会话是否在同一位置（同为本地，或同一主机） */
function sameLocation(src: FsSource, s: FsSession) {
  return src.kind === "local" ? !isRemote(s) : s.hostId === src.hostId;
}

/** 粘贴到会话 s 的目录 dir：同名项逐个询问；粘贴回源所在目录时自动“都保留” */
async function paste(s: FsSession, dir: string) {
  if (dir === "") return;
  const src = await pasteSource();
  if (!src) return;
  const listing = await call({ type: "fsList", reqId: nextId(), sessionId: s.id, path: dir });
  if (failed(listing, "粘贴")) return;
  // 本地不区分大小写
  const fold = (n: string) => (isRemote(s) ? n : n.toLowerCase());
  const existing = new Map((listing.entries ?? []).map((e) => [fold(e.name), e]));
  const same = sameLocation(src.source, s);
  const plan = src.paths.map((path) => {
    const up = same ? parentOf(s, path) : null;
    if (up !== null && fold(up) === fold(dir)) return { path, auto: true, hit: undefined };
    return { path, auto: false, hit: existing.get(fold(baseName(path))) };
  });
  let left = plan.filter((p) => p.hit).length;
  let all: Conflict | undefined;
  const items: CopyItem[] = [];
  for (const p of plan) {
    if (p.auto || !p.hit) {
      items.push({ path: p.path, keepBoth: p.auto });
      continue;
    }
    left--;
    let choice = all;
    if (!choice) {
      const r = await askConflict(baseName(p.path), p.hit.isDir, left);
      if (!r) return;
      choice = r.choice;
      if (r.all) all = choice;
    }
    if (choice !== "skip") items.push({ path: p.path, keepBoth: choice === "keep" });
  }
  if (!items.length) return;
  const reqId = nextId();
  startTransfer(reqId, items.length === 1 ? `粘贴 ${baseName(items[0].path)}` : `粘贴 ${items.length} 项`);
  const r = await call({ type: "fsCopy", reqId, sessionId: s.id, source: src.source, items, destDir: dir });
  failed(r, "粘贴");
  // 失败时也刷新：可能已复制了一部分
  const t = tree(s);
  if (dir !== t.root) t.expanded.add(dir);
  refreshDir(s, t, dir);
}

/** 同名冲突对话框：返回选择与是否应用到其余 left 个同名项；Esc 取消整个粘贴返回 null */
function askConflict(name: string, isDir: boolean, left: number): Promise<{ choice: Conflict; all: boolean } | null> {
  $("fs-c-msg").textContent = `目标位置已存在${isDir ? "文件夹" : "文件"}“${name}”。`;
  $("fs-c-hint").textContent = isDir
    ? "覆盖：合并并覆盖同名文件（文件夹内的同名文件不再逐个询问），其余文件保留。都保留：复制为“名称 (1)”。Esc 取消粘贴。"
    : "都保留：复制为“名称 (1).扩展名”。Esc 取消粘贴。";
  const allBox = $<HTMLInputElement>("fs-c-all");
  allBox.checked = false;
  $("fs-c-all-row").hidden = left === 0;
  $("fs-c-all-text").textContent = `对其余 ${left} 个同名项应用相同操作`;
  conflictDlg.returnValue = "";
  conflictDlg.showModal();
  return new Promise((resolve) =>
    conflictDlg.addEventListener(
      "close",
      () => {
        const v = conflictDlg.returnValue;
        resolve(v === "overwrite" || v === "skip" || v === "keep" ? { choice: v, all: allBox.checked } : null);
      },
      { once: true },
    ),
  );
}

// ---------------- 拖入上传 ----------------

/** 拖入 / 粘贴的目标目录：文件夹行 → 该文件夹；文件行 → 其所在文件夹；没有行（空白处）→ 当前浏览目录 */
function dirFor(s: FsSession, t: Tree, row: HTMLElement | undefined) {
  if (!row) return t.root;
  return row.dataset.dir ? row.dataset.path! : (parentOf(s, row.dataset.path!) ?? t.root);
}

/** 鼠标所在的目标目录 */
function dropTarget(x: number, y: number) {
  const s = deps.active();
  const hit = document.elementFromPoint(x, y);
  if (!s || !opened || !hit || !treeEl.contains(hit)) return undefined;
  const t = tree(s);
  const dir = dirFor(s, t, rowOf(hit));
  // 盘符列表不能作为上传目标
  if (dir === "") return undefined;
  const el = dir === t.root ? treeEl : [...treeEl.querySelectorAll<HTMLElement>(".fs-row")].find((r) => r.dataset.path === dir);
  return { s, dir, el: el ?? treeEl };
}

function highlight(el: HTMLElement | undefined) {
  if (dropEl === el) return;
  dropEl?.classList.remove("drop");
  el?.classList.add("drop");
  dropEl = el;
}

// ---------------- 传输进度 ----------------

function startTransfer(reqId: number, label: string) {
  const el = document.createElement("div");
  el.className = "fs-transfer";
  el.innerHTML = `<div class="fs-t-line"><span class="fs-t-name"></span><span class="fs-t-pct">准备中…</span></div><div class="fs-t-bar"><i></i></div>`;
  el.querySelector(".fs-t-name")!.textContent = label;
  el.title = label;
  transfersEl.appendChild(el);
  transfers.set(reqId, el);
}

function showTransfer(m: { reqId: number; name: string; done: number; total: number }) {
  const el = transfers.get(m.reqId);
  if (!el) return;
  if (m.name) el.querySelector(".fs-t-name")!.textContent = m.name;
  const pct = m.total ? Math.floor((m.done * 100) / m.total) : 100;
  el.querySelector(".fs-t-pct")!.textContent = `${pct}%  ${fmtSize(m.done)} / ${fmtSize(m.total)}`;
  el.querySelector<HTMLElement>(".fs-t-bar i")!.style.width = `${pct}%`;
}
