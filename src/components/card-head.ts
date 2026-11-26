// 卡片头：当前会话的「工作区 › 会话名」、目录 · 分支 · shell（远程为 ssh user@host）、未提交改动统计与 Claude 状态；
// 点分支弹出菜单（过滤 / 查看历史 / 切换本地与远程分支）；点改动统计开关变更面板（本地与远程相同）
import { invoke } from "@tauri-apps/api/core";
import { type Refs, filterRefs, parseRefs, switchTarget } from "../git-utils";
import { fmtNum, onLocaleChange, t, tn } from "../i18n";
import { icon } from "../icons";
import { displayName, nameTooltip, renderClaude, shellLabel } from "../session-utils";
import { branchDir, branchLabel } from "../term-utils";
import { type Component, type Ctx, html } from "../ui/component";
import { overlay } from "../ui/layout";
import { activeSession, shallow } from "../ui/store";
import { workspaceOf } from "../workspace";

export interface CardHead extends Component {
  /** 查询当前会话的 git 分支（激活、窗口获得焦点、定时） */
  refreshBranch(): void;
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", text = "") => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text) e.textContent = text;
  return e;
};
const errText = (e: unknown) => (typeof e === "string" ? e : String(e));

export function createCardHead(ctx: Ctx): CardHead {
  const { store, actions } = ctx;
  const root = html(`<div id="card-head">
        <span class="crumb"><span id="ch-ws"></span>${icon("chevron-right")}<b id="ch-name"></b></span>
        <div class="ch-text">
          <div class="ch-meta"><span id="ch-cwd"></span><span id="ch-branch" role="button"></span><span id="ch-meta"></span></div>
        </div>
        <button id="ch-changes" class="dchip" hidden></button>
        <div class="ch-claude"><span id="ch-claude" hidden></span></div>
      </div>`);
  const $ = (id: string) => root.querySelector<HTMLElement>(`#${id}`)!;
  const branchEl = $("ch-branch");
  const chip = $("ch-changes");
  /** 分支菜单（git-menu）：浮在 overlay 槽位 */
  const menu = html(`<div id="git-menu" hidden></div>`);
  const menuEsc = ctx.keymap.esc(closeMenu);
  /** 当前会话启动目录的 git 分支文字（只缓存激活会话的一份） */
  let branch: { id: number; text: string } | undefined;
  /** 菜单打开时的目录与分支列表 */
  let menuDir = "";
  let refs: Refs = { current: null, locals: [], remotes: [] };

  function render() {
    const st = store.get();
    const s = activeSession(st);
    root.classList.toggle("none", !s);
    $("ch-ws").textContent = s ? workspaceOf(s, st.roots).name : "";
    $("ch-name").textContent = s ? displayName(s) : "";
    $("ch-name").title = s ? nameTooltip(s) : "";
    $("ch-meta").textContent = s ? shellLabel(s) : "";
    const cwd = $("ch-cwd");
    cwd.innerHTML = s ? icon(s.host ? "globe" : "folder") : "";
    if (s) cwd.append(s.host ? `${s.host}:${s.cwd}` : s.cwd);
    cwd.title = s?.cwd ?? "";
    branchEl.textContent = s && branch?.id === s.id ? branch.text : "";
    renderClaude($("ch-claude"), s?.claude);
    renderChanges();
  }

  /** 当前会话所在工作区的未提交改动：「N 文件 +a −d」，没有改动时隐藏；未跟踪文件计入文件数、不计行数 */
  function renderChanges() {
    const st = store.get();
    const s = activeSession(st);
    const cs = s && st.changes.get(workspaceOf(s, st.roots).key);
    chip.hidden = !cs?.files.length;
    if (!s || !cs?.files.length) return;
    const nums = el("span", "nums");
    if (cs.added) nums.append(el("span", "add", `+${fmtNum(cs.added)}`));
    if (cs.deleted) nums.append(el("span", "del", `−${fmtNum(cs.deleted)}`));
    chip.innerHTML = icon("file-diff");
    chip.append(tn("changes.files", cs.files.length), nums);
    chip.title = t("changes.chipTitle");
  }

  /** 查询激活会话的 git 分支并更新；仅本地会话，不在仓库里时为空 */
  async function refreshBranch() {
    const s = activeSession(store.get());
    if (!s) return;
    const dir = branchDir(s.host, s.cwd);
    const b = dir ? await invoke<string | null>("git_branch", { dir }).catch(() => null) : null;
    // 查询期间切换了会话则丢弃
    if (store.get().activeId !== s.id) return;
    branch = { id: s.id, text: branchLabel(b) };
    branchEl.textContent = branch.text;
  }

  // ---------------- 分支菜单 ----------------

  function closeMenu() {
    menu.hidden = true;
    menuEsc(false);
  }

  async function openMenu() {
    const s = activeSession(store.get());
    const dir = s ? branchDir(s.host, s.cwd) : null;
    if (!dir) return;
    menuDir = dir;
    const filter = el("input", "gm-filter");
    filter.placeholder = t("git.filter");
    const history = el("button", "gm-item");
    history.innerHTML = icon("history");
    history.append(t("git.viewHistory"));
    history.addEventListener("click", () => {
      closeMenu();
      actions.openHistory(dir);
    });
    const list = el("div", "gm-list", t("git.loadingBranches"));
    const status = el("pre", "gm-status");
    status.hidden = true;
    menu.replaceChildren(filter, history, list, status);
    menu.classList.remove("busy");
    menu.hidden = false;
    menuEsc(true);
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
    const renderList = () => renderBranches(list, status, filter.value);
    filter.addEventListener("input", renderList);
    // 回车切到过滤结果的第一项
    filter.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter") list.querySelector<HTMLButtonElement>("button")?.click();
    });
    renderList();
  }

  function renderBranches(list: HTMLElement, status: HTMLElement, query: string) {
    const items: HTMLElement[] = [];
    const locals = filterRefs(refs.locals, query);
    if (locals.length) items.push(el("div", "gm-sec", t("git.localBranches")));
    for (const name of locals) {
      const cur = name === refs.current;
      items.push(branchItem(name, cur, () => !cur && doSwitch(status, name, false)));
    }
    for (const g of refs.remotes) {
      const names = filterRefs(g.branches, query);
      if (names.length) items.push(el("div", "gm-sec", t("git.remote", { remote: g.remote })));
      for (const name of names) {
        items.push(
          branchItem(name, false, () => {
            const target = switchTarget(name, refs);
            doSwitch(status, target.name, target.track);
          }),
        );
      }
    }
    if (!items.length) items.push(el("div", "gm-empty", t("git.noMatch")));
    list.replaceChildren(...items);
  }

  function branchItem(name: string, current: boolean, onClick: () => void) {
    const b = el("button", current ? "gm-item gm-branch current" : "gm-item gm-branch");
    b.innerHTML = icon(current ? "check" : "git-branch");
    b.append(el("span", "gm-name", name));
    b.title = current ? t("git.current") : t("git.switchTo", { name });
    b.addEventListener("click", onClick);
    return b;
  }

  /** git switch：期间禁用菜单；失败在菜单底部显示 git 的错误输出 */
  async function doSwitch(status: HTMLElement, name: string, track: boolean) {
    if (menu.classList.contains("busy")) return;
    menu.classList.add("busy");
    showStatus(status, t("git.switching", { name }), false);
    try {
      await invoke("git_switch", { dir: menuDir, name, track });
      closeMenu();
      void refreshBranch();
      actions.reloadHistory(menuDir);
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

  const syncTitle = () => (branchEl.title = t("git.branchTitle"));
  syncTitle();
  branchEl.addEventListener("click", () => (menu.hidden ? openMenu() : closeMenu()));
  chip.addEventListener("click", () => store.set({ changesOpen: !store.get().changesOpen, changesWs: null }));
  document.addEventListener("mousedown", (ev) => {
    const target = ev.target as Node;
    if (!menu.hidden && !menu.contains(target) && !branchEl.contains(target)) closeMenu();
  });
  // 切换语言：重绘，分支菜单收起
  onLocaleChange(() => {
    render();
    syncTitle();
    closeMenu();
  });
  store.select((s) => [activeSession(s), s.sessionsLoaded, s.roots], render, shallow);
  store.select((s) => s.changes, renderChanges);
  // 菜单属于切换前的会话
  store.select((s) => s.activeId, closeMenu);
  // 定时刷新当前会话的 git 分支，反映终端里的 git checkout（窗口隐藏时跳过）
  setInterval(() => !document.hidden && refreshBranch(), 4000);

  return {
    el: root,
    mount: () => overlay().append(menu),
    unmount: () => menu.remove(),
    refreshBranch: () => void refreshBranch(),
  };
}
