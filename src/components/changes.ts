// 变更面板（默认在 bottom 槽位）：页签「未提交」（文件表 + 带新旧行号的 diff）与「提交」（最近提交 + 选中提交的改动），
// 页签条与 diff 文件头合成一行（设计稿 docs/design/05-dense-c4-onebar.html）。未提交改动读 store.changes（changes-refresher 维护），
// diff 与提交历史经 GitSource 懒加载；远程会话还没有 log / show 通道，「提交」只列 Changes 结果里的最近提交、不看详情。
// 开关即 store.changesOpen（Ctrl+Shift+J、卡片头改动芯片）；焦点在面板内时 Esc 收起；上缘拖动调整高度并记在本机。
// 放在 left / right 槽位时为窄版：列表在上、diff 在下（样式见 styles.css 的 .slot-left / .slot-right）
import type { GitSource, GitTarget } from "../git-source";
import { type ChangedFile, type Commit, classifyDiff, parseDecor, parseNumstat, relTime } from "../git-utils";
import { type MessageKey, fmtDateTime, fmtNum, onLocaleChange, t, tn } from "../i18n";
import { icon } from "../icons";
import type { SessionInfo } from "../protocol";
import { type Component, type Ctx, html } from "../ui/component";
import { shallow } from "../ui/store";
import { type Workspace, groupSessions, workspaceOf } from "../workspace";

const HEIGHT_KEY = "tether.changes.height";
const DEFAULT_H = 300;
const MIN_H = 120;
/** diff 显示的行数上限、「提交」页签列出的提交数 */
const DIFF_LINES = 3000;
const LOG_LIMIT = 100;

type Tab = "work" | "log";

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", text = "") => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text) e.textContent = text;
  return e;
};
const errText = (e: unknown) => (typeof e === "string" ? e : String(e));
const readHeight = () => {
  try {
    const v = Number(localStorage.getItem(HEIGHT_KEY));
    return v >= MIN_H ? v : DEFAULT_H;
  } catch {
    return DEFAULT_H;
  }
};

const LETTER: Record<ChangedFile["kind"], string> = { modified: "M", added: "A", deleted: "D", renamed: "R", conflict: "U", untracked: "?" };

/** 「目录/」淡色 + 文件名加粗（未跟踪目录以 / 结尾，整段算名字） */
function pathEl(cls: string, p: string) {
  const i = p.replace(/\/$/, "").lastIndexOf("/");
  const s = el("span", cls);
  if (i >= 0) s.append(el("span", "dir", p.slice(0, i + 1)));
  s.append(el("b", "", p.slice(i + 1)));
  s.title = p;
  return s;
}

/** +a −d（为 0 的一侧不显示） */
function numsEl(add: number | null, del: number | null) {
  const s = el("span", "nums");
  if (add) s.append(el("span", "add", `+${fmtNum(add)}`));
  if (del) s.append(el("span", "del", `−${fmtNum(del)}`));
  return s;
}

/** 5 格比例条：绿 = 增、红 = 删 */
function barEl(add: number | null, del: number | null) {
  const b = el("span", "bar");
  const total = (add ?? 0) + (del ?? 0);
  if (!total) return b;
  const g = Math.round((5 * (add ?? 0)) / total);
  for (let i = 0; i < 5; i++) b.append(el("i", i < g ? "g" : "r"));
  return b;
}

/** 带新旧行号的 diff；files 为多文件 diff（提交）各文件的名字，按 diff --git 的先后插文件头 */
function diffEl(text: string, cut: boolean, files?: string[]) {
  const { lines, truncated } = classifyDiff(text, DIFF_LINES);
  const box = el("div", "diff");
  let fi = 0;
  for (const l of lines) {
    if (l.kind === "meta") {
      if (files && l.text.startsWith("diff ")) box.append(pathEl("dfile", files[fi++] ?? l.text));
      continue;
    }
    const row = el("div", `dl ${l.kind}`);
    row.append(el("span", "n", l.old === undefined ? "" : String(l.old)), el("span", "n", l.new === undefined ? "" : String(l.new)), el("span", "c", l.kind === "hunk" ? l.text : l.text.slice(1)));
    box.append(row);
  }
  if (!box.childElementCount) return el("div", "cp-note", t("changes.noDiff"));
  if (cut || truncated) box.append(el("div", "cp-note warn", t("git.truncated", { lines: DIFF_LINES })));
  return box;
}

export function createChangesPanel(ctx: Ctx, git: GitSource): Component {
  const { store, actions, keymap } = ctx;
  const root = html(`<section class="c-changes" tabindex="-1" hidden>
      <div class="cp-resize" data-i18n-title="changes.resize"></div>
      <div class="cp-bar">
        <div class="cp-bl" role="tablist">
          <button class="cp-tab" data-tab="work" role="tab"><span data-i18n="changes.tabWork"></span><span class="n"></span></button>
          <button class="cp-tab" data-tab="log" role="tab"><span data-i18n="changes.tabLog"></span></button>
          <button class="cp-ws" data-i18n-title="changes.wsTitle"></button>
        </div>
        <div class="cp-br">
          <span class="cp-name"></span>
          <span class="cp-stat"></span>
          <button class="ib cp-hist" data-i18n-title="changes.openHistory">${icon("history")}</button>
          <button class="ib cp-close" data-i18n-title="changes.collapse">${icon("x")}</button>
        </div>
      </div>
      <div class="cp-body">
        <div class="cp-list" tabindex="0"></div>
        <div class="cp-diff"></div>
      </div>
    </section>`);
  const $ = (sel: string) => root.querySelector<HTMLElement>(sel)!;
  const list = $(".cp-list");
  const out = $(".cp-diff");
  const nameEl = $(".cp-name");
  const statEl = $(".cp-stat");
  const wsBtn = $(".cp-ws");
  const histBtn = $(".cp-hist");
  const esc = keymap.esc(() => store.set({ changesOpen: false }));
  let unbind: (() => void) | undefined;

  let tab: Tab = "work";
  /** 未提交：选中的文件；已显示的 diff 对应的「工作区 + 路径 + 状态 + 行数」（不变就不重新拉取） */
  let sel: string | undefined;
  let diffKey: string | undefined;
  let diffSeq = 0;
  /** 提交：本地已读取的列表（按工作区与 HEAD 缓存）与选中的提交 */
  let log: { key: string; commits: Commit[] | string } | undefined;
  let logSeq = 0;
  let selCommit: string | undefined;
  let showKey: string | undefined;
  let showSeq = 0;

  root.style.height = `${readHeight()}px`;

  /** 面板显示的工作区：选定的（仍存在时），否则当前会话的 */
  function currentWs(): Workspace | undefined {
    const st = store.get();
    const ws = groupSessions(st.sessions, st.roots);
    const s = st.sessions.find((i) => i.id === st.activeId);
    const own = s && workspaceOf(s, st.roots).key;
    return ws.find((w) => w.key === st.changesWs) ?? ws.find((w) => w.key === own);
  }
  /** 远程借用一个未退出的会话的 SSH 连接 */
  function targetOf(w: Workspace): GitTarget | undefined {
    if (!w.host) return { kind: "local" };
    const s = w.sessions.find((i) => i.status !== "exited");
    return s && { kind: "remote", sessionId: s.id };
  }
  /** 预览用的会话：当前会话在该工作区则用它，否则用工作区里第一个未退出的 */
  function sessionOf(w: Workspace): SessionInfo | undefined {
    return w.sessions.find((s) => s.id === store.get().activeId) ?? w.sessions.find((s) => s.status !== "exited");
  }

  function render() {
    if (root.hidden) return;
    const w = currentWs();
    const cs = w && store.get().changes.get(w.key);
    for (const b of root.querySelectorAll<HTMLElement>(".cp-tab")) {
      const on = b.dataset.tab === tab;
      b.classList.toggle("on", on);
      b.setAttribute("aria-selected", String(on));
    }
    $(".cp-tab .n").textContent = cs?.files.length ? String(cs.files.length) : "";
    wsBtn.innerHTML = `${icon("folder-git")}<span class="wn"></span>${icon("chevron-down")}`;
    wsBtn.querySelector(".wn")!.textContent = w ? (w.host ? `${w.host}:${w.name}` : w.name) : "—";
    wsBtn.title = w ? `${t("changes.wsTitle")}\n${w.host ? `${w.host}:` : ""}${w.dir}` : t("changes.wsTitle");
    histBtn.hidden = !w || !!w.host;
    if (tab === "work") renderWork(w);
    else renderLog(w);
  }

  // ---------------- 未提交 ----------------

  function renderWork(w: Workspace | undefined) {
    const cs = w && store.get().changes.get(w.key);
    nameEl.replaceChildren();
    statEl.replaceChildren();
    if (!w || !cs || !cs.files.length) {
      list.replaceChildren(el("div", "cp-note", t(!w ? "changes.noWorkspace" : !cs ? "changes.noData" : "changes.clean")));
      out.replaceChildren();
      sel = diffKey = undefined;
      return;
    }
    const f = cs.files.find((i) => i.path === sel) ?? cs.files[0];
    sel = f.path;
    const rows = cs.files.map((file) => {
      const row = el("button", file === f ? "tr on" : "tr");
      row.tabIndex = -1;
      const st = el("span", `st st-${file.kind}`, LETTER[file.kind]);
      st.title = t(`changes.kind.${file.kind}` as MessageKey);
      const p = pathEl("p", file.path);
      if (file.orig) p.title = `${file.orig} → ${file.path}`;
      row.append(st, p);
      // 未跟踪文件不统计行数，只标「新」
      if (file.kind === "untracked") row.append(el("span", "new", t("changes.new")));
      else if (file.add === null) row.append(el("span", "bin", t("git.binary")));
      else row.append(el("span", "a", file.add ? `+${fmtNum(file.add)}` : ""), el("span", "d", file.del ? `−${fmtNum(file.del)}` : ""));
      row.append(barEl(file.add, file.del));
      row.addEventListener("click", () => select(file.path));
      row.addEventListener("dblclick", () => openPreview(file));
      return row;
    });
    const sum = el("div", "tr sum");
    sum.append(el("span"), el("span", "p", tn("changes.total", cs.files.length)), el("span", "a", cs.added ? `+${fmtNum(cs.added)}` : ""), el("span", "d", cs.deleted ? `−${fmtNum(cs.deleted)}` : ""), el("span"));
    list.replaceChildren(...rows, sum);
    nameEl.replaceChildren(pathEl("diff-name", f.path));
    if (f.kind === "untracked") statEl.append(el("span", "new", t("changes.new")));
    else statEl.append(numsEl(f.add, f.del));
    loadDiff(w, f);
  }

  function select(path: string) {
    sel = path;
    render();
    list.querySelector(".tr.on")?.scrollIntoView({ block: "nearest" });
  }

  /** 选中文件的 diff：状态与行数都没变时保留已显示的 */
  async function loadDiff(w: Workspace, f: ChangedFile) {
    const key = [w.key, f.path, f.xy, f.add, f.del].join("\0");
    if (key === diffKey) return;
    diffKey = key;
    const seq = ++diffSeq;
    const target = targetOf(w);
    const dir = store.get().changes.get(w.key)?.root ?? w.dir;
    out.replaceChildren(el("div", "cp-note", t("git.loadingDiff")));
    out.scrollTop = 0;
    let node: HTMLElement;
    try {
      if (!target) throw new Error(t("changes.noData"));
      const d = await git.diff(target, dir, f.path, f.kind === "untracked");
      node = diffEl(d.text, d.truncated);
    } catch (e) {
      node = el("pre", "cp-note err", errText(e));
      // 失败不缓存：下次刷新重试
      if (seq === diffSeq) diffKey = undefined;
    }
    if (seq === diffSeq) out.replaceChildren(node);
  }

  /** Enter / 双击：在应用内预览（删除的文件与未跟踪的目录除外） */
  function openPreview(f: ChangedFile) {
    const w = currentWs();
    const cs = w && store.get().changes.get(w.key);
    const s = w && sessionOf(w);
    if (!w || !cs || !s || f.kind === "deleted" || f.path.endsWith("/")) return;
    const path = w.host ? `${cs.root.replace(/\/$/, "")}/${f.path}` : `${cs.root.replace(/[\\/]$/, "")}\\${f.path.replace(/\//g, "\\")}`;
    actions.previewFile(s, path);
  }

  // ---------------- 提交 ----------------

  function renderLog(w: Workspace | undefined) {
    nameEl.replaceChildren();
    statEl.replaceChildren();
    if (!w) {
      list.replaceChildren(el("div", "cp-note", t("changes.noWorkspace")));
      out.replaceChildren();
      return;
    }
    const cs = store.get().changes.get(w.key);
    let commits: Commit[] | string;
    if (w.host) commits = cs?.commits ?? [];
    else {
      // 按工作区与 HEAD 缓存：有新提交（HEAD 变化）时重新读取
      const key = `${w.key}\0${cs?.commits[0]?.hash ?? ""}`;
      if (log?.key !== key) {
        log = { key, commits: t("git.loadingCommits") };
        void loadLog(w, key);
      }
      commits = log.commits;
    }
    if (typeof commits === "string") {
      list.replaceChildren(el("div", "cp-note", commits));
      out.replaceChildren();
      return;
    }
    const c = commits.find((i) => i.hash === selCommit) ?? commits[0];
    selCommit = c?.hash;
    const now = Date.now();
    const rows = commits.map((cm) => {
      const row = el("button", cm === c ? "cr on" : "cr");
      row.tabIndex = -1;
      const m = el("span", "m");
      m.append(cm.subject, ...cm.refs.map((d) => el("span", `ref ${parseDecor(d).kind}`, parseDecor(d).name)));
      m.title = cm.subject;
      row.append(el("span", "hash", cm.short), m, el("span", "t", relTime(cm.time, now)));
      row.title = `${cm.author} · ${fmtDateTime(cm.time * 1000)}`;
      row.addEventListener("click", () => {
        selCommit = cm.hash;
        render();
      });
      return row;
    });
    list.replaceChildren(...(rows.length ? rows : [el("div", "cp-note", t("git.noCommits"))]));
    if (!c) {
      out.replaceChildren();
      showKey = undefined;
      return;
    }
    const copy = el("button", "hash cp-copy", c.short);
    copy.title = t("git.copyHash", { hash: c.hash });
    copy.addEventListener("click", () => actions.copy(c.hash));
    nameEl.replaceChildren(copy, el("span", "cp-subj", c.subject));
    statEl.append(el("span", "cp-meta", `${c.author} · ${relTime(c.time, now)}`));
    if (w.host) {
      showKey = undefined;
      out.replaceChildren(el("div", "cp-note", t("changes.remoteNoDetail")));
    } else void loadShow(w, c);
  }

  async function loadLog(w: Workspace, key: string) {
    const seq = ++logSeq;
    let commits: Commit[] | string;
    try {
      commits = await git.log(w.dir, LOG_LIMIT);
    } catch (e) {
      commits = errText(e);
    }
    if (seq !== logSeq || log?.key !== key) return;
    log = { key, commits };
    render();
  }

  async function loadShow(w: Workspace, c: Commit) {
    const key = `${w.key}\0${c.hash}`;
    if (key === showKey) return;
    showKey = key;
    const seq = ++showSeq;
    out.replaceChildren(el("div", "cp-note", t("git.loadingDiff")));
    out.scrollTop = 0;
    let node: HTMLElement;
    try {
      const r = await git.show(w.dir, c.hash);
      const files = parseNumstat(r.numstat);
      node = el("div", "cp-commit");
      const fl = el("div", "cp-cfiles");
      for (const f of files) {
        const row = el("button", "tr");
        row.append(el("span"), pathEl("p", f.path));
        if (f.add === null) row.append(el("span", "bin", t("git.binary")));
        else row.append(el("span", "a", f.add ? `+${fmtNum(f.add)}` : ""), el("span", "d", f.del ? `−${fmtNum(f.del)}` : ""));
        row.append(barEl(f.add, f.del));
        // numstat 与 patch 的文件顺序一致：点击滚动到对应的文件头
        row.addEventListener("click", () => node.querySelectorAll(".dfile")[files.indexOf(f)]?.scrollIntoView({ block: "start" }));
        fl.append(row);
      }
      if (!files.length) fl.append(el("div", "cp-note", t("git.noFiles")));
      node.append(fl, diffEl(r.diff, r.truncated, files.map((f) => f.path)));
    } catch (e) {
      node = el("pre", "cp-note err", errText(e));
      if (seq === showSeq) showKey = undefined;
    }
    if (seq === showSeq) out.replaceChildren(node);
  }

  // ---------------- 交互 ----------------

  /** ↑↓ 在当前页签的列表里移动选中 */
  function move(delta: number) {
    const w = currentWs();
    if (!w) return;
    const cs = store.get().changes.get(w.key);
    if (tab === "work") {
      const files = cs?.files ?? [];
      const i = files.findIndex((f) => f.path === sel);
      const next = files[Math.max(0, Math.min(files.length - 1, i + delta))];
      if (next) select(next.path);
    } else {
      const commits = w.host ? (cs?.commits ?? []) : typeof log?.commits === "object" ? log.commits : [];
      const i = commits.findIndex((c) => c.hash === selCommit);
      const next = commits[Math.max(0, Math.min(commits.length - 1, i + delta))];
      if (next) {
        selCommit = next.hash;
        render();
        list.querySelector(".cr.on")?.scrollIntoView({ block: "nearest" });
      }
    }
  }

  list.addEventListener("keydown", (ev) => {
    if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
      ev.preventDefault();
      move(ev.key === "ArrowDown" ? 1 : -1);
    } else if (ev.key === "Enter" && tab === "work") {
      ev.preventDefault();
      const w = currentWs();
      const f = w && store.get().changes.get(w.key)?.files.find((i) => i.path === sel);
      if (f) openPreview(f);
    }
  });
  for (const b of root.querySelectorAll<HTMLElement>(".cp-tab")) {
    b.addEventListener("click", () => {
      tab = b.dataset.tab as Tab;
      render();
      list.focus();
    });
  }
  wsBtn.addEventListener("click", () => {
    const st = store.get();
    const cur = currentWs();
    const ws = groupSessions(st.sessions, st.roots).filter((w) => w.branch !== null || st.changes.has(w.key));
    const r = wsBtn.getBoundingClientRect();
    actions.showMenu(
      r.left,
      r.bottom + 4,
      ws.map((w) => [`${w.key === cur?.key ? "✓ " : ""}${w.host ? `${w.host}:` : ""}${w.name}`, () => store.set({ changesWs: w.key })]),
    );
  });
  histBtn.addEventListener("click", () => {
    const w = currentWs();
    if (w && !w.host) actions.openHistory(w.dir);
  });
  $(".cp-close").addEventListener("click", () => store.set({ changesOpen: false }));
  // 焦点在面板内时才登记 Esc（否则终端里的 Esc 会被面板拦下）
  root.addEventListener("focusin", () => esc(true));
  root.addEventListener("focusout", (ev) => {
    if (!root.contains(ev.relatedTarget as Node | null)) esc(false);
  });

  // 上缘拖动调整高度：松开后记住；终端随槽位尺寸变化由其 ResizeObserver 重新 fit
  $(".cp-resize").addEventListener("pointerdown", (ev) => {
    const handle = ev.currentTarget as HTMLElement;
    handle.setPointerCapture(ev.pointerId);
    const y0 = ev.clientY;
    const h0 = root.offsetHeight;
    const onMove = (e: PointerEvent) => (root.style.height = `${Math.max(MIN_H, Math.min(innerHeight - 160, h0 + y0 - e.clientY))}px`);
    const onUp = () => {
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", onUp);
      try {
        localStorage.setItem(HEIGHT_KEY, String(root.offsetHeight));
      } catch {}
    };
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", onUp);
  });

  store.select(
    (s) => s.changesOpen,
    (open) => {
      root.hidden = !open;
      if (open) {
        render();
        list.focus();
      } else {
        esc(false);
        diffKey = showKey = undefined;
        diffSeq++;
        showSeq++;
        actions.focusTerminal(true);
      }
    },
  );
  // 切换会话：工作区回到跟随当前会话
  store.select(
    (s) => s.activeId,
    () => store.get().changesWs !== null && store.set({ changesWs: null }),
  );
  store.select((s) => [s.changes, s.changesWs, s.activeId, s.roots, s.sessions], render, shallow);
  onLocaleChange(() => {
    diffKey = showKey = undefined;
    render();
  });

  return {
    el: root,
    mount() {
      unbind = keymap.bind("Ctrl+Shift+J", () => store.set({ changesOpen: !store.get().changesOpen }));
    },
    unmount: () => unbind?.(),
  };
}
