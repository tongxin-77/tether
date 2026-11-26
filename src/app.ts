// 编排：组件之间的联动（激活会话、会话列表同步、拖出独立窗口、焦点上报）与 ctx.actions 的实现。
// 组件之间不互相调用，需要别的组件配合时经这里
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openClaudeHistory } from "./claude-history";
import { t } from "./i18n";
import type { OpenFile } from "./preview";
import type { ClientMsg, SessionInfo } from "./protocol";
import { PWSH_INSTALL_CMD, displayName } from "./session-utils";
import { parseGpu, parseTermFont } from "./term-utils";
import type { TerminalArea } from "./components/terminal";
import type { Actions } from "./ui/component";
import { ask, showMenu } from "./ui/dialogs";
import type { AppState, Store } from "./ui/store";
import type { GitHead } from "./workspace";

/** app 需要调用的各组件接口（装配时填入） */
export interface Parts {
  terminal: TerminalArea;
  preview: { dirty(): boolean; close(): void; requestClose(): Promise<boolean>; show(f: OpenFile, edit?: boolean): Promise<void> };
  history: { open(dir: string): void; close(): void; reload(dir: string): void };
  files: { show(): void; forget(id: number): void; suggestRoot(id: number): void; openPath(s: SessionInfo, path: string): void; preview(s: SessionInfo, path: string): void };
  hostInfo: { show(): void };
  cardHead: { refreshBranch(): void };
  settings: { open(): void; isOpen(): boolean };
}

// 终端字号与 GPU 开关：立即生效并记在本机，其它窗口（拖出的独立窗口）修改时经 storage 事件同步
const TERM_FONT_KEY = "tether.term.fontSize";
const GPU_KEY = "tether.term.gpu";
const readStore = (key: string) => {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
};
const writeStore = (key: string, value: string | null) => {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {}
};

/** 本机记住的终端偏好（store 初值） */
export const readPrefs = () => ({ termFont: parseTermFont(readStore(TERM_FONT_KEY)), gpu: parseGpu(readStore(GPU_KEY)) });

const without = (set: ReadonlySet<number>, ids: number[]) => new Set([...set].filter((x) => !ids.includes(x)));

export function createApp(store: Store<AppState>, send: (msg: ClientMsg) => void) {
  const parts = {} as Parts;
  /** 主窗口：已拖出为独立窗口的会话，不在列表中显示；独立窗口关闭后移出 */
  const detached = new Set<number>();
  /** 收到 activate / --session 时会话可能还未出现在列表中，先记下 */
  let pendingActivate: number | undefined;

  /** 只由有焦点的窗口上报（见 focus / blur 监听）；force 时无论是否有焦点都按本窗口状态上报 */
  function reportFocus(force = false) {
    if (!force && !document.hasFocus()) return;
    send({ type: "focus", activeId: store.get().activeId ?? null, windowFocused: document.hasFocus() });
  }

  function activate(id: number) {
    const cur = store.get().activeId;
    // 预览属于切换前的会话；编辑器有未保存的修改时先确认，放弃后再切换
    if (id !== cur && parts.preview.dirty()) {
      void parts.preview.requestClose().then((ok) => ok && activate(id));
      return;
    }
    if (id !== cur) {
      parts.preview.close();
      parts.history.close();
    }
    store.set({ activeId: id, unread: without(store.get().unread, [id]) });
    parts.cardHead.refreshBranch();
    parts.terminal.activate(id);
    reportFocus();
    parts.files.show();
    parts.hostInfo.show();
  }

  /** 移除会话（关闭 / 拖出 / daemon 断开）：丢弃文件树状态；移除的是当前会话时关闭预览 */
  function drop(ids: number[]) {
    if (!ids.length) return;
    const prev = store.get();
    for (const id of ids) parts.files.forget(id);
    const lostActive = prev.activeId !== undefined && ids.includes(prev.activeId);
    store.set({ sessions: prev.sessions.filter((s) => !ids.includes(s.id)), unread: without(prev.unread, ids), ...(lostActive && { activeId: undefined }) });
    if (!lostActive) return;
    parts.preview.close();
    reportFocus();
  }

  /** 以 daemon 推送的列表为准：增删会话、更新信息，没有当前会话时切到最后出现的 */
  function syncSessions(all: SessionInfo[]) {
    const incoming = all.filter((i) => !detached.has(i.id));
    const byId = new Map(incoming.map((i) => [i.id, i]));
    drop(store.get().sessions.filter((s) => !byId.has(s.id)).map((s) => s.id));
    const prev = store.get().sessions;
    for (const info of incoming) {
      // winget 安装会话结束后重新探测 shell，装好了提示条随之消失（设置面板打开时不刷，以免覆盖草稿）
      const old = prev.find((s) => s.id === info.id);
      if (info.command === PWSH_INSTALL_CMD && info.status === "exited" && old && old.status !== "exited" && !parts.settings.isOpen()) send({ type: "getConfig" });
    }
    // 保持首次出现的先后顺序：已有的按原位置更新，新的追加在后
    const sessions = [...prev.map((s) => byId.get(s.id)!), ...incoming.filter((i) => !prev.some((s) => s.id === i.id))];
    store.set({ sessions, sessionsLoaded: true });
    if (pendingActivate !== undefined && byId.has(pendingActivate)) {
      activate(pendingActivate);
      pendingActivate = undefined;
    } else if (store.get().activeId === undefined && sessions.length) {
      activate(sessions[sessions.length - 1].id);
    }
    parts.files.show();
    parts.hostInfo.show();
  }

  /** 主窗口：把会话拖成独立窗口（屏幕坐标 x / y 处）。会话从列表移除；独立窗口关闭后重新取列表，会话回到主窗口 */
  function detach(id: number, x: number, y: number) {
    const s = store.get().sessions.find((i) => i.id === id);
    // 已退出的会话在独立窗口中会立即关闭，不拖出
    if (!s || s.status === "exited") return;
    const size = parts.terminal.areaSize();
    const win = new WebviewWindow(`session-${id}`, {
      url: `index.html?detached=${id}`,
      title: displayName(s),
      x: Math.max(0, x - 80),
      y: Math.max(0, y - 16),
      width: Math.max(480, size.width),
      height: Math.max(320, size.height),
      transparent: true,
      decorations: false,
    });
    const back = () => {
      detached.delete(id);
      send({ type: "list" });
      // 独立窗口可能带着“有焦点”的上报关闭，按主窗口当前状态覆盖
      reportFocus(true);
    };
    win.once("tauri://error", (e) => {
      console.error("Failed to create detached window", e.payload);
      back();
    });
    win.once("tauri://destroyed", back);
    detached.add(id);
    drop([id]);
    // 重新取列表：由 syncSessions 切到其余会话 / 显示空状态
    send({ type: "list" });
  }

  /** 正在查询仓库的目录 */
  const resolving = new Set<string>();
  /** 查询本地会话启动目录所在的仓库（git_head 只读 .git 文件，不起 git 进程）：all 为 false 时只查新目录。
   *  同一批结果一次写入 store，侧栏据此在首轮结果回来后再按仓库分组 */
  async function resolveRoots(all: boolean) {
    const { sessions, roots } = store.get();
    const dirs = [...new Set(sessions.filter((s) => !s.host).map((s) => s.cwd))].filter((d) => (all || !roots.has(d)) && !resolving.has(d));
    if (!dirs.length) return;
    for (const d of dirs) resolving.add(d);
    const heads = await Promise.all(dirs.map((dir) => invoke<GitHead | null>("git_head", { dir }).catch(() => null)));
    for (const d of dirs) resolving.delete(d);
    const next = new Map(store.get().roots);
    const changed = dirs.filter((d, i) => !next.has(d) || JSON.stringify(next.get(d)) !== JSON.stringify(heads[i]));
    if (!changed.length) return;
    dirs.forEach((d, i) => next.set(d, heads[i]));
    store.set({ roots: next });
  }
  store.select((s) => s.sessions, () => void resolveRoots(false));
  // 反映终端里的 git checkout / git init：窗口获得焦点与每 30 秒重查（窗口隐藏时跳过）
  setInterval(() => !document.hidden && resolveRoots(true), 30_000);

  const actions: Actions = {
    activate,
    focusTerminal: (fit) => parts.terminal.focus(fit),
    createAt(target, command, resume) {
      // 初始尺寸取当前终端，attach 时会按实际 fit 结果再 resize
      const size = parts.terminal.termSize();
      send({ type: "create", target, cols: size?.cols ?? 80, rows: size?.rows ?? 24, command, resume });
    },
    async createLocal() {
      const dir = await open({ directory: true });
      if (typeof dir === "string") actions.createAt({ kind: "local", cwd: dir });
    },
    detach,
    openSettings: () => parts.settings.open(),
    openClaudeHistory: (cwd) => void openClaudeHistory(cwd),
    openHistory: (dir) => parts.history.open(dir),
    reloadHistory: (dir) => parts.history.reload(dir),
    openPreview: (f, edit) => parts.preview.show(f, edit),
    requestClosePreview: () => parts.preview.requestClose(),
    paste: (text) => parts.terminal.paste(text),
    copy: (text) => void writeText(text).then(() => parts.terminal.hint(t("common.copied"))),
    dropOnTerminal: (x, y, paths) => parts.terminal.dropOnTerminal(x, y, paths),
    openPath: (s, path) => parts.files.openPath(s, path),
    previewFile(s, path) {
      // 预览属于当前会话：先切到该会话（已是当前会话时不变）
      if (store.get().activeId !== s.id) activate(s.id);
      if (store.get().activeId === s.id) parts.files.preview(s, path);
    },
    suggestRoot: (id) => parts.files.suggestRoot(id),
    setTermFont(size) {
      writeStore(TERM_FONT_KEY, size === null ? null : String(size));
      store.set({ termFont: size });
    },
    setGpu(on) {
      writeStore(GPU_KEY, on ? "1" : "0");
      store.set({ gpu: on });
    },
    ask,
    showMenu,
  };

  window.addEventListener("storage", (ev) => {
    if (ev.key === TERM_FONT_KEY) store.set({ termFont: parseTermFont(readStore(TERM_FONT_KEY)) });
    if (ev.key === GPU_KEY) store.set({ gpu: parseGpu(readStore(GPU_KEY)) });
  });

  // 激活会话 + 窗口焦点上报给 daemon，用于判定是否弹系统通知。daemon 只记一份焦点状态，多个窗口时：
  // 获得焦点立即上报并广播 gui-focus；失焦延迟上报，期间另一个本应用窗口获得焦点则取消，避免覆盖它的上报
  let blurTimer: number | undefined;
  window.addEventListener("focus", () => {
    clearTimeout(blurTimer);
    reportFocus();
    emit("gui-focus");
    parts.cardHead.refreshBranch();
    void resolveRoots(true);
  });
  window.addEventListener("blur", () => {
    clearTimeout(blurTimer);
    blurTimer = setTimeout(() => reportFocus(true), 100);
  });
  listen("gui-focus", () => {
    if (!document.hasFocus()) clearTimeout(blurTimer);
  });

  return {
    actions,
    parts,
    syncSessions,
    reportFocus,
    /** daemon 要求聚焦（点击通知 / 新建完成）：窗口前置并切到该会话；已拖出的会话由其独立窗口处理 */
    focusSession(id: number) {
      if (detached.has(id)) return;
      const win = getCurrentWindow();
      win.unminimize();
      win.setFocus();
      if (store.get().sessions.some((s) => s.id === id)) activate(id);
      else pendingActivate = id;
    },
    /** 后台通知：非当前会话标为未读 */
    notify(id: number) {
      const s = store.get();
      if (id !== s.activeId && s.sessions.some((i) => i.id === id)) store.set({ unread: new Set([...s.unread, id]) });
    },
    setPendingActivate(id: number | undefined) {
      pendingActivate = id;
    },
    /** 与 daemon 的连接断开：清空全部会话（daemon 重启后会话已不存在，且 id 从头分配，旧视图不能复用） */
    reset() {
      drop(store.get().sessions.map((s) => s.id));
      pendingActivate = undefined;
      store.set({ sessionsLoaded: false, disconnected: true });
      parts.files.show();
      parts.hostInfo.show();
    },
    /** 会话总数（含拖出的），更新提示条文案用 */
    sessionCount: () => store.get().sessions.length + detached.size,
  };
}
