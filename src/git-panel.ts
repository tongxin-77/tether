// 卡片头分支的菜单（切换分支 / 查看历史）与提交历史面板；git 命令由后端执行，输出解析见 git-utils.ts
import { invoke } from "@tauri-apps/api/core";
import { icon } from "./icons";
import { requestClosePreview } from "./preview";
import { type Commit, type Refs, classifyDiff, filterRefs, graphRuns, parseDecor, parseLog, parseNumstat, parseRefs, relTime, switchTarget } from "./git-utils";

interface Deps {
  /** 激活会话查分支的目录（仅本地会话），没有时为 null */
  dir: () => string | null;
  /** 切换分支成功后（刷新卡片头分支） */
  onSwitched: () => void;
  /** 复制文本并提示 */
  copy: (text: string) => void;
  /** 面板关闭后（重新 fit 并聚焦当前终端） */
  onClose: () => void;
}

/** 历史列表的提交数与 diff 显示的行数上限 */
const LOG_LIMIT = 200;
const DIFF_LINES = 3000;

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const branchEl = $("ch-branch");
const menu = $("git-menu");
const panel = $("git-panel");
const listEl = $("gp-list");
const detailEl = $("gp-detail");

let deps: Deps;
/** 菜单打开时的目录与分支列表 */
let menuDir = "";
let refs: Refs = { current: null, locals: [], remotes: [] };
/** 面板对应的目录；异步结果按序号丢弃过期的 */
let panelDir = "";
let loadSeq = 0;
let showSeq = 0;

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", text = "") => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text) e.textContent = text;
  return e;
};
const errText = (e: unknown) => (typeof e === "string" ? e : String(e));

export function initGitPanel(d: Deps) {
  deps = d;
  branchEl.title = "切换分支 / 查看历史";
  branchEl.addEventListener("click", () => (menu.hidden ? openMenu() : (menu.hidden = true)));
  $("gp-ico").innerHTML = icon("history");
  $("gp-close").addEventListener("click", closeGitPanel);
  document.addEventListener("mousedown", (ev) => {
    const t = ev.target as Node;
    if (!menu.hidden && !menu.contains(t) && !branchEl.contains(t)) menu.hidden = true;
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key !== "Escape" || document.querySelector("dialog[open]")) return;
    if (!menu.hidden) menu.hidden = true;
    else if (!panel.hidden) closeGitPanel();
  });
}

// ---------------- 分支菜单 ----------------

async function openMenu() {
  const dir = deps.dir();
  if (!dir) return;
  menuDir = dir;
  const filter = el("input", "gm-filter");
  filter.placeholder = "过滤分支…";
  const history = el("button", "gm-item");
  history.innerHTML = icon("history");
  history.append("查看历史");
  history.addEventListener("click", () => {
    menu.hidden = true;
    void openHistory(dir);
  });
  const list = el("div", "gm-list", "正在读取分支…");
  const status = el("pre", "gm-status");
  status.hidden = true;
  menu.replaceChildren(filter, history, list, status);
  menu.classList.remove("busy");
  menu.hidden = false;
  const r = branchEl.getBoundingClientRect();
  menu.style.left = `${Math.max(4, Math.min(r.left - 8, innerWidth - menu.offsetWidth - 8))}px`;
  menu.style.top = `${r.bottom + 6}px`;
  filter.focus();
  try {
    refs = parseRefs(await invoke<string>("git_refs", { dir }));
  } catch (e) {
    list.textContent = "";
    showStatus(status, errText(e), true);
    return;
  }
  if (menu.hidden || menuDir !== dir) return;
  const render = () => renderBranches(list, status, filter.value);
  filter.addEventListener("input", render);
  // 回车切到过滤结果的第一项
  filter.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") list.querySelector<HTMLButtonElement>("button")?.click();
  });
  render();
}

function renderBranches(list: HTMLElement, status: HTMLElement, query: string) {
  const items: HTMLElement[] = [];
  const locals = filterRefs(refs.locals, query);
  if (locals.length) items.push(el("div", "gm-sec", "本地分支"));
  for (const name of locals) {
    const cur = name === refs.current;
    items.push(branchItem(name, cur, () => !cur && doSwitch(status, name, false)));
  }
  for (const g of refs.remotes) {
    const names = filterRefs(g.branches, query);
    if (names.length) items.push(el("div", "gm-sec", `远程 ${g.remote}`));
    for (const name of names) {
      items.push(branchItem(name, false, () => {
        const t = switchTarget(name, refs);
        doSwitch(status, t.name, t.track);
      }));
    }
  }
  if (!items.length) items.push(el("div", "gm-empty", "没有匹配的分支"));
  list.replaceChildren(...items);
}

function branchItem(name: string, current: boolean, onClick: () => void) {
  const b = el("button", current ? "gm-item gm-branch current" : "gm-item gm-branch");
  b.innerHTML = icon(current ? "check" : "git-branch");
  b.append(el("span", "gm-name", name));
  b.title = current ? "当前分支" : `切换到 ${name}`;
  b.addEventListener("click", onClick);
  return b;
}

/** git switch：期间禁用菜单；失败在菜单底部显示 git 的错误输出 */
async function doSwitch(status: HTMLElement, name: string, track: boolean) {
  if (menu.classList.contains("busy")) return;
  menu.classList.add("busy");
  showStatus(status, `正在切换到 ${name}…`, false);
  try {
    await invoke("git_switch", { dir: menuDir, name, track });
    menu.hidden = true;
    deps.onSwitched();
    if (!panel.hidden && panelDir === menuDir) loadHistory();
  } catch (e) {
    showStatus(status, errText(e), true);
  } finally {
    menu.classList.remove("busy");
  }
}

function showStatus(status: HTMLElement, text: string, error: boolean) {
  status.hidden = false;
  status.classList.toggle("err", error);
  status.textContent = text;
}

// ---------------- 提交历史 ----------------

async function openHistory(dir: string) {
  if (!(await requestClosePreview())) return;
  panelDir = dir;
  panel.hidden = false;
  loadHistory();
  listEl.focus();
}

export function closeGitPanel() {
  menu.hidden = true;
  if (panel.hidden) return;
  panel.hidden = true;
  loadSeq++;
  showSeq++;
  listEl.replaceChildren();
  detailEl.replaceChildren();
  deps.onClose();
}

async function loadHistory() {
  const seq = ++loadSeq;
  showSeq++;
  detailEl.hidden = true;
  detailEl.replaceChildren();
  listEl.replaceChildren(el("div", "gp-note", "正在读取提交…"));
  let out: string;
  try {
    const [log, r] = await Promise.all([invoke<string>("git_log", { dir: panelDir, limit: LOG_LIMIT }), invoke<string>("git_refs", { dir: panelDir })]);
    out = log;
    const cur = parseRefs(r).current;
    $("gp-branch").textContent = cur ? `⎇ ${cur}` : "分离 HEAD";
  } catch (e) {
    if (seq === loadSeq) listEl.replaceChildren(el("pre", "gp-note err", errText(e)));
    return;
  }
  if (seq !== loadSeq) return;
  const now = Date.now();
  const log = parseLog(out);
  // 图形列统一宽度，哈希等列对齐
  listEl.style.setProperty("--gw", `${Math.max(0, ...log.map((r) => r.graph.trimEnd().length))}ch`);
  const rows = log.map((row) => {
    const div = el("div", row.commit ? "gp-row" : "gp-row graph-only");
    div.append(graphEl(row.graph));
    const c = row.commit;
    if (c) {
      div.append(el("span", "gp-hash", c.short));
      const subj = el("span", "gp-subj");
      subj.append(...c.refs.map(decorEl), c.subject);
      subj.title = c.subject;
      div.append(subj, el("span", "gp-author", c.author), el("span", "gp-time", relTime(c.time, now)));
      div.addEventListener("click", () => {
        listEl.querySelector(".gp-row.on")?.classList.remove("on");
        div.classList.add("on");
        showCommit(c);
      });
    }
    return div;
  });
  listEl.replaceChildren(...(rows.length ? rows : [el("div", "gp-note", "没有提交")]));
  if (rows.length >= LOG_LIMIT) listEl.append(el("div", "gp-note", `仅显示最近 ${LOG_LIMIT} 个提交`));
}

/** 图形列：按 lane 着色（6 色循环） */
function graphEl(graph: string) {
  const g = el("span", "gp-graph");
  for (const r of graphRuns(graph)) {
    if (r.lane < 0) g.append(r.text);
    else g.append(el("span", `ln${r.lane % 6}`, r.text));
  }
  return g;
}

function decorEl(d: string) {
  const { name, kind } = parseDecor(d);
  return el("span", `gp-ref ${kind}`, name);
}

async function showCommit(c: Commit) {
  const seq = ++showSeq;
  detailEl.hidden = false;
  detailEl.replaceChildren(commitHead(c), el("div", "gp-note", "正在读取改动…"));
  let numstat: string, diff: string, cut: boolean;
  try {
    [numstat, diff, cut] = await invoke<[string, string, boolean]>("git_show", { dir: panelDir, hash: c.hash });
  } catch (e) {
    if (seq === showSeq) detailEl.replaceChildren(commitHead(c), el("pre", "gp-note err", errText(e)));
    return;
  }
  if (seq !== showSeq) return;
  const files = parseNumstat(numstat);
  const { lines, truncated } = classifyDiff(diff, DIFF_LINES);
  const diffEl = el("div", "gp-diff");
  const fileHeads: HTMLElement[] = [];
  for (const l of lines) {
    const row = el("div", `dl ${l.kind}`, l.text || " ");
    if (l.text.startsWith("diff ")) fileHeads.push(row);
    diffEl.append(row);
  }
  if (cut || truncated) diffEl.append(el("div", "gp-note warn", `改动过大，已截断显示（最多 ${DIFF_LINES} 行 / 512 KB）`));
  const filesEl = el("div", "gp-files");
  files.forEach((f, i) => {
    const row = el("button", "gp-file");
    row.append(el("span", "gp-path", f.path));
    if (f.add === null) row.append(el("span", "gp-bin", "二进制"));
    else row.append(el("span", "gp-add", `+${f.add}`), el("span", "gp-del", `-${f.del}`));
    row.title = f.path;
    // numstat 与 patch 的文件顺序一致：点击滚动到对应 diff
    row.addEventListener("click", () => fileHeads[i]?.scrollIntoView({ block: "start" }));
    filesEl.append(row);
  });
  if (!files.length) filesEl.append(el("div", "gp-note", "没有文件改动"));
  const scroll = el("div", "gp-dscroll");
  scroll.append(filesEl, diffEl);
  detailEl.replaceChildren(commitHead(c), scroll);
}

function commitHead(c: Commit) {
  const head = el("div", "gp-dhead");
  const copy = el("button", "gp-copy");
  copy.innerHTML = icon("copy");
  copy.append(c.short);
  copy.title = `复制完整哈希 ${c.hash}`;
  copy.addEventListener("click", () => deps.copy(c.hash));
  const meta = el("span", "gp-dmeta", `${c.author} · ${new Date(c.time * 1000).toLocaleString()}`);
  head.append(copy, el("span", "gp-dsubj", c.subject), meta);
  return head;
}
