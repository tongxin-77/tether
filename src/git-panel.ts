// 提交历史面板（卡片头分支菜单“查看历史”打开）；git 命令由后端执行，输出解析见 git-utils.ts
import { invoke } from "@tauri-apps/api/core";
import { icon } from "./icons";
import { fmtDateTime, onLocaleChange, t, tn } from "./i18n";
import { type Commit, classifyDiff, graphRuns, parseDecor, parseLog, parseNumstat, parseRefs, relTime } from "./git-utils";
import { type Component, type Ctx, html } from "./ui/component";

export interface HistoryPanel extends Component {
  open(dir: string): Promise<void>;
  /** 分支切换后：面板正显示 dir 时重新读取 */
  reload(dir: string): void;
  close(): void;
}

interface Deps {
  /** 复制文本并提示 */
  copy: (text: string) => void;
  /** 面板关闭后（重新 fit 并聚焦当前终端） */
  onClose: () => void;
  /** 关闭预览（有未保存修改时先确认），返回是否已关闭 */
  requestClosePreview: () => Promise<boolean>;
}

/** 历史列表的提交数与 diff 显示的行数上限 */
const LOG_LIMIT = 200;
const DIFF_LINES = 3000;

let panel: HTMLElement;
let listEl: HTMLElement;
let detailEl: HTMLElement;
/** 打开期间登记 Esc 关闭 */
let esc: (open: boolean) => void;

let deps: Deps;
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

export function createHistoryPanel(ctx: Ctx): HistoryPanel {
  panel = html(`      <!-- 提交历史：覆盖在终端卡片上方（git-panel.ts 渲染）；上为 log --graph 列表，选中提交后下方显示改动 -->
      <div id="git-panel" hidden>
        <div id="gp-head">
          <span id="gp-ico"></span>
          <span id="gp-title" data-i18n="git.historyTitle"></span>
          <span id="gp-branch"></span>
          <button id="gp-close" class="fs-btn" data-i18n-title="common.closeEsc"><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg></button>
        </div>
        <div id="gp-body">
          <div id="gp-list" tabindex="-1"></div>
          <div id="gp-detail" hidden></div>
        </div>
      </div>`);
  const $ = (id: string) => panel.querySelector<HTMLElement>(`#${id}`)!;
  listEl = $("gp-list");
  detailEl = $("gp-detail");
  deps = { copy: ctx.actions.copy, onClose: () => ctx.actions.focusTerminal(true), requestClosePreview: ctx.actions.requestClosePreview };
  esc = ctx.keymap.esc(closeGitPanel);
  // 切换语言：已打开的历史面板重新读取渲染
  onLocaleChange(() => {
    if (!panel.hidden) loadHistory();
  });
  $("gp-ico").innerHTML = icon("history");
  $("gp-close").addEventListener("click", closeGitPanel);
  return { el: panel, open: openHistory, reload: reloadHistory, close: closeGitPanel };
}

// ---------------- 提交历史 ----------------

async function openHistory(dir: string) {
  if (!(await deps.requestClosePreview())) return;
  panelDir = dir;
  panel.hidden = false;
  esc(true);
  loadHistory();
  listEl.focus();
}

/** 分支切换后：面板正显示 dir 时重新读取 */
function reloadHistory(dir: string) {
  if (!panel.hidden && panelDir === dir) loadHistory();
}

function closeGitPanel() {
  if (panel.hidden) return;
  panel.hidden = true;
  esc(false);
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
  listEl.replaceChildren(el("div", "gp-note", t("git.loadingCommits")));
  let out: string;
  try {
    const [log, r] = await Promise.all([invoke<string>("git_log", { dir: panelDir, limit: LOG_LIMIT }), invoke<string>("git_refs", { dir: panelDir })]);
    out = log;
    const cur = parseRefs(r).current;
    panel.querySelector("#gp-branch")!.textContent = cur ? `⎇ ${cur}` : t("git.detached");
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
  listEl.replaceChildren(...(rows.length ? rows : [el("div", "gp-note", t("git.noCommits"))]));
  if (rows.length >= LOG_LIMIT) listEl.append(el("div", "gp-note", tn("git.limit", LOG_LIMIT)));
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
  detailEl.replaceChildren(commitHead(c), el("div", "gp-note", t("git.loadingDiff")));
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
  if (cut || truncated) diffEl.append(el("div", "gp-note warn", t("git.truncated", { lines: DIFF_LINES })));
  const filesEl = el("div", "gp-files");
  files.forEach((f, i) => {
    const row = el("button", "gp-file");
    row.append(el("span", "gp-path", f.path));
    if (f.add === null) row.append(el("span", "gp-bin", t("git.binary")));
    else row.append(el("span", "gp-add", `+${f.add}`), el("span", "gp-del", `-${f.del}`));
    row.title = f.path;
    // numstat 与 patch 的文件顺序一致：点击滚动到对应 diff
    row.addEventListener("click", () => fileHeads[i]?.scrollIntoView({ block: "start" }));
    filesEl.append(row);
  });
  if (!files.length) filesEl.append(el("div", "gp-note", t("git.noFiles")));
  const scroll = el("div", "gp-dscroll");
  scroll.append(filesEl, diffEl);
  detailEl.replaceChildren(commitHead(c), scroll);
}

function commitHead(c: Commit) {
  const head = el("div", "gp-dhead");
  const copy = el("button", "gp-copy");
  copy.innerHTML = icon("copy");
  copy.append(c.short);
  copy.title = t("git.copyHash", { hash: c.hash });
  copy.addEventListener("click", () => deps.copy(c.hash));
  const meta = el("span", "gp-dmeta", `${c.author} · ${fmtDateTime(c.time * 1000)}`);
  head.append(copy, el("span", "gp-dsubj", c.subject), meta);
  return head;
}
