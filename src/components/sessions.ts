// 侧栏会话列表：筛选框 + 按工作区分组的紧凑行。组头可折叠（记在本机）、显示项目名与远程主机（悬停看目录 / 分支 / 未提交文件数）、可在此新建；
// 行：彩色状态字符、名称（未读加粗 / 已退出删除线）、子目录、接管与内存小标签、状态字、悬停显示关闭。
// 单击切换、双击改名、右键在此新建、拖出成独立窗口；组内拖动行 / 拖动组头排序（记在本机）；拖动侧栏右缘调宽；Ctrl+Shift+K 聚焦筛选框，Esc 清空并退出
import { applyDom, onLocaleChange, t, tn } from "../i18n";
import { icon } from "../icons";
import type { SessionInfo, Target } from "../protocol";
import { displayName } from "../session-utils";
import { clampFilesWidth, formatMemory, memoryHigh } from "../term-utils";
import { type Component, type Ctx, html } from "../ui/component";
import { type RowState, type Workspace, groupSessions, matchSession, mergeOrder, moveKey, rekey, relSubdir, rowState, sessionOrderKeys, sortByOrder } from "../workspace";

interface Item {
  li: HTMLLIElement;
  glyph: HTMLSpanElement;
  nameEl: HTMLSpanElement;
  subEl: HTMLSpanElement;
  takenEl: HTMLSpanElement;
  /** 悬停提示（不含内存行，内存行由 renderMemory 拼上） */
  title: string;
  stateEl: HTMLSpanElement;
}

interface Group {
  sec: HTMLElement;
  toggle: HTMLButtonElement;
  nameEl: HTMLElement;
  hostEl: HTMLElement;
  add: HTMLButtonElement;
  ul: HTMLUListElement;
  /** 在此新建的目标（随会话列表更新） */
  target: Target | undefined;
}

const GLYPH: Record<RowState, string> = { thinking: "●", background: "●", waiting: "▲", done: "✓", shell: "○", exited: "○" };
const COLLAPSED_KEY = "tether.ws.collapsed";
/** 拖动排序的记录：分组按工作区 key，会话按 sessionOrderKeys 的键 */
const GROUP_ORDER_KEY = "tether.ws.order";
const SESSION_ORDER_KEY = "tether.ws.sessionOrder";
/** 侧栏宽度（与文件栏同样的拖动方式与上下限） */
const SIDE_W_KEY = "tether.side.width";
/** 首轮仓库查询最多等这么久，超时按启动目录先显示 */
const ROOTS_WAIT_MS = 1500;

function readList(key: string): string[] {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function writeList(key: string, list: Iterable<string>) {
  try {
    localStorage.setItem(key, JSON.stringify([...list]));
  } catch {}
}

/** 拖动排序的放置位置：指示线画在 el 的上沿（before）或下沿（after） */
interface Hit {
  el: HTMLElement;
  key: string;
  after: boolean;
}

export function createSessionList(ctx: Ctx): Component {
  const { store, send, actions, keymap } = ctx;
  const el = html(`<div class="c-sessions">
      <label class="ws-filter"><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg><input id="ws-filter" data-i18n-placeholder="sidebar.filter" autocomplete="off" spellcheck="false" /><kbd>Ctrl+Shift+K</kbd></label>
      <div id="ws-list"></div>
      <div class="side-resize"></div>
    </div>`);
  const filter = el.querySelector<HTMLInputElement>("#ws-filter")!;
  const list = el.querySelector<HTMLElement>("#ws-list")!;
  const empty = document.createElement("p");
  empty.className = "ws-empty";
  const items = new Map<number, Item>();
  const groups = new Map<string, Group>();
  const collapsed = new Set(readList(COLLAPSED_KEY));
  let groupOrder = readList(GROUP_ORDER_KEY);
  let sessionOrder = readList(SESSION_ORDER_KEY);
  /** 上次渲染时的会话 id → 排序键、排好序的分组，拖动时据此算新顺序 */
  let orderKeys = new Map<number, string>();
  let view: Workspace[] = [];
  /** 拖动后松开时吞掉随之而来的单击（不切换会话 / 不折叠组） */
  let swallowClick = false;
  /** 已按仓库分组显示过（之前等首轮仓库查询） */
  let shown = false;
  let waitTimer: number | undefined;

  const infoOf = (id: number) => store.get().sessions.find((s) => s.id === id);

  function addItem(info: SessionInfo): Item {
    const id = info.id;
    const li = document.createElement("li");
    li.className = "r";
    li.innerHTML = `<span class="g"></span><span class="nm"></span><span class="sd"></span><span class="tag taken" data-i18n="sidebar.taken" data-i18n-title="banner.hub.title" hidden></span><span class="ss"></span><span class="close" data-i18n-title="common.close">${icon("x")}</span>`;
    applyDom(li);
    const q = (sel: string) => li.querySelector(sel) as HTMLSpanElement;
    const item: Item = { li, glyph: q(".g"), nameEl: q(".nm"), subEl: q(".sd"), takenEl: q(".taken"), title: "", stateEl: q(".ss") };
    li.addEventListener("click", () => actions.activate(id));
    item.nameEl.addEventListener("dblclick", () => rename(id, item));
    // 右键：从该会话的启动目录新建（daemon 不跟踪 cd）；远程会话用同一主机
    li.addEventListener("contextmenu", (ev) => {
      ev.preventDefault();
      const s = infoOf(id) ?? info;
      const target: Target | undefined = !s.host ? { kind: "local", cwd: s.cwd } : s.hostId ? { kind: "remote", hostId: s.hostId, cwd: s.cwd } : undefined;
      actions.showMenu(ev.clientX, ev.clientY, [[t("sidebar.newHere"), () => target && actions.createAt(target), !target]]);
    });
    // 拖动：在窗口外松开 = 拖成独立窗口（与之前相同）；在本组的行上松开 = 组内排序；别的组不可放置（禁止光标，松开不变）。
    // Tauri 接管了系统拖放（文件拖入），HTML5 拖放不可用，改用指针捕获
    li.addEventListener("pointerdown", (ev) => {
      if (ev.button !== 0 || (ev.target as HTMLElement).closest(".close, input")) return;
      const from = li.closest<HTMLElement>("section.grp")?.dataset.key;
      drag(
        li,
        li,
        ev,
        (x, y) => {
          const el = document.elementFromPoint(x, y);
          const sec = el && list.contains(el) ? el.closest<HTMLElement>("section.grp") : null;
          if (!el || !sec) return null;
          if (sec.dataset.key !== from) return "deny";
          const rows = [...sec.querySelectorAll<HTMLElement>("li.r")];
          const row = el.closest<HTMLElement>("li.r");
          // 行上按上下半区；第一行以上（组头、组间距）→ 第一行之前；行以外的空白 → 最后一行之后
          const hit = row
            ? { el: row, after: y > row.getBoundingClientRect().top + row.offsetHeight / 2 }
            : y < rows[0].getBoundingClientRect().top
              ? { el: rows[0], after: false }
              : { el: rows[rows.length - 1], after: true };
          return hit.el === li ? null : { ...hit, key: hit.el.dataset.key! };
        },
        (hit) => {
          const all = view.flatMap((w) => w.sessions.map((s) => orderKeys.get(s.id)!));
          sessionOrder = moveKey(mergeOrder(sessionOrder, all), li.dataset.key!, hit.key, hit.after);
          writeList(SESSION_ORDER_KEY, sessionOrder);
          render();
        },
        (e) => actions.detach(id, e.screenX, e.screenY),
      );
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

  /** 指针拖动（移动 8px 后开始）：src 半透明，find 按指针位置给出放置位置（null 不放置，"deny" 不可放置）并画指示线；
   *  松开时在窗口外调 outside，否则在放置位置调 commit */
  function drag(src: HTMLElement, handle: HTMLElement, ev: PointerEvent, find: (x: number, y: number) => Hit | "deny" | null, commit: (hit: Hit) => void, outside?: (e: PointerEvent) => void) {
    handle.setPointerCapture(ev.pointerId);
    let dragging = false;
    let hit: Hit | "deny" | null = null;
    const mark = (on: boolean) => hit && hit !== "deny" && hit.el.classList.toggle(hit.after ? "drop-after" : "drop-before", on);
    const move = (e: PointerEvent) => {
      if (!dragging) {
        if (Math.hypot(e.clientX - ev.clientX, e.clientY - ev.clientY) < 8) return;
        dragging = true;
        src.classList.add("dragging");
      }
      mark(false);
      hit = find(e.clientX, e.clientY);
      mark(true);
      src.classList.toggle("deny", hit === "deny");
    };
    const up = (e: PointerEvent) => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
      mark(false);
      src.classList.remove("dragging", "deny");
      if (!dragging) return;
      swallowClick = true;
      setTimeout(() => (swallowClick = false));
      if (e.type !== "pointerup") return;
      const out = e.clientX < 0 || e.clientY < 0 || e.clientX >= innerWidth || e.clientY >= innerHeight;
      if (out) outside?.(e);
      else if (hit && hit !== "deny") commit(hit);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  }

  function addGroup(key: string): Group {
    const sec = html(`<section class="grp">
        <div class="gh"><button class="tg">${icon("chevron-right", "cv")}<b></b><span class="hs"></span></button><button class="ad">${icon("plus")}</button></div>
        <ul></ul>
      </section>`);
    const g: Group = {
      sec,
      toggle: sec.querySelector(".tg")!,
      nameEl: sec.querySelector("b")!,
      hostEl: sec.querySelector(".hs")!,
      add: sec.querySelector(".ad")!,
      ul: sec.querySelector("ul")!,
      target: undefined,
    };
    sec.dataset.key = key;
    g.toggle.addEventListener("click", () => {
      if (collapsed.has(key)) collapsed.delete(key);
      else collapsed.add(key);
      writeList(COLLAPSED_KEY, collapsed);
      render();
    });
    // 拖动组头：调整分组之间的顺序（在窗口外松开不做任何事）
    g.toggle.addEventListener("pointerdown", (ev) => {
      if (ev.button !== 0) return;
      drag(
        sec,
        g.toggle,
        ev,
        (x, y) => {
          const el = document.elementFromPoint(x, y);
          const to = el && list.contains(el) ? el.closest<HTMLElement>("section.grp") : null;
          if (!to || to === sec) return null;
          return { el: to, key: to.dataset.key!, after: y > to.getBoundingClientRect().top + to.offsetHeight / 2 };
        },
        (hit) => {
          groupOrder = moveKey(mergeOrder(groupOrder, view.map((w) => w.key)), key, hit.key, hit.after);
          writeList(GROUP_ORDER_KEY, groupOrder);
          render();
        },
      );
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
    g.toggle.setAttribute("aria-expanded", String(open));
    g.sec.classList.toggle("closed", !open);
    g.hostEl.textContent = w.host ? `@${w.host}` : "";
    // 分支与未提交文件数只放进悬停提示
    const n = store.get().changes.get(w.key)?.files.length ?? 0;
    g.toggle.title = [w.dir, w.branch, n ? tn("changes.groupTitle", n) : ""].filter(Boolean).join("\n");
    g.target = w.target;
    g.add.hidden = !w.target;
    g.add.title = t("sidebar.newInGroup", { name: w.name });
    rows.forEach((s, i) => {
      const v = items.get(s.id) ?? addItem(s);
      v.li.dataset.key = orderKeys.get(s.id);
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
    v.title = `${shown} · ${t(`sidebar.state.${st}`)}${orig}\n${s.host ? `${s.host}:` : ""}${s.cwd}${s.claude?.state === "waiting" && s.claude.message ? `\n${s.claude.message}` : ""}`;
    v.li.classList.toggle("exited", st === "exited");
    setTitle(v, store.get().memory.get(s.id));
  }

  /** 行的悬停提示 + 内存行（仅本地会话，收到 memory 推送后才有） */
  function setTitle(v: Item, b: number | undefined) {
    v.li.title = b === undefined ? v.title : `${v.title}\n${t("hostInfo.memory")} ${formatMemory(b)}${memoryHigh(b) ? t("sidebar.memHigh") : ""}`;
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
    // 排序键变了（改名等）的会话：记录里换成新键，位置不变
    const keys0 = sessionOrderKeys(sessions);
    const before = sessionOrder;
    for (const [id, k] of keys0) {
      const old = orderKeys.get(id);
      if (old !== undefined && old !== k) sessionOrder = rekey(sessionOrder, old, k);
    }
    if (sessionOrder !== before) writeList(SESSION_ORDER_KEY, sessionOrder);
    orderKeys = keys0;
    const ws = sortByOrder(groupSessions(sessions, roots), (w) => w.key, groupOrder).map((w) => ({ ...w, sessions: sortByOrder(w.sessions, (s) => keys0.get(s.id)!, sessionOrder) }));
    view = ws;
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

  /** 内存占用：daemon 每次推送全量，不在其中的会话（远程 / 已退出）提示里不显示内存 */
  function renderMemory(bytes: ReadonlyMap<number, number>) {
    for (const [id, v] of items) setTitle(v, bytes.get(id));
  }

  function renderMarks() {
    const { activeId, unread } = store.get();
    for (const [id, v] of items) {
      v.li.classList.toggle("on", id === activeId);
      v.li.classList.toggle("unread", unread.has(id));
    }
  }

  list.addEventListener(
    "click",
    (ev) => {
      if (!swallowClick) return;
      ev.stopPropagation();
      ev.preventDefault();
    },
    true,
  );

  // 拖动右缘调整侧栏宽度并记住（与文件栏相同的上下限）。--side-w 设在 body 上，顶栏品牌区随之同宽；终端由其 ResizeObserver 重新 fit
  const handle = el.querySelector<HTMLElement>(".side-resize")!;
  const applyWidth = (w: number) => {
    const px = clampFilesWidth(w, window.innerWidth);
    document.body.style.setProperty("--side-w", `${px}px`);
    return px;
  };
  handle.addEventListener("pointerdown", (ev) => {
    ev.preventDefault();
    handle.setPointerCapture(ev.pointerId);
    const startX = ev.clientX;
    const startW = parseFloat(getComputedStyle(document.body).getPropertyValue("--side-w"));
    let w = startW;
    const move = (e: PointerEvent) => (w = applyWidth(startW + e.clientX - startX));
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
      try {
        localStorage.setItem(SIDE_W_KEY, String(w));
      } catch {}
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  });

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
      try {
        const saved = Number(localStorage.getItem(SIDE_W_KEY));
        if (saved) applyWidth(saved);
      } catch {}
      unbind = keymap.bind("Ctrl+Shift+K", () => {
        filter.focus();
        filter.select();
      });
    },
    unmount: () => unbind?.(),
  };
}
