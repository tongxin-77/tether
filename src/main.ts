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
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";
// WebView2 中 navigator.clipboard.readText 会弹权限询问，统一走剪贴板插件
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { type FsClientMsg, type FsServerMsg, ask, forgetFiles, initFiles, onFsMessage, openPathQuiet, showFiles, showMenu } from "./files";
import { attachFileLinks } from "./file-links";
import { attachTableSelect, cellSelectionText, clearCellSelection } from "./table-select";
import { closePreview, initPreview } from "./preview";
import { afterConnect, beforeConnect, daemonBannerShown, initUpdater, onHello } from "./updater";
import { type HostStatsClientMsg, type HostStatsServerMsg, initHostInfo, onHostStats, showHostInfo } from "./host-info";
import { attachClaudeComplete, setCustomCompletions, type ShellKind } from "./claude-complete";
import { type HubClientMsg, type HubConfig, type HubServerMsg, closeHub, draftHub, fillHub, initHub, onHubMessage } from "./hub";
import { icon } from "./icons";
import { applyTheme, currentTheme, getThemes, nextTheme, registerTerminal, setTransparent, unregisterTerminal, xtermTheme } from "./themes";

// 与 proto crate 的协议类型保持一致；会话数据全部由 daemon 维护，前端只做展示
type Status = "running" | "waiting" | "exited";

/** 会话中 Claude Code 的状态（Claude hooks 上报） */
interface ClaudeState {
  state: "idle" | "thinking" | "waiting" | "background" | "done";
  message?: string;
  /** background 时仍在运行的后台子代理数 */
  count?: number;
  at: number;
}

interface SessionInfo {
  id: number;
  name: string;
  cwd: string;
  command: string;
  status: Status;
  startedAt: number;
  lastActivityAt: number;
  /** 远程会话的主机显示名，本地为 null */
  host: string | null;
  /** 远程会话所用主机 id */
  hostId?: string;
  claude?: ClaudeState;
}

interface Host {
  id: string;
  name: string;
  host: string;
  port: number;
  user: string;
  auth: "key" | "password";
  keyPath?: string;
  /** 来自 ~/.ssh/config 的主机（含其在 Config.hosts 中的覆盖项）为 "sshConfig"，Tether 管理的不带该字段 */
  source?: "sshConfig";
}

type PromptMsg = {
  type: "prompt";
  reqId: number;
  kind: "password" | "passphrase" | "hostKey";
  host: string;
  message: string;
  fingerprint?: string;
};

type Target = { kind: "local"; cwd: string } | { kind: "remote"; hostId: string; cwd?: string };

/** 最近关闭的会话目录，daemon 维护，最新在前 */
interface RecentItem {
  target: Target;
  /** 远程主机显示名（记录时），本地为 null */
  host: string | null;
  closedAt: number;
}

type ServerMsg =
  | { type: "sessions"; sessions: SessionInfo[] }
  | { type: "snapshot" | "output"; id: number; data: string }
  | { type: "notify"; id: number; message: string }
  | { type: "activate"; id: number }
  | { type: "error"; message: string }
  | { type: "config"; config: Config; shells: ShellOption[]; hosts: Host[]; sshHosts: Host[]; passwordSaved: string[]; hubTokenSaved?: boolean }
  | { type: "recent"; items: RecentItem[] }
  | { type: "hello"; version: string; proto: number }
  | { type: "claudeHooks"; path: string; enabled: boolean; outdated: boolean; error: string | null }
  | PromptMsg
  | FsServerMsg
  | HostStatsServerMsg
  | HubServerMsg
  | { type: "testHost"; reqId: number; elapsedMs: number | null; error: string | null };

interface ShellOption {
  name: string;
  command: string;
}

interface Config {
  shell: string;
  /** Tether 管理的主机 + ~/.ssh/config 主机的覆盖项 */
  hosts: Host[];
  appearance: Appearance;
  /** 弹系统通知时播放提示音 */
  sound: boolean;
  /** 用户自定义联想命令 */
  completions: CompletionItem[];
  /** 管理服务器设置（旧 daemon 无此字段） */
  hub?: HubConfig;
}

interface CompletionItem {
  text: string;
  desc: string;
}

/** 外观：窗口透明度 */
interface Appearance {
  /** 窗口背景不透明度（%），30–100 */
  opacity: number;
}

type ClientMsg =
  | { type: "list" }
  | { type: "create"; target: Target; cols: number; rows: number; command?: string }
  | { type: "attach" | "close"; id: number }
  | { type: "input"; id: number; data: string }
  | { type: "resize"; id: number; cols: number; rows: number }
  | { type: "rename"; id: number; name: string }
  | { type: "focus"; activeId: number | null; windowFocused: boolean }
  | { type: "getConfig" }
  | { type: "setConfig"; config: Config }
  | { type: "setHostPassword"; hostId: string; password: string | null }
  | { type: "promptReply"; reqId: number; value: string | null; save: boolean }
  | { type: "removeRecent"; target: Target }
  | { type: "openUrl"; url: string }
  | { type: "getClaudeHooks" }
  | { type: "setClaudeHooks"; enabled: boolean }
  | { type: "playSound" }
  | FsClientMsg
  | HostStatsClientMsg
  | HubClientMsg
  | { type: "testHost"; reqId: number; host: Host; password: string | null; useSaved: boolean };

/** 每个会话一个 xterm，切换时只隐藏不销毁 */
interface View {
  info: SessionInfo;
  term: Terminal;
  fit: FitAddon;
  box: HTMLDivElement;
  item: HTMLLIElement;
  nameEl: HTMLSpanElement;
  hostEl: HTMLSpanElement;
  cwdEl: HTMLSpanElement;
  claudeEl: HTMLSpanElement;
}

const views = new Map<number, View>();
let activeId: number | undefined;
/** 独立窗口模式（URL ?detached=<id>）：只显示这一个会话，不连接 daemon（共用主窗口建立的连接），不处理配置 / 提示等 */
const detachedId = (() => {
  const v = new URLSearchParams(location.search).get("detached");
  return v === null ? undefined : Number(v);
})();
/** 主窗口：已拖出为独立窗口的会话，不在列表中显示；独立窗口关闭后移出 */
const detached = new Set<number>();
/** 独立窗口：当前窗口标题（会话名），改名时同步 */
let shownTitle = "";
// 收到 activate / --session 时会话可能还未出现在列表中，先记下
let pendingActivate: number | undefined;
/** daemon 返回的合并主机列表（~/.ssh/config 主机已应用覆盖项），用于新建菜单 */
let hosts: Host[] = [];
/** 是否已收到过 config：之前 hosts 为空，不据此把远程记录判为“主机已删除” */
let hostsLoaded = false;
let recent: RecentItem[] = [];
/** 收到首个会话列表前不显示空状态，避免有会话时启动闪一下 */
let sessionsLoaded = false;
/** ~/.ssh/config 原始解析结果，设置面板中无覆盖项时显示 */
let sshHosts: Host[] = [];
/** 设置面板中编辑中的 Config.hosts（Tether 管理主机 + ssh config 覆盖项），点保存时随配置提交 */
let draftHosts: Host[] = [];
/** 正在编辑的主机，undefined 为新增 */
let editingHost: Host | undefined;
/** 凭据管理器中有已保存密码的主机 id（daemon 下发，不含密码） */
let savedPasswords = new Set<string>();
/** 设置面板中待提交的密码改动：主机 id → 新密码 / null（清除）；点设置“保存”时在 setConfig 之后逐条发送，取消则丢弃 */
const pendingPasswords = new Map<string, string | null>();
/** 主机对话框中点了“清除已保存密码” */
let clearPassword = false;
/** 主机对话框中进行中的连接测试：left 为剩余等待时间（毫秒），等待用户确认指纹 / 输入口令期间不扣减 */
let hostTest: { reqId: number; left: number; timer: number } | undefined;
/** 与文件树一样错开 reqId 起点 */
let hostTestSeq = Math.floor(Math.random() * 2 ** 40);
/** 等待测试结果的最长时间（daemon 测试限时 10 秒）；超时视为不认识该消息的旧版 daemon */
const HOST_TEST_TIMEOUT = 15000;
/** 已保存的外观（daemon 下发）；设置面板中的改动先实时预览，取消时回到它 */
let savedAppearance: Appearance = { opacity: 100 };
/** 设置面板中编辑中的外观 */
let draftAppearance: Appearance = { ...savedAppearance };
/** 设置面板中编辑中的自定义联想命令 */
let draftCompletions: CompletionItem[] = [];
/** 配置中的默认 shell（Config.shell），会话命令无法识别 shell 类型时据此判断 */
let defaultShell = "auto";
const promptQueue: PromptMsg[] = [];
let currentPrompt: PromptMsg | undefined;
/** 当前生效（含预览中）的背景不透明度，0.3–1 */
let shownAlpha = 1;
/** daemon 探测到 PowerShell 7（shells 中有该项）；收到 config 前视为已安装，不显示提示 */
let pwshDetected = true;
const PWSH_NOTICE_KEY = "tether.pwshNotice.dismissed";
const PWSH_INSTALL_CMD = "winget install --id Microsoft.PowerShell -e --source winget";
/** 卡片标题行中本地 shell 的显示名（按程序文件名，不含 .exe） */
const SHELL_NAMES: Record<string, string> = {
  pwsh: "PowerShell 7",
  powershell: "Windows PowerShell",
  cmd: "命令提示符",
  bash: "Git Bash",
  wsl: "WSL",
};
/** 双击选词的分隔符：空白、引号、括号、逗号及常见 shell 符号；不含 \ / : . 等，Windows 路径与 URL 可整体选中 */
const WORD_SEPARATOR = " ()[]{}<>'\"`,;|，。；（）【】「」《》";
let copiedTimer: number | undefined;
/** Claude Code 的 settings.json 路径（daemon 解析），用于确认提示 */
let claudeSettingsPath = "";
const CLAUDE_LABELS: Record<ClaudeState["state"], string> = {
  idle: "Claude",
  thinking: "思考中",
  waiting: "等待确认",
  background: "子代理运行中",
  done: "已完成",
};

const list = document.getElementById("session-list") as HTMLUListElement;
const terminals = document.getElementById("terminals") as HTMLElement;
const settings = document.getElementById("settings") as HTMLDialogElement;
const shellSelect = document.getElementById("shell-select") as HTMLSelectElement;
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const newMenu = $<HTMLDivElement>("new-menu");
const hostList = $<HTMLUListElement>("host-list");
const hostDialog = $<HTMLDialogElement>("host-dialog");
const promptDialog = $<HTMLDialogElement>("prompt-dialog");
const emptyPanel = $<HTMLDivElement>("empty");
const recentList = $<HTMLUListElement>("recent-list");
const opacityRange = $<HTMLInputElement>("opacity-range");
const soundCheck = $<HTMLInputElement>("sound-check");
const completionList = $<HTMLUListElement>("completion-list");

const send = (msg: ClientMsg) => invoke("send", { msg });
const decode = (b64: string) => Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));

// 新建菜单：本地 + 每台主机一项
document.getElementById("new-btn")!.addEventListener("click", (ev) => {
  ev.stopPropagation();
  if (!newMenu.hidden) {
    newMenu.hidden = true;
    return;
  }
  const item = (ico: "folder-open" | "server", text: string, sub: string, action: () => void) => {
    const b = document.createElement("button");
    b.innerHTML = `<span class="mi-ico">${icon(ico)}</span><span class="mi-text"></span>`;
    const t = b.querySelector(".mi-text")!;
    t.append(text);
    if (sub) t.appendChild(document.createElement("small")).textContent = sub;
    b.addEventListener("click", () => {
      newMenu.hidden = true;
      action();
    });
    return b;
  };
  newMenu.replaceChildren(
    item("folder-open", "本地目录", "选择一个文件夹打开", createSession),
    ...hosts.map((h) => item("server", h.name, `${h.user}@${h.host}`, () => createRemote(h))),
  );
  newMenu.hidden = false;
});
document.addEventListener("click", () => (newMenu.hidden = true));

// 主题：设置面板下拉框从注册表生成，侧栏按钮轮换；切换立即生效并记在本机（不随设置“取消”回退）
const themeSelect = $<HTMLSelectElement>("theme-select");
themeSelect.replaceChildren(...getThemes().map((t) => new Option(t.label, t.id)));
const syncThemeUi = () => {
  themeSelect.value = currentTheme().id;
  $("theme-btn").title = `切换主题（当前：${currentTheme().label}）`;
};
themeSelect.addEventListener("change", () => {
  applyTheme(themeSelect.value);
  syncThemeUi();
});
$("theme-btn").addEventListener("click", () => {
  nextTheme();
  syncThemeUi();
});
syncThemeUi();

// 设置对话框：打开时向 daemon 取最新配置，收到 config 消息时回填
document.getElementById("settings-btn")!.addEventListener("click", () => {
  send({ type: "getConfig" });
  send({ type: "getClaudeHooks" });
  // Esc 关闭不会改 returnValue，清掉上次的值以免误保存
  settings.returnValue = "";
  settings.showModal();
});
settings.addEventListener("close", () => {
  const shell = shellSelect.value;
  const save = settings.returnValue === "save" && !!shell;
  if (save) {
    send({ type: "setConfig", config: { shell, hosts: draftHosts, appearance: draftAppearance, sound: soundCheck.checked, completions: draftCompletions.filter((c) => c.text.trim()), hub: draftHub() } });
    // daemon 按顺序处理：先保存配置（账户变化时迁移已保存密码），再按主机 id 写入 / 清除密码
    for (const [hostId, password] of pendingPasswords) send({ type: "setHostPassword", hostId, password });
  } else applyAppearance(savedAppearance);
  // hub token 改动同样在 setConfig 之后提交
  closeHub(save);
  pendingPasswords.clear();
  activeView()?.term.focus();
});

// 外观：拖动滑块只改草稿并实时预览，点保存才持久化
opacityRange.addEventListener("input", () => {
  draftAppearance.opacity = Number(opacityRange.value);
  previewAppearance();
});

// 试听：不受提示音开关影响
$("sound-test").addEventListener("click", () => send({ type: "playSound" }));

// PowerShell 7 安装提示：安装 = 新建本地会话运行 winget（用户可见进度、可响应 UAC / 协议确认）
$("pwsh-install").addEventListener("click", () => createAt({ kind: "local", cwd: "" }, PWSH_INSTALL_CMD));
$("pwsh-dismiss").addEventListener("click", () => {
  try {
    localStorage.setItem(PWSH_NOTICE_KEY, "1");
  } catch {}
  renderPwshNotice();
});

// Claude Code 集成：由 daemon 修改 settings.json（写前备份），写入前确认
for (const enabled of [true, false]) {
  $(enabled ? "cc-enable" : "cc-remove").addEventListener("click", async () => {
    const msg = enabled
      ? `将在 ${claudeSettingsPath} 中添加 Tether 的 hooks（SessionStart / UserPromptSubmit / Notification / Stop / SubagentStart / SubagentStop / SessionEnd），写入前会备份原文件。继续？`
      : `将从 ${claudeSettingsPath} 中移除 Tether 添加的 hooks（不影响其它 hooks），写入前会备份原文件。继续？`;
    if ((await ask(msg, enabled ? "启用" : "移除")) === null) return;
    send({ type: "setClaudeHooks", enabled });
  });
}

$("host-add").addEventListener("click", () => editHost(undefined));
$("completion-add").addEventListener("click", () => {
  draftCompletions.push({ text: "", desc: "" });
  renderCompletions();
  completionList.querySelector<HTMLInputElement>("li:last-child input")?.focus();
});
$("h-auth").addEventListener("change", renderPasswordRow);
$("h-pw-clear").addEventListener("click", () => {
  clearPassword = true;
  $<HTMLInputElement>("h-password").value = "";
  renderPasswordRow();
});
$("h-password").addEventListener("input", renderPasswordRow);
$("h-test").addEventListener("click", testHost);
hostDialog.addEventListener("close", () => {
  endHostTest();
  const pwInput = $<HTMLInputElement>("h-password");
  const password = $<HTMLSelectElement>("h-auth").value === "password" ? pwInput.value : "";
  // 密码不留在 DOM 中
  pwInput.value = "";
  if (hostDialog.returnValue !== "save") return;
  const h = hostFromDialog();
  const i = draftHosts.findIndex((d) => d.id === h.id);
  if (i >= 0) draftHosts[i] = h;
  else draftHosts.push(h);
  // 留空不修改已保存的密码；点过清除且未输入新密码则清除
  if (password) pendingPasswords.set(h.id, password);
  else if (clearPassword) pendingPasswords.set(h.id, null);
  renderHosts();
});

promptDialog.addEventListener("close", () => {
  const p = currentPrompt!;
  const ok = promptDialog.returnValue === "ok";
  const input = $<HTMLInputElement>("p-input");
  const value = p.kind === "hostKey" ? "yes" : input.value;
  const save = ok && p.kind === "password" && $<HTMLInputElement>("p-save").checked;
  send({ type: "promptReply", reqId: p.reqId, value: ok ? value : null, save });
  input.value = "";
  currentPrompt = undefined;
  if (promptQueue.length) showPrompt();
  else activeView()?.term.focus();
});

initFiles({ send, active: () => activeView()?.info, newSession: createAt });
initHostInfo({ send, active: () => activeView()?.info });
initHub({ send });
initPreview({
  openUrl: (url) => send({ type: "openUrl", url }),
  onClose: () => {
    const v = activeView();
    v?.fit.fit();
    v?.term.focus();
  },
});

$("empty-new").addEventListener("click", createSession);
// 空状态显示期间每分钟刷新一次相对时间
setInterval(() => !emptyPanel.hidden && renderEmpty(), 60_000);

// 容器尺寸变化时重新 fit 当前会话（含文件树展开 / 收起导致的宽度变化）；fit 改变行列后由 term.onResize 同步给 daemon
new ResizeObserver(() => activeView()?.fit.fit()).observe(terminals);

// 激活会话 + 窗口焦点上报给 daemon，用于判定是否弹系统通知。daemon 只记一份焦点状态，多个窗口时：
// 获得焦点立即上报并广播 gui-focus；失焦延迟上报，期间另一个本应用窗口获得焦点则取消，避免覆盖它的上报
let blurTimer: number | undefined;
window.addEventListener("focus", () => {
  clearTimeout(blurTimer);
  reportFocus();
  emit("gui-focus");
});
window.addEventListener("blur", () => {
  clearTimeout(blurTimer);
  blurTimer = setTimeout(() => reportFocus(true), 100);
});
listen("gui-focus", () => {
  if (!document.hasFocus()) clearTimeout(blurTimer);
});
if (detachedId !== undefined) document.body.classList.add("detached");
initTitlebar();

init();

/** 自定义标题栏的窗口按钮；拖动与双击最大化由 data-tauri-drag-region 处理。最大化状态变化（含双击、Win+↑）时切换图标 */
function initTitlebar() {
  const win = getCurrentWindow();
  const maxBtn = $("win-max");
  $("win-min").innerHTML = icon("minus");
  $("win-close").innerHTML = icon("x");
  $("win-min").addEventListener("click", () => win.minimize());
  maxBtn.addEventListener("click", () => win.toggleMaximize());
  $("win-close").addEventListener("click", () => win.close());
  const sync = async () => {
    const max = await win.isMaximized();
    maxBtn.innerHTML = icon(max ? "copy" : "square");
    maxBtn.title = max ? "还原" : "最大化";
  };
  sync();
  win.onResized(sync);
}

// 用 daemon 探测到的 shell 填充下拉框；当前配置不在列表中（手改过 config.json）时额外列出并选中，保证不丢失
function fillShells(current: string, shells: ShellOption[]) {
  const options = shells.map((s) => new Option(s.name, s.command));
  if (current && !shells.some((s) => s.command === current)) options.push(new Option(current, current));
  shellSelect.replaceChildren(...options);
  shellSelect.value = current;
}

async function init() {
  // xterm 打开时按当前字体测量字符格，先等打包的等宽字体就绪（失败则用回退字体）
  await Promise.all(["400", "700"].map((w) => document.fonts.load(`${w} 14px "JetBrains Mono"`))).catch(() => {});
  // 先注册监听再连接，避免丢失早到的消息
  await listen<ServerMsg>("daemon", ({ payload: m }) => {
    if (detachedId !== undefined) return onDetachedMsg(m);
    switch (m.type) {
      case "sessions":
        return syncSessions(m.sessions);
      case "snapshot":
      case "output":
        return views.get(m.id)?.term.write(decode(m.data));
      case "notify":
        if (m.id !== activeId) views.get(m.id)?.item.classList.add("unread");
        return;
      case "activate":
        return focusSession(m.id);
      case "error":
        return alert(m.message);
      case "config":
        fillShells(m.config.shell, m.shells);
        pwshDetected = m.shells.some((s) => s.name === "PowerShell 7");
        renderPwshNotice();
        savedAppearance = m.config.appearance;
        draftAppearance = { ...savedAppearance };
        previewAppearance();
        soundCheck.checked = m.config.sound;
        defaultShell = m.config.shell;
        setCustomCompletions(m.config.completions);
        draftCompletions = m.config.completions.map((c) => ({ ...c }));
        renderCompletions();
        hosts = m.hosts;
        sshHosts = m.sshHosts;
        draftHosts = m.config.hosts.map((h) => ({ ...h }));
        savedPasswords = new Set(m.passwordSaved);
        fillHub(m.config.hub, m.hubTokenSaved);
        hostsLoaded = true;
        renderEmpty();
        return renderHosts();
      case "recent":
        recent = m.items;
        return renderEmpty();
      case "hello":
        return onHello(m.version, m.proto);
      case "claudeHooks":
        claudeSettingsPath = m.path;
        // 旧版只写了部分事件：提示更新，“启用”按钮改为“更新”（重新启用即补齐）
        $("cc-status").textContent = m.outdated ? "需要更新" : m.enabled ? "已启用" : "未启用";
        $("cc-status").classList.toggle("on", m.enabled && !m.outdated);
        $("cc-status").classList.toggle("outdated", m.outdated);
        $("cc-enable").textContent = m.outdated ? "更新" : "启用";
        $("cc-path").textContent = m.path;
        if (m.error) alert(m.error);
        return;
      case "prompt":
        promptQueue.push(m);
        if (!currentPrompt) showPrompt();
        return;
      case "fsEntries":
      case "fsContent":
      case "fsResult":
      case "transfer":
        return onFsMessage(m);
      case "hostStats":
        return onHostStats(m);
      case "testHost":
        return onTestHost(m.reqId, m.elapsedMs, m.error);
      case "hubStatus":
      case "testHub":
        return onHubMessage(m);
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
    pendingActivate = (await invoke<number | null>("connect")) ?? undefined;
  } catch (err) {
    alert(`连接后台进程失败：${err}`);
    return;
  }
  afterConnect();
  requestState();
  initUpdater(() => views.size + detached.size, syncBannerSpace);
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
      if (s.name !== shownTitle) getCurrentWindow().setTitle((shownTitle = s.name));
      return syncSessions([s]);
    }
    case "snapshot":
    case "output":
      return views.get(m.id)?.term.write(decode(m.data));
    case "activate":
      if (m.id === detachedId) focusSession(m.id);
      return;
    case "config":
      defaultShell = m.config.shell;
      setCustomCompletions(m.config.completions);
      savedAppearance = m.config.appearance;
      return applyAppearance(savedAppearance);
    case "fsEntries":
    case "fsContent":
    case "fsResult":
    case "transfer":
      return onFsMessage(m);
  }
}

/** 连接建立后向 daemon 取会话列表与配置，并上报焦点 */
function requestState() {
  send({ type: "list" });
  send({ type: "getConfig" });
  reportFocus();
}

/** 断开后的重连间隔（毫秒），用完后一直按最后一项重试；connect 本身会在管道不存在时拉起 tetherd 并等待约 5 秒 */
const RECONNECT_DELAYS = [1000, 2000, 5000];
/** 每次断开加一；新连接刚建立又断开时，由新一轮处理负责提示条与重连 */
let disconnects = 0;

/** 与 daemon 的连接断开（崩溃 / 被结束）：清空全部会话视图（daemon 重启后会话已不存在，且 id 从头分配，旧视图不能复用），
 *  显示断开提示并按退避重连；重连后像启动时一样重新取列表与配置，界面以新 daemon 的状态为准 */
async function onDaemonClosed() {
  const epoch = ++disconnects;
  for (const id of [...views.keys()]) removeView(id);
  sessionsLoaded = false;
  pendingActivate = undefined;
  promptQueue.length = 0;
  if (promptDialog.open) promptDialog.close();
  renderEmpty();
  renderCardHead();
  showFiles();
  showHostInfo();
  $("daemon-banner").hidden = false;
  for (let i = 0; ; i++) {
    await new Promise((r) => setTimeout(r, RECONNECT_DELAYS[Math.min(i, RECONNECT_DELAYS.length - 1)]));
    try {
      beforeConnect();
      await invoke("connect");
      break;
    } catch (err) {
      console.warn("重连后台进程失败", err);
    }
  }
  if (epoch !== disconnects) return;
  $("daemon-banner").hidden = true;
  afterConnect();
  requestState();
}

/** 应用外观：设置背景不透明度 CSS 变量（对话框 / 菜单至少 88%），同步所有已存在终端的背景 */
function applyAppearance(a: Appearance) {
  const root = document.documentElement;
  shownAlpha = Math.min(100, Math.max(30, a.opacity)) / 100;
  root.style.setProperty("--alpha", String(shownAlpha));
  root.style.setProperty("--alpha-strong", String(Math.max(shownAlpha, 0.88)));
  setTransparent(shownAlpha < 1);
}

/** PowerShell 7 安装提示条与设置面板中的提示：未检测到且未点过“不再提示”时显示 */
function renderPwshNotice() {
  let dismissed = false;
  try {
    dismissed = localStorage.getItem(PWSH_NOTICE_KEY) === "1";
  } catch {}
  const banner = $("pwsh-banner");
  const show = !pwshDetected && !dismissed;
  $("pwsh-hint").hidden = pwshDetected;
  if (banner.hidden === !show) return;
  banner.hidden = !show;
  syncBannerSpace();
}

/** 终端区顶部提示条（PowerShell 7 安装 / 后台旧版本）任一显示时内容下移 */
function syncBannerSpace() {
  terminals.classList.toggle("banner", !$("pwsh-banner").hidden || daemonBannerShown());
  // .term 下移不改变 #terminals 尺寸，ResizeObserver 不会触发，手动 fit
  activeView()?.fit.fit();
}

/** 应用草稿外观并回填设置面板控件 */
function previewAppearance() {
  const a = draftAppearance;
  applyAppearance(a);
  opacityRange.value = String(a.opacity);
  $("opacity-val").textContent = `${a.opacity}%`;
}

/** 本地会话：先选目录再创建，取消则不创建 */
async function createSession() {
  const dir = await open({ directory: true });
  if (typeof dir === "string") createAt({ kind: "local", cwd: dir });
}

/** 选主机后直接连接，进入远程 home 目录 */
function createRemote(h: Host) {
  createAt({ kind: "remote", hostId: h.id });
}

/** command 仅本地会话有效：代替配置的 shell 运行 */
function createAt(target: Target, command?: string) {
  // 初始尺寸取当前终端，attach 时会按实际 fit 结果再 resize
  const t = activeView()?.term;
  send({ type: "create", target, cols: t?.cols ?? 80, rows: t?.rows ?? 24, command });
}

/** 设置面板主机列表：Tether 管理的可编辑 / 删除；~/.ssh/config 的可编辑（存为覆盖项），有覆盖项时可恢复默认 */
function renderHosts() {
  const row = (h: Host) => {
    const li = document.createElement("li");
    const span = (cls: string, text: string) => {
      const el = li.appendChild(document.createElement("span"));
      el.className = cls;
      el.textContent = text;
    };
    span("host-name", h.name);
    span("host-addr", `${h.user}@${h.host}:${h.port}`);
    const btn = (text: string, action: () => void, cls = "") => {
      const b = li.appendChild(document.createElement("button"));
      b.type = "button";
      b.className = cls;
      b.textContent = text;
      b.addEventListener("click", action);
    };
    const remove = () => {
      draftHosts = draftHosts.filter((d) => d.id !== h.id);
      // 删除主机时 daemon 随配置删除其已保存密码；恢复默认时账户变了则迁移，未提交的密码改动一并丢弃
      pendingPasswords.delete(h.id);
      renderHosts();
    };
    if (h.source === "sshConfig") span("tag", "ssh config");
    btn("编辑", () => editHost(h));
    if (h.source !== "sshConfig") btn("删除", remove, "danger");
    else if (draftHosts.some((d) => d.id === h.id)) btn("恢复默认", remove);
    return li;
  };
  const own = draftHosts.filter((h) => h.source !== "sshConfig");
  const ssh = sshHosts.map((s) => draftHosts.find((d) => d.id === s.id) ?? s);
  hostList.replaceChildren(...own.map(row), ...ssh.map(row));
}

/** 设置面板：自定义联想命令行（命令 + 说明 + 删除），编辑直接写入草稿 */
function renderCompletions() {
  completionList.replaceChildren(
    ...draftCompletions.map((c) => {
      const li = document.createElement("li");
      const field = (key: keyof CompletionItem, placeholder: string) => {
        const el = li.appendChild(document.createElement("input"));
        el.placeholder = placeholder;
        el.value = c[key];
        el.addEventListener("input", () => (c[key] = el.value));
      };
      field("text", "命令");
      field("desc", "说明");
      const b = li.appendChild(document.createElement("button"));
      b.type = "button";
      b.className = "danger";
      b.textContent = "删除";
      b.addEventListener("click", () => {
        draftCompletions = draftCompletions.filter((d) => d !== c);
        renderCompletions();
      });
      return li;
    }),
  );
}

function editHost(h: Host | undefined) {
  editingHost = h;
  const set = (id: string, v: string) => ($<HTMLInputElement>(id).value = v);
  set("h-name", h?.name ?? "");
  set("h-host", h?.host ?? "");
  set("h-port", String(h?.port ?? 22));
  set("h-user", h?.user ?? "root");
  set("h-auth", h?.auth ?? "key");
  set("h-key", h?.keyPath ?? "");
  set("h-password", "");
  clearPassword = false;
  renderPasswordRow();
  $("h-test-result").hidden = true;
  hostDialog.returnValue = "";
  hostDialog.showModal();
}

/** 主机对话框当前填写的主机配置；ssh config 主机保存为同 id 的覆盖项（source 仍为 sshConfig），不写回 ~/.ssh/config */
function hostFromDialog(): Host {
  const val = (id: string) => $<HTMLInputElement>(id).value.trim();
  const old = editingHost;
  return {
    id: old?.id ?? `h${Date.now().toString(36)}`,
    name: val("h-name"),
    host: val("h-host"),
    port: Number(val("h-port")) || 22,
    user: val("h-user"),
    auth: $<HTMLSelectElement>("h-auth").value as Host["auth"],
    keyPath: val("h-key") || undefined,
    source: old?.source,
  };
}

/** 测试连接：用对话框当前填写的值（不必先保存）。密码与保存时的规则一致：本次填写的 > 点过清除则没有 >
 *  设置面板中未提交的改动 > 凭据管理器中已保存的（由 daemon 按保存时的迁移规则查找） */
function testHost() {
  if (!["h-host", "h-port", "h-user"].every((id) => $<HTMLInputElement>(id).reportValidity())) return;
  const typed = $<HTMLSelectElement>("h-auth").value === "password" ? $<HTMLInputElement>("h-password").value : "";
  const pending = editingHost ? pendingPasswords.get(editingHost.id) : undefined;
  const password = typed || (clearPassword ? null : (pending ?? null));
  const useSaved = !typed && !clearPassword && pending === undefined;
  endHostTest();
  const reqId = ++hostTestSeq;
  const timer = window.setInterval(() => {
    if (!hostTest) return;
    if (!promptDialog.open && !promptQueue.length) hostTest.left -= 500;
    if (hostTest.left <= 0) {
      endHostTest();
      showTestResult(false, "未收到后台响应：后台版本较旧，重启后台后可用");
    }
  }, 500);
  hostTest = { reqId, left: HOST_TEST_TIMEOUT, timer };
  const btn = $<HTMLButtonElement>("h-test");
  btn.disabled = true;
  btn.textContent = "测试中…";
  $("h-test-result").hidden = true;
  send({ type: "testHost", reqId, host: hostFromDialog(), password, useSaved });
}

function onTestHost(reqId: number, elapsedMs: number | null, error: string | null) {
  if (reqId !== hostTest?.reqId) return;
  endHostTest();
  if (error) showTestResult(false, error);
  else showTestResult(true, `连接成功（耗时 ${elapsedMs ?? 0} ms）`);
}

/** 结束等待中的测试（收到结果 / 超时 / 关闭对话框），按钮恢复 */
function endHostTest() {
  if (hostTest) clearInterval(hostTest.timer);
  hostTest = undefined;
  const btn = $<HTMLButtonElement>("h-test");
  btn.disabled = false;
  btn.textContent = "测试连接";
}

function showTestResult(ok: boolean, text: string) {
  const el = $("h-test-result");
  el.textContent = text;
  el.className = ok ? "ok" : "fail";
  el.hidden = false;
}

/** 主机对话框的密码区：认证方式为密码时可输入；有已保存密码（含待提交的改动）时显示状态与清除按钮 */
function renderPasswordRow() {
  const id = editingHost?.id;
  const pending = id === undefined ? undefined : pendingPasswords.get(id);
  const typed = $<HTMLInputElement>("h-password").value !== "";
  // 本次对话框内的操作优先，其次是设置面板中未提交的改动，最后是 daemon 下发的状态
  const saved = typed || (!clearPassword && (pending !== undefined ? pending !== null : !!id && savedPasswords.has(id)));
  const isPassword = $<HTMLSelectElement>("h-auth").value === "password";
  const changed = typed || clearPassword || pending !== undefined;
  $("h-pw-row").hidden = !isPassword && !saved && !changed;
  $("h-pw-label").hidden = !isPassword;
  const status = $("h-pw-status");
  status.textContent = (saved ? "密码：已保存" : "密码：未保存") +(changed ? "（点设置“保存”后生效）" : "");
  status.classList.toggle("on", saved);
  $("h-pw-clear").hidden = !saved;
}

/** 显示队首的交互提示：密码（可保存）/ 私钥口令 / 主机指纹确认 */
function showPrompt() {
  const p = (currentPrompt = promptQueue.shift()!);
  const titles = { password: "输入密码", passphrase: "输入私钥口令", hostKey: "确认主机指纹" };
  $("p-title").textContent = titles[p.kind];
  $("p-host").textContent = p.host;
  $("p-message").textContent = p.message;
  $("p-fingerprint").textContent = p.fingerprint ?? "";
  $("p-fingerprint").hidden = p.kind !== "hostKey";
  $("p-input").hidden = p.kind === "hostKey";
  $("p-save-row").hidden = p.kind !== "password";
  $<HTMLInputElement>("p-save").checked = false;
  $("p-ok").textContent = p.kind === "hostKey" ? "信任并连接" : "确定";
  promptDialog.returnValue = "";
  promptDialog.showModal();
  if (p.kind !== "hostKey") $("p-input").focus();
}

/** 以 daemon 推送的列表为准：增删视图、更新名称与状态 */
function syncSessions(sessions: SessionInfo[]) {
  sessions = sessions.filter((i) => !detached.has(i.id));
  const ids = new Set(sessions.map((i) => i.id));
  for (const id of [...views.keys()]) if (!ids.has(id)) removeView(id);
  for (const info of sessions) {
    const v = views.get(info.id) ?? addView(info);
    // winget 安装会话结束后重新探测 shell，装好了提示条随之消失（设置面板打开时不刷，以免覆盖草稿）
    if (info.command === PWSH_INSTALL_CMD && info.status === "exited" && v.info.status !== "exited" && !settings.open) {
      send({ type: "getConfig" });
    }
    v.info = info;
    if (!v.nameEl.querySelector("input")) v.nameEl.textContent = info.name;
    v.hostEl.textContent = info.host ?? "";
    v.hostEl.hidden = !info.host;
    v.cwdEl.textContent = info.cwd;
    v.cwdEl.title = info.cwd;
    renderClaude(v.claudeEl, info.claude);
    // 状态写到 li 的 class 上，由 CSS 决定状态点颜色
    v.item.classList.remove("running", "waiting", "exited");
    v.item.classList.add(info.status);
  }
  if (pendingActivate !== undefined && views.has(pendingActivate)) {
    activate(pendingActivate);
    pendingActivate = undefined;
  } else if (activeId === undefined && views.size) {
    activate([...views.keys()].pop()!);
  }
  sessionsLoaded = true;
  renderEmpty();
  renderCardHead();
  showFiles();
  showHostInfo();
}

/** 卡片标题行：当前会话名 · shell（远程为 ssh user@host） */
function renderCardHead() {
  const s = activeView()?.info;
  $("ch-name").textContent = s?.name ?? "Tether";
  $("ch-meta").textContent = s ? shellLabel(s) : "";
  $("ch-ico").innerHTML = icon(s?.host ? "server" : "terminal");
  const cwd = $("ch-cwd");
  cwd.innerHTML = s ? icon(s.host ? "globe" : "folder") : "";
  if (s) cwd.append(s.host ? `${s.host}:${s.cwd}` : s.cwd);
  cwd.title = s?.cwd ?? "";
  renderClaude($("ch-claude"), s?.claude);
}

/** Claude 状态胶囊：圆点 + 状态文字（思考中圆点呼吸），等待确认时悬停显示消息；无状态时隐藏 */
function renderClaude(el: HTMLElement, c: ClaudeState | undefined) {
  el.hidden = !c;
  if (!c) return;
  el.className = `cc ${c.state}`;
  el.innerHTML = "<i></i>";
  el.append(`${CLAUDE_LABELS[c.state]}${c.state === "background" && c.count ? ` ×${c.count}` : ""}`);
  el.title = c.message ?? "Claude Code";
}

/** 补全用的 shell 类型：远程为 posix；本地按命令的程序名，识别不了时按默认 shell（auto 为 PowerShell） */
function shellKind(s: SessionInfo): ShellKind {
  if (s.host) return "posix";
  const kind = (command: string): ShellKind | undefined => {
    const m = command.match(/^"([^"]*)"|^(\S*)/);
    const base = ((m?.[1] ?? m?.[2] ?? "").split(/[\\/]/).pop() ?? "").replace(/\.exe$/i, "").toLowerCase();
    if (base === "pwsh" || base === "powershell") return "powershell";
    if (base === "cmd") return "cmd";
    if (["bash", "zsh", "sh", "fish", "wsl", "ssh"].includes(base)) return "posix";
  };
  return kind(s.command) ?? kind(defaultShell) ?? "powershell";
}

function shellLabel(s: SessionInfo) {
  // 远程会话的 command 即 "ssh user@host"
  if (s.host) return s.command;
  const m = s.command.match(/^"([^"]*)"|^(\S*)/);
  const prog = m?.[1] ?? m?.[2] ?? "";
  const base = (prog.split(/[\\/]/).pop() ?? "").replace(/\.exe$/i, "").toLowerCase();
  return SHELL_NAMES[base] ?? base;
}

/** 复制选区（优先表格单元格选区）到系统剪贴板并短暂提示“已复制”；无选区返回 false */
function copySelection(term: Terminal) {
  const text = cellSelectionText(term) || term.getSelection();
  if (!text) return false;
  writeText(text).then(showCopied, (err) => console.error("复制失败", err));
  return true;
}

/** 粘贴剪贴板文本：走 term.paste，开启 bracketed paste 的程序（PSReadLine / Claude Code 等）不会把多行逐行执行。
 *  noText：剪贴板没有文本（为空或是图片）时调用，Ctrl+V 据此把图片粘贴键发给会话（Claude Code 收到后自行读取剪贴板图片） */
async function pasteInto(term: Terminal, noText?: () => void) {
  let text = "";
  try {
    text = await readText();
  } catch {
    // 剪贴板为空或不是文本
  }
  if (text) term.paste(text);
  else noText?.();
  term.focus();
}

function showCopied() {
  const el = $("copied");
  el.hidden = false;
  clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => (el.hidden = true), 1000);
}

/** 复制粘贴快捷键（类 Windows Terminal）：有选区时 Ctrl+C 复制而不发送 ^C；Ctrl+Shift+C 复制；Ctrl+V / Ctrl+Shift+V 粘贴。
 *  返回 false 表示 xterm 不再处理该键；粘贴键在 keydown 上 preventDefault，阻止浏览器原生 paste 事件，避免 xterm 再粘贴一次 */
function clipboardKey(term: Terminal, ev: KeyboardEvent, sendImagePaste: () => void) {
  if (!ev.ctrlKey || ev.altKey || ev.metaKey) return true;
  if (ev.code === "KeyC") {
    if (!ev.shiftKey && !term.hasSelection() && !cellSelectionText(term)) return true;
    ev.preventDefault();
    if (ev.type === "keydown" && copySelection(term)) {
      term.clearSelection();
      clearCellSelection(term);
    }
    return false;
  }
  if (ev.code === "KeyV") {
    ev.preventDefault();
    // 仅 Ctrl+V（不含 Ctrl+Shift+V）在剪贴板无文本时发送图片粘贴键，触发 Claude Code 的图片粘贴
    if (ev.type === "keydown") pasteInto(term, ev.shiftKey ? undefined : sendImagePaste);
    return false;
  }
  return true;
}

/** 无会话时在终端区显示“最近关闭”：点击行新建该目录的会话，× 删除该条；远程主机已不存在的置灰不可点 */
function renderEmpty() {
  $("session-count").textContent = views.size ? String(views.size) : "";
  emptyPanel.hidden = !sessionsLoaded || views.size > 0;
  if (emptyPanel.hidden) return;
  $("recent-head").hidden = !recent.length;
  $("recent-none").hidden = recent.length > 0;
  recentList.replaceChildren(
    ...recent.map((r) => {
      const li = document.createElement("li");
      const span = (cls: string, text: string) => {
        const el = li.appendChild(document.createElement("span"));
        el.className = cls;
        el.textContent = text;
        return el;
      };
      const t = r.target;
      const remote = t.kind === "remote";
      const path = t.cwd ?? "";
      span("recent-icon", remote ? "🌐" : "📁");
      span("recent-path", middleEllipsis(path, 56)).title = path;
      if (remote) span("tag", r.host ?? "");
      const gone = t.kind === "remote" && hostsLoaded && !hosts.some((h) => h.id === t.hostId);
      span("recent-time", gone ? "主机已删除" : relativeTime(r.closedAt));
      const del = span("close", "×");
      del.title = "从列表中删除";
      del.addEventListener("click", (ev) => {
        ev.stopPropagation();
        send({ type: "removeRecent", target: r.target });
      });
      if (gone) li.classList.add("gone");
      else li.addEventListener("click", () => createAt(r.target));
      return li;
    }),
  );
}

/** 过长时保留首尾、中间用 … 省略 */
function middleEllipsis(s: string, max: number) {
  if (s.length <= max) return s;
  const head = Math.ceil((max - 1) / 2);
  return `${s.slice(0, head)}…${s.slice(s.length - (max - 1 - head))}`;
}

function relativeTime(ms: number) {
  const min = Math.floor((Date.now() - ms) / 60_000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;
  if (min < 24 * 60) return `${Math.floor(min / 60)} 小时前`;
  const day = new Date(ms);
  day.setHours(0, 0, 0, 0);
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const days = Math.round((today.getTime() - day.getTime()) / 86_400_000);
  if (days <= 1) return "昨天";
  if (days < 30) return `${days} 天前`;
  return new Date(ms).toLocaleDateString();
}

function addView(info: SessionInfo): View {
  const term = new Terminal({
    fontFamily: '"JetBrains Mono", "Cascadia Mono", Consolas, "Microsoft YaHei UI", "Microsoft YaHei", monospace',
    fontSize: 14,
    cursorBlink: true,
    // 窗口半透明时终端背景透明，由终端卡片提供半透明底色
    allowTransparency: true,
    wordSeparator: WORD_SEPARATOR,
    // 配色来自 themes.ts 的当前主题（cursorAccent 显式给出，背景透明时块光标下的字符仍可见）
    theme: xtermTheme(),
  });
  registerTerminal(term);
  const fit = new FitAddon();
  term.loadAddon(fit);
  const box = document.createElement("div");
  box.className = "term";
  terminals.appendChild(box);
  term.open(box);
  // 可见时 fit 出实际行列，先同步尺寸再 attach，使快照与当前窗口一致
  fit.fit();
  box.classList.add("hidden");
  const id = info.id;
  send({ type: "resize", id, cols: term.cols, rows: term.rows });
  send({ type: "attach", id });

  term.onData((data) => send({ type: "input", id, data }));
  term.onResize(({ cols, rows }) => send({ type: "resize", id, cols, rows }));
  // claude 补全弹出层可见时消费 ↑ / ↓ / Tab / Esc，其余交给复制粘贴处理
  const completeKey = attachClaudeComplete(
    term,
    terminals,
    (data) => send({ type: "input", id, data }),
    () => shellKind(views.get(id)?.info ?? info),
  );
  term.attachCustomKeyEventHandler(
    (ev) =>
      completeKey(ev) &&
      clipboardKey(term, ev, () =>
        // Windows 版 Claude Code 的图片粘贴键是 Alt+V（ESC v），^V 未绑定；远程会话（Linux / macOS）仍为 ^V
        send({ type: "input", id, data: (views.get(id)?.info ?? info).host !== null ? "\x16" : "\x1bv" }),
      ),
  );
  attachTableSelect(term, box);
  // 输出中的文件路径可单击打开；相对路径按会话目录（daemon 只记录启动目录，不跟踪 cd）解析
  attachFileLinks(
    term,
    () => {
      const s = views.get(id)?.info ?? info;
      return { dir: s.cwd, remote: s.host !== null };
    },
    (path) => openPathQuiet(views.get(id)?.info ?? info, path),
  );
  // 右键粘贴，不弹菜单
  box.addEventListener("contextmenu", (ev) => {
    ev.preventDefault();
    pasteInto(term);
  });

  const item = document.createElement("li");
  item.innerHTML = `<span class="ico">${icon(info.host ? "server" : "terminal")}<span class="dot"></span></span><div class="row"><span class="name"></span><span class="close" title="关闭">${icon("x")}</span></div><div class="sub"><span class="host" hidden></span><span class="cc" hidden></span><span class="cwd"></span></div>`;
  const nameEl = item.querySelector(".name") as HTMLSpanElement;
  const hostEl = item.querySelector(".host") as HTMLSpanElement;
  const cwdEl = item.querySelector(".cwd") as HTMLSpanElement;
  const claudeEl = item.querySelector(".cc") as HTMLSpanElement;
  item.addEventListener("click", () => activate(id));
  nameEl.addEventListener("dblclick", () => rename(id));
  // 右键：从该会话的启动目录新建（daemon 不跟踪 cd）；远程会话用同一主机
  item.addEventListener("contextmenu", (ev) => {
    ev.preventDefault();
    const s = views.get(id)?.info ?? info;
    const target: Target | undefined = !s.host ? { kind: "local", cwd: s.cwd } : s.hostId ? { kind: "remote", hostId: s.hostId, cwd: s.cwd } : undefined;
    showMenu(ev.clientX, ev.clientY, [["从此目录新建", () => target && createAt(target), !target]]);
  });
  // 拖出窗口外松开：拖成独立窗口。Tauri 接管了系统拖放（文件拖入），HTML5 拖放不可用，改用指针捕获
  item.addEventListener("pointerdown", (ev) => {
    if (ev.button !== 0 || (ev.target as HTMLElement).closest(".close, input")) return;
    item.setPointerCapture(ev.pointerId);
    let dragging = false;
    const move = (e: PointerEvent) => {
      if (dragging || Math.hypot(e.clientX - ev.clientX, e.clientY - ev.clientY) < 8) return;
      dragging = true;
      item.classList.add("dragging");
    };
    const up = (e: PointerEvent) => {
      item.removeEventListener("pointermove", move);
      item.removeEventListener("pointerup", up);
      item.removeEventListener("pointercancel", up);
      item.classList.remove("dragging");
      const outside = e.clientX < 0 || e.clientY < 0 || e.clientX >= innerWidth || e.clientY >= innerHeight;
      if (dragging && outside && e.type === "pointerup") detach(id, e.screenX, e.screenY);
    };
    item.addEventListener("pointermove", move);
    item.addEventListener("pointerup", up);
    item.addEventListener("pointercancel", up);
  });
  item.querySelector(".close")!.addEventListener("click", async (ev) => {
    ev.stopPropagation();
    // 已退出的会话没有可丢失的东西，直接关闭；其余先确认
    const v = views.get(id);
    if (v && v.info.status !== "exited") {
      const ok = await ask(`关闭会话「${v.info.name}」？其中正在运行的程序将被终止。`, "关闭");
      if (ok === null) return;
    }
    send({ type: "close", id });
  });
  list.appendChild(item);

  const v: View = { info, term, fit, box, item, nameEl, hostEl, cwdEl, claudeEl };
  views.set(id, v);
  return v;
}

function removeView(id: number) {
  const v = views.get(id)!;
  unregisterTerminal(v.term);
  v.term.dispose();
  v.box.remove();
  v.item.remove();
  views.delete(id);
  forgetFiles(id);
  if (activeId !== id) return;
  activeId = undefined;
  closePreview();
  reportFocus();
}

function rename(id: number) {
  const v = views.get(id)!;
  const input = document.createElement("input");
  input.value = v.info.name;
  v.nameEl.replaceChildren(input);
  input.focus();
  input.select();
  let done = false;
  const finish = (save: boolean) => {
    if (done) return;
    done = true;
    const name = input.value.trim();
    v.nameEl.textContent = save && name ? name : v.info.name;
    if (save && name && name !== v.info.name) send({ type: "rename", id, name });
    v.term.focus();
  };
  input.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") finish(true);
    if (ev.key === "Escape") finish(false);
  });
  input.addEventListener("blur", () => finish(true));
  input.addEventListener("click", (ev) => ev.stopPropagation());
}

function activeView() {
  return activeId === undefined ? undefined : views.get(activeId);
}

function activate(id: number) {
  // 预览属于切换前的会话
  if (id !== activeId) closePreview();
  activeId = id;
  for (const v of views.values()) {
    const on = v.info.id === id;
    v.box.classList.toggle("hidden", !on);
    v.item.classList.toggle("active", on);
  }
  const v = views.get(id)!;
  v.item.classList.remove("unread");
  renderCardHead();
  // 隐藏期间容器尺寸为 0，显示后再 fit
  v.fit.fit();
  v.term.focus();
  reportFocus();
  showFiles();
  showHostInfo();
}

/** 主窗口：把会话拖成独立窗口（屏幕坐标 x / y 处）。会话从列表移除；独立窗口关闭后重新取列表，会话回到主窗口 */
function detach(id: number, x: number, y: number) {
  const v = views.get(id);
  // 已退出的会话在独立窗口中会立即关闭，不拖出
  if (!v || v.info.status === "exited") return;
  const win = new WebviewWindow(`session-${id}`, {
    url: `index.html?detached=${id}`,
    title: v.info.name,
    x: Math.max(0, x - 80),
    y: Math.max(0, y - 16),
    width: Math.max(480, terminals.clientWidth),
    height: Math.max(320, terminals.clientHeight),
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
    console.error("创建独立窗口失败", e.payload);
    back();
  });
  win.once("tauri://destroyed", back);
  detached.add(id);
  removeView(id);
  // 重新取列表：由 syncSessions 切到其余会话 / 显示空状态
  send({ type: "list" });
}

/** daemon 要求聚焦（点击通知 / 新建完成）：窗口前置并切到该会话；已拖出的会话由其独立窗口处理 */
function focusSession(id: number) {
  if (detached.has(id)) return;
  const win = getCurrentWindow();
  win.unminimize();
  win.setFocus();
  if (views.has(id)) activate(id);
  else pendingActivate = id;
}

/** 只由有焦点的窗口上报（见 focus / blur 监听）；force 时无论是否有焦点都按本窗口状态上报 */
function reportFocus(force = false) {
  if (!force && !document.hasFocus()) return;
  send({ type: "focus", activeId: activeId ?? null, windowFocused: document.hasFocus() });
}
