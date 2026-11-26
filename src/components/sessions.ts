// 侧栏会话列表：筛选框 + 按工作区分组的紧凑行。组头可折叠（记在本机）、显示主机 / 分支 / 未提交文件数 Δn / 状态汇总、可在此新建；
// 行：彩色状态字符、名称（未读加粗 / 已退出删除线）、子目录、接管与内存小标签、状态字、悬停显示关闭。
// 单击切换、双击改名、右键在此新建、拖出成独立窗口；Ctrl+Shift+K 聚焦筛选框，Esc 清空并退出
import { applyDom, onLocaleChange, t, tn } from "../i18n";
import { icon } from "../icons";
import type { SessionInfo, Target } from "../protocol";
import { displayName } from "../session-utils";
import { formatMemory, memoryHigh } from "../term-utils";
import { type Component, type Ctx, html } from "../ui/component";
import { type RowState, type Workspace, groupSessions, matchSession, relSubdir, rowState, summarize } from "../workspace";

interface Item {
  li: HTMLLIElement;
  glyph: HTMLSpanElement;
  nameEl: HTMLSpanElement;
  subEl: HTMLSpanElement;
  takenEl: HTMLSpanElement;
  /** 内存占用（仅本地会话，收到 memory 推送后显示） */
  memEl: HTMLSpanElement;
  stateEl: HTMLSpanElement;
}

interface Group {
  sec: HTMLElement;
  toggle: HTMLButtonElement;
  nameEl: HTMLElement;
  hostEl: HTMLElement;
  branchEl: HTMLElement;
  /** 未提交文件数 Δn */
  dcEl: HTMLElement;
  sumEl: HTMLElement;
  add: HTMLButtonElement;
  ul: HTMLUListElement;
  /** 在此新建的目标（随会话列表更新） */
  target: Target | undefined;
}

const GLYPH: Record<RowState, string> = { thinking: "●", background: "●", waiting: "▲", done: "✓", shell: "○", exited: "○" };
const COLLAPSED_KEY = "tether.ws.collapsed";
/** 首轮仓库查询最多等这么久，超时按启动目录先显示 */
const ROOTS_WAIT_MS = 1500;

function readCollapsed(): Set<string> {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? "[]");
    return new Set(Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

function writeCollapsed(keys: Set<string>) {
  try {
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...keys]));
  } catch {}
}

export function createSessionList(ctx: Ctx): Component {
  const { store, send, actions, keymap } = ctx;
  const el = html(`<div class="c-sessions">
      <label class="ws-filter"><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg><input id="ws-filter" data-i18n-placeholder="sidebar.filter" autocomplete="off" spellcheck="false" /><kbd>Ctrl+Shift+K</kbd></label>
      <div id="ws-list"></div>
    </div>`);
  const filter = el.querySelector<HTMLInputElement>("#ws-filter")!;
  const list = el.querySelector<HTMLElement>("#ws-list")!;
  const empty = document.createElement("p");
  empty.className = "ws-empty";
  const items = new Map<number, Item>();
  const groups = new Map<string, Group>();
  const collapsed = readCollapsed();
  /** 已按仓库分组显示过（之前等首轮仓库查询） */
  let shown = false;
  let waitTimer: number | undefined;

  const infoOf = (id: number) => store.get().sessions.find((s) => s.id === id);

  function addItem(info: SessionInfo): Item {
    const id = info.id;
    const li = document.createElement("li");
    li.className = "r";
    li.innerHTML = `<span class="g"></span><span class="nm"></span><span class="sd"></span><span class="tag taken" data-i18n="sidebar.taken" data-i18n-title="banner.hub.title" hidden></span><span class="tag mem" data-i18n-title="sidebar.memTitle" hidden></span><span class="ss"></span><span class="close" data-i18n-title="common.close">${icon("x")}</span>`;
    applyDom(li);
    const q = (sel: string) => li.querySelector(sel) as HTMLSpanElement;
    const item: Item = { li, glyph: q(".g"), nameEl: q(".nm"), subEl: q(".sd"), takenEl: q(".taken"), memEl: q(".mem"), stateEl: q(".ss") };
    li.addEventListener("click", () => actions.activate(id));
    item.nameEl.addEventListener("dblclick", () => rename(id, item));
    // 右键：从该会话的启动目录新建（daemon 不跟踪 cd）；远程会话用同一主机
    li.addEventListener("contextmenu", (ev) => {
      ev.preventDefault();
      const s = infoOf(id) ?? info;
      const target: Target | undefined = !s.host ? { kind: "local", cwd: s.cwd } : s.hostId ? { kind: "remote", hostId: s.hostId, cwd: s.cwd } : undefined;
      actions.showMenu(ev.clientX, ev.clientY, [[t("sidebar.newHere"), () => target && actions.createAt(target), !target]]);
    });
    // 拖出窗口外松开：拖成独立窗口。Tauri 接管了系统拖放（文件拖入），HTML5 拖放不可用，改用指针捕获
    li.addEventListener("pointerdown", (ev) => {
      if (ev.button !== 0 || (ev.target as HTMLElement).closest(".close, input")) return;
      li.setPointerCapture(ev.pointerId);
      let dragging = false;
      const move = (e: PointerEvent) => {
        if (dragging || Math.hypot(e.clientX - ev.clientX, e.clientY - ev.clientY) < 8) return;
        dragging = true;
        li.classList.add("dragging");
      };
      const up = (e: PointerEvent) => {
        li.removeEventListener("pointermove", move);
        li.removeEventListener("pointerup", up);
        li.removeEventListener("pointercancel", up);
        li.classList.remove("dragging");
        const outside = e.clientX < 0 || e.clientY < 0 || e.clientX >= innerWidth || e.clientY >= innerHeight;
        if (dragging && outside && e.type === "pointerup") actions.detach(id, e.screenX, e.screenY);
      };
      li.addEventListener("pointermove", move);
      li.addEventListener("pointerup", up);
      li.addEventListener("pointercancel", up);
    });
    li.querySelector(".close")!.addEventListener("click", async (ev) => {
      ev.stopPropagation();
      // 已退出的会话没有可丢失的东西，直接关闭；其余先确认
      const s = infoOf(id);
      if (s && s.status !== "exited") {
        const ok = await actions.ask(t("sidebar.closeConfirm", { name: displayName(s) }), t("common.close"));
        if (ok === null) return;
      }
      send({ type: "close", id });
    });
    items.set(id, item);
    return item;
  }

  /** 改名：输入框默认值为当前显示名（看到什么改什么）；内容未改动时不发送，会话仍跟随对话标题 */
  function rename(id: number, item: Item) {
    const shown = () => {
      const s = infoOf(id);
      return s ? displayName(s) : "";
    };
    const name0 = shown();
    const input = document.createElement("input");
    input.value = name0;
    item.nameEl.replaceChildren(input);
    input.focus();
    input.select();
    let done = false;
    const finish = (save: boolean) => {
      if (done) return;
      done = true;
      const cur = shown() || name0;
      const name = input.value.trim();
      item.nameEl.textContent = save && name ? name : cur;
      if (save && name && name !== cur) send({ type: "rename", id, name });
      actions.focusTerminal();
    };
    input.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter") finish(true);
      if (ev.key === "Escape") finish(false);
    });
    input.addEventListener("blur", () => finish(true));
    input.addEventListener("click", (ev) => ev.stopPropagation());
  }

  function addGroup(key: string): Group {
    const sec = html(`<section class="grp">
        <div class="gh"><button class="tg">${icon("chevron-right", "cv")}<b></b><span class="hs"></span><span class="br"></span><span class="dc"></span><span class="ty"></span></button><button class="ad">${icon("plus")}</button></div>
        <ul></ul>
      </section>`);
    const g: Group = {
      sec,
      toggle: sec.querySelector(".tg")!,
      nameEl: sec.querySelector("b")!,
      hostEl: sec.querySelector(".hs")!,
      branchEl: sec.querySelector(".br")!,
      dcEl: sec.querySelector(".dc")!,
      sumEl: sec.querySelector(".ty")!,
      add: sec.querySelector(".ad")!,
      ul: sec.querySelector("ul")!,
      target: undefined,
    };
    g.toggle.addEventListener("click", () => {
      if (collapsed.has(key)) collapsed.delete(key);
      else collapsed.add(key);
      writeCollapsed(collapsed);
      render();
    });
    g.add.addEventListener("click", () => g.target && actions.createAt(g.target));
    groups.set(key, g);
    return g;
  }

  /** 把 node 放到 parent 的第 i 个位置（已在原位时不动，避免打断正在改名的输入框） */
  const place = (parent: HTMLElement, node: HTMLElement, i: number) => {
    if (parent.children[i] !== node) parent.insertBefore(node, parent.children[i] ?? null);
  };

  function renderGroup(g: Group, w: Workspace, rows: SessionInfo[], open: boolean) {
    g.nameEl.textContent = w.name;
    g.toggle.title = w.dir;
    g.toggle.setAttribute("aria-expanded", String(open));
    g.sec.classList.toggle("closed", !open);
    g.hostEl.textContent = w.host ? `@${w.host}` : "";
    // 远程的仓库根与分支来自改动查询，还没查到时不显示（也不标非仓库）
    g.branchEl.textContent = w.branch ?? (w.host ? "" : t("sidebar.notRepo"));
    const n = store.get().changes.get(w.key)?.files.length ?? 0;
    g.dcEl.textContent = n ? `Δ${n}` : "";
    g.dcEl.title = n ? tn("changes.groupTitle", n) : "";
    const sum = summarize(w.sessions);
    g.sumEl.replaceChildren(
      ...(
        [
          ["run", "●", sum.run],
          ["wait", "▲", sum.wait],
          ["done", "✓", sum.done],
          ["shell", "○", sum.shell],
        ] as const
      )
        .filter(([, , c]) => c > 0)
        .map(([cls, ch, c]) => Object.assign(document.createElement("span"), { className: `s-${cls}`, textContent: `${ch}${c}` })),
    );
    g.target = w.target;
    g.add.hidden = !w.target;
    g.add.title = t("sidebar.newInGroup", { name: w.name });
    rows.forEach((s, i) => {
      const v = items.get(s.id) ?? addItem(s);
      renderItem(v, s, w.dir);
      place(g.ul, v.li, i);
    });
    while (g.ul.children.length > rows.length) g.ul.lastElementChild!.remove();
  }

  function renderItem(v: Item, s: SessionInfo, dir: string) {
    const st = rowState(s);
    v.glyph.className = `g ${st}`;
    v.glyph.textContent = GLYPH[st];
    if (!v.nameEl.querySelector("input")) v.nameEl.textContent = displayName(s);
    v.subEl.textContent = relSubdir(s.cwd, dir);
    v.takenEl.hidden = !s.takenOver;
    v.stateEl.className = `ss ${st}`;
    v.stateEl.textContent = st === "background" && s.claude?.count ? `${t("sidebar.state.background")}×${s.claude.count}` : t(`sidebar.state.${st}`);
    // 显示的是对话标题时，悬停提示附上原名
    const shown = displayName(s);
    const orig = shown === s.name ? "" : `\n${t("sidebar.originalName", { name: s.name })}`;
    v.li.title = `${shown} · ${t(`sidebar.state.${st}`)}${orig}\n${s.host ? `${s.host}:` : ""}${s.cwd}${s.claude?.state === "waiting" && s.claude.message ? `\n${s.claude.message}` : ""}`;
    v.li.classList.toggle("exited", st === "exited");
  }

  /** 按 store 中的会话、仓库与筛选关键字重建分组（行元素复用） */
  function render() {
    const { sessions, roots } = store.get();
    // 首屏：等本地会话的仓库查询都回来后再分组，避免先按目录分组再跳到按仓库分组
    if (!shown) {
      if (sessions.some((s) => !s.host && !roots.has(s.cwd))) {
        waitTimer ??= setTimeout(() => {
          shown = true;
          render();
        }, ROOTS_WAIT_MS);
        return;
      }
      shown = true;
      clearTimeout(waitTimer);
    }
    const query = filter.value;
    const ids = new Set(sessions.map((s) => s.id));
    for (const [id, item] of items) {
      if (ids.has(id)) continue;
      item.li.remove();
      items.delete(id);
    }
    const ws = groupSessions(sessions, roots);
    const keys = new Set(ws.map((w) => w.key));
    for (const [key, g] of groups) {
      if (keys.has(key)) continue;
      g.sec.remove();
      groups.delete(key);
    }
    let i = 0;
    for (const w of ws) {
      const g = groups.get(w.key) ?? addGroup(w.key);
      const rows = w.sessions.filter((s) => matchSession(s, query));
      // 筛选时只显示有匹配的组，并忽略折叠
      g.sec.hidden = !!query.trim() && !rows.length;
      renderGroup(g, w, rows, !collapsed.has(w.key) || !!query.trim());
      place(list, g.sec, i++);
    }
    empty.textContent = t("sidebar.noMatch");
    empty.hidden = !query.trim() || ws.some((w) => w.sessions.some((s) => matchSession(s, query)));
    place(list, empty, i);
    renderMarks();
    renderMemory(store.get().memory);
  }

  /** 内存占用：daemon 每次推送全量，不在其中的会话（远程 / 已退出）隐藏 */
  function renderMemory(bytes: ReadonlyMap<number, number>) {
    for (const [id, v] of items) {
      const b = bytes.get(id);
      v.memEl.hidden = b === undefined;
      if (b === undefined) continue;
      v.memEl.textContent = formatMemory(b);
      v.memEl.classList.toggle("high", memoryHigh(b));
    }
  }

  function renderMarks() {
    const { activeId, unread } = store.get();
    for (const [id, v] of items) {
      v.li.classList.toggle("on", id === activeId);
      v.li.classList.toggle("unread", unread.has(id));
    }
  }

  // 筛选框：输入即筛选；Esc 清空并回到终端
  filter.addEventListener("input", render);
  filter.addEventListener("keydown", (ev) => {
    if (ev.key !== "Escape") return;
    ev.preventDefault();
    filter.value = "";
    render();
    actions.focusTerminal();
  });
  let unbind: (() => void) | undefined;

  store.select((s) => s.sessions, render);
  store.select((s) => s.roots, render);
  store.select((s) => s.changes, render);
  store.select((s) => s.activeId, renderMarks);
  store.select((s) => s.unread, renderMarks);
  store.select((s) => s.memory, renderMemory);
  onLocaleChange(render);
  return {
    el,
    mount() {
      unbind = keymap.bind("Ctrl+Shift+K", () => {
        filter.focus();
        filter.select();
      });
    },
    unmount: () => unbind?.(),
  };
}
