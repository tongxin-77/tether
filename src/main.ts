import "@xterm/xterm/css/xterm.css";
// 界面字体随包打包（离线可用），只引入用到的字重的 latin 子集；中文回退到微软雅黑
import "@fontsource/nunito/latin-500.css";
import "@fontsource/nunito/latin-600.css";
import "@fontsource/nunito/latin-700.css";
import "@fontsource/nunito/latin-800.css";
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-500.css";
import "@fontsource/jetbrains-mono/latin-700.css";
import "./styles.css";
// 入口：连接 daemon、把消息路由给各组件，并装配 store / 编排 / 组件 / 布局
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { createApp, readPrefs } from "./app";
import { createChangesRefresher } from "./changes-refresher";
import { setCustomCompletions } from "./claude-complete";
import { initClaudeHistory } from "./claude-history";
import { createFiles } from "./files";
import { createHistoryPanel } from "./git-panel";
import { createGitSource } from "./git-source";
import { createHostInfo } from "./host-info";
import { onHubMessage } from "./hub";
import { applyDom, locale, onLocaleChange, t } from "./i18n";
import { createPreview } from "./preview";
import { initPrompt, pushPrompt, resetPrompts } from "./prompt";
import type { ClientMsg, ServerMsg } from "./protocol";
import { displayName, relativeTime } from "./session-utils";
import { afterConnect, beforeConnect, initUpdater, onHello } from "./updater";
import { createBrand } from "./components/brand";
import { createChangesPanel } from "./components/changes";
import { createCardHead } from "./components/card-head";
import { createSessionList } from "./components/sessions";
import { createSettings } from "./components/settings";
import { createSidebarFoot } from "./components/sidebar-foot";
import { createTerminalArea } from "./components/terminal";
import { createTitleBar } from "./components/titlebar";
import type { Ctx } from "./ui/component";
import { keymap } from "./ui/keymap";
import { applyLayout, type LayoutName, pageSlots, PRESETS } from "./ui/layout";
import { type AppState, createStore } from "./ui/store";

/** 独立窗口模式（URL ?detached=<id>）：只显示这一个会话，不连接 daemon（共用主窗口建立的连接），不处理配置 / 提示等 */
const detachedId = (() => {
  const v = new URLSearchParams(location.search).get("detached");
  return v === null ? undefined : Number(v);
})();
/** 独立窗口：当前窗口标题（会话名），改名时同步 */
let shownTitle = "";

const send = (msg: ClientMsg) => invoke("send", { msg });

// 静态文字随语言刷新（先于各组件的重绘）；主窗口同时告知 daemon（通知等文字随之切换）
onLocaleChange(() => {
  applyDom();
  sendLocale();
});

// ---------------- 装配：store、编排、组件、布局 ----------------
const store = createStore<AppState>({
  sessions: [],
  sessionsLoaded: false,
  activeId: undefined,
  unread: new Set(),
  memory: new Map(),
  roots: new Map(),
  changes: new Map(),
  hosts: [],
  hostsLoaded: false,
  recent: [],
  defaultShell: "auto",
  pwshDetected: true,
  disconnected: false,
  changesOpen: false,
  changesWs: null,
  ...readPrefs(),
});
const app = createApp(store, send);
const gitSource = createGitSource(send);
createChangesRefresher(store, gitSource);
const ctx: Ctx = { store, send, actions: app.actions, keymap };
const terminal = createTerminalArea(ctx);
const cardHead = createCardHead(ctx);
const preview = createPreview(ctx);
const history = createHistoryPanel(ctx);
const hostInfo = createHostInfo(ctx);
const files = createFiles(ctx);
const settings = createSettings(ctx);
const changes = createChangesPanel(ctx, gitSource);
Object.assign(app.parts, { terminal, cardHead, preview, history, hostInfo, files, settings });
const layout: LayoutName = detachedId === undefined ? "main" : "detached";
document.body.dataset.layout = layout;
applyLayout(
  PRESETS[layout],
  { titleBar: createTitleBar(), brand: createBrand(ctx), sessions: createSessionList(ctx), sidebarFoot: createSidebarFoot(ctx), cardHead, terminal, preview, history, hostInfo, files, settings, changes },
  pageSlots(),
);
// overlay 中的静态对话框（恢复 Claude 对话、更新、交互提示）
applyDom();

initClaudeHistory({
  openedIn: (cid) => store.get().sessions.find((s) => s.claudeSession === cid && s.status !== "exited")?.id,
  activate: app.actions.activate,
  resume: (cwd, r) => app.actions.createAt({ kind: "local", cwd }, undefined, r),
  relativeTime,
  filesDir: files.dir,
});
initPrompt(send, () => app.actions.focusTerminal());

init();

/** 主窗口把当前语言告知 daemon（连接后收到 Hello 时与语言变化时）；独立窗口共用主窗口的连接，不发 */
function sendLocale() {
  if (detachedId === undefined) send({ type: "setLocale", locale: locale() });
}

async function init() {
  // xterm 打开时按当前字体测量字符格，先等打包的等宽字体就绪（失败则用回退字体）
  await Promise.all(["400", "700"].map((w) => document.fonts.load(`${w} 14px "JetBrains Mono"`))).catch(() => {});
  // 先注册监听再连接，避免丢失早到的消息
  await listen<ServerMsg>("daemon", ({ payload: m }) => {
    if (detachedId !== undefined) return onDetachedMsg(m);
    switch (m.type) {
      case "sessions":
        return app.syncSessions(m.sessions);
      case "snapshot":
      case "output":
        return terminal.write(m);
      case "notify":
        return app.notify(m.id);
      case "activate":
        return app.focusSession(m.id);
      case "error":
        return alert(m.message);
      case "config":
        settings.onConfig(m);
        setCustomCompletions(m.config.completions);
        return store.set({ hosts: m.hosts, hostsLoaded: true, defaultShell: m.config.shell, pwshDetected: m.shells.some((s) => s.name === "PowerShell 7") });
      case "recent":
        return store.set({ recent: m.items });
      case "hello":
        sendLocale();
        return onHello(m.version, m.proto);
      case "claudeHooks":
        return settings.onClaudeHooks(m);
      case "prompt":
        return pushPrompt(m);
      case "fsEntries":
      case "fsContent":
      case "fsSaved":
      case "fsResult":
      case "transfer":
        return files.onMessage(m);
      case "hostStats":
        return hostInfo.onStats(m);
      case "git":
        return gitSource.onMessage(m);
      case "testHost":
        return settings.onTestHost(m.reqId, m.elapsedMs, m.error);
      case "hubStatus":
      case "testHub":
        return onHubMessage(m);
      case "memory":
        return store.set({ memory: new Map(m.items.map((i) => [i.id, i.bytes])) });
      case "resetModes":
        return terminal.resetModes(m.id);
    }
  });
  // 独立窗口：daemon 断开后会话已不存在，直接关闭；连接由主窗口建立，这里只取状态
  if (detachedId !== undefined) {
    await listen("daemon-closed", () => getCurrentWindow().close());
    return requestState();
  }
  await listen("daemon-closed", onDaemonClosed);
  try {
    beforeConnect();
    app.setPendingActivate((await invoke<number | null>("connect")) ?? undefined);
  } catch (err) {
    alert(t("error.connectDaemon", { err: String(err) }));
    return;
  }
  afterConnect();
  requestState();
  initUpdater(app.sessionCount, terminal.syncBannerSpace);
}

/** 独立窗口只处理自己会话的消息；交互提示、错误提示、设置面板等由主窗口处理 */
function onDetachedMsg(m: ServerMsg) {
  switch (m.type) {
    case "sessions": {
      const s = m.sessions.find((i) => i.id === detachedId);
      // 会话被关闭或已退出：关闭窗口，仍存在的会话回到主窗口列表
      if (!s || s.status === "exited") {
        getCurrentWindow().close();
        return;
      }
      if (displayName(s) !== shownTitle) getCurrentWindow().setTitle((shownTitle = displayName(s)));
      return app.syncSessions([s]);
    }
    case "snapshot":
    case "output":
      return terminal.write(m);
    case "activate":
      if (m.id === detachedId) app.focusSession(m.id);
      return;
    case "resetModes":
      return terminal.resetModes(m.id);
    case "config":
      store.set({ defaultShell: m.config.shell });
      setCustomCompletions(m.config.completions);
      return settings.applySaved(m.config.appearance);
    case "fsEntries":
    case "fsContent":
    case "fsSaved":
    case "fsResult":
    case "transfer":
      return files.onMessage(m);
    case "git":
      return gitSource.onMessage(m);
  }
}

/** 连接建立后向 daemon 取会话列表与配置，并上报焦点 */
function requestState() {
  send({ type: "list" });
  send({ type: "getConfig" });
  app.reportFocus();
}

/** 断开后的重连间隔（毫秒），用完后一直按最后一项重试；connect 本身会在管道不存在时拉起 tetherd 并等待约 5 秒 */
const RECONNECT_DELAYS = [1000, 2000, 5000];
/** 每次断开加一；新连接刚建立又断开时，由新一轮处理负责提示条与重连 */
let disconnects = 0;

/** 与 daemon 的连接断开（崩溃 / 被结束）：清空全部会话（daemon 重启后会话已不存在，且 id 从头分配，旧视图不能复用），
 *  显示断开提示并按退避重连；重连后像启动时一样重新取列表与配置，界面以新 daemon 的状态为准 */
async function onDaemonClosed() {
  const epoch = ++disconnects;
  app.reset();
  resetPrompts();
  for (let i = 0; ; i++) {
    await new Promise((r) => setTimeout(r, RECONNECT_DELAYS[Math.min(i, RECONNECT_DELAYS.length - 1)]));
    try {
      beforeConnect();
      await invoke("connect");
      break;
    } catch (err) {
      console.warn("Failed to reconnect to background service", err);
    }
  }
  if (epoch !== disconnects) return;
  store.set({ disconnected: false });
  afterConnect();
  requestState();
}
