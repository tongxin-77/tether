// 终端区：每个会话一个 xterm（切换时只隐藏不销毁）、终端上方的提示条、无会话时的“最近关闭”空状态与短暂提示
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { invoke } from "@tauri-apps/api/core";
import { attachFileLinks } from "../file-links";
import { dropText } from "../claude-paths";
import { attachTableSelect } from "../table-select";
import { attachClaudeComplete } from "../claude-complete";
import { t, onLocaleChange } from "../i18n";
import type { SessionInfo } from "../protocol";
import { middleEllipsis, PWSH_INSTALL_CMD, relativeTime, shellKind } from "../session-utils";
import { attachClaudeSelectAll, clipboardKey, pasteImage, pasteInto } from "../term-keys";
import { TERMINAL_OPTIONS, clampTermFont, completionEnabled, debounce, modeResetSeq, needsRefresh, nextRootSeen, oscLinkTarget, promptKind, shouldFit, takeoverStale, terminalFontSize, touchRecent, wasReleased, wheelSteps, windowsPtyFor } from "../term-utils";
import { registerTerminal, unregisterTerminal, xtermTheme } from "../themes";
import { type Component, type Ctx, html } from "../ui/component";
import { activeSession, shallow } from "../ui/store";

interface View {
  info: SessionInfo;
  term: Terminal;
  fit: FitAddon;
  box: HTMLDivElement;
  /** 经历过 hub 接管、尚未刷新（接管期间按网页尺寸写入的内容错乱） */
  stale: boolean;
  /** 用户回车后等待新提示符出现，据此判断终端是否变为 root（远程会话） */
  promptCheck: boolean;
  promptTimer?: number;
  /** 终端提示符当前为 root */
  rootSeen: boolean;
  /** 当前加载的 WebGL 渲染（仅显示中的会话加载：WebGL 上下文数量有限） */
  webgl?: WebglAddon;
  /** WebGL 加载失败或上下文丢失：本会话不再尝试，保持 DOM 渲染 */
  gpuFailed?: boolean;
}

export interface TerminalArea extends Component {
  /** 写入会话输出 / 快照 */
  write(m: { type: "snapshot" | "output"; id: number; data: string }): void;
  /** 会话中的 Claude Code 异常退出：复位该终端的鼠标跟踪 / 备用缓冲等模式 */
  resetModes(id: number): void;
  /** 显示该会话的终端（激活时调用，同一会话重复调用也重新 fit 并聚焦） */
  activate(id: number): void;
  focus(fit?: boolean): void;
  paste(text: string): void;
  /** 右上角短暂提示 */
  hint(text: string): void;
  /** 当前终端的行列（新建会话的初始尺寸） */
  termSize(): { cols: number; rows: number } | undefined;
  /** 终端区尺寸（独立窗口的初始大小） */
  areaSize(): { width: number; height: number };
  /** 提示条显隐变化后：内容下移 / 复位并重新 fit */
  syncBannerSpace(): void;
  dropOnTerminal(x: number, y: number, paths: string[]): void;
}

/** 回车后输出停顿这么久（毫秒）才检查提示符 */
const PROMPT_SETTLE = 300;
/** 双击选词的分隔符：空白、引号、括号、逗号及常见 shell 符号；不含 \ / : . 等，Windows 路径与 URL 可整体选中 */
const WORD_SEPARATOR = " ()[]{}<>'\"`,;|，。；（）【】「」《》";
/** WebGL 上下文数量有限（约 16 个），只给最近显示过的 GPU_KEEP 个终端加载（新建上下文与字形纹理较慢，切走时不释放，来回切换不卡），更早的释放 */
const GPU_KEEP = 8;
const PWSH_NOTICE_KEY = "tether.pwshNotice.dismissed";

const decode = (b64: string) => Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));

export function createTerminalArea(ctx: Ctx): TerminalArea {
  const { store, send, actions } = ctx;
  const el = html(`<div class="c-terminal">
      <!-- 复制成功的短暂提示 -->
      <div id="copied" hidden></div>
      <!-- 未检测到 PowerShell 7 时的安装提示（可关闭），位于卡片头下方；显示时终端区内容整体下移 -->
      <div id="pwsh-banner" class="notice" hidden>
        <span class="b-ico"><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z"/><path d="M20 3v4"/><path d="M22 5h-4"/><path d="M4 17v2"/><path d="M5 18H3"/></svg></span>
        <span class="b-text"><b data-i18n="banner.pwsh.title"></b><small data-i18n="banner.pwsh.sub"></small></span>
        <button id="pwsh-install" data-i18n="banner.pwsh.install"></button>
        <button id="pwsh-dismiss" data-i18n="banner.pwsh.dismiss"></button>
      </div>
      <!-- 与后台服务的连接断开（daemon 崩溃 / 被结束）时显示，重连成功后隐藏；覆盖在 PowerShell 提示条之上 -->
      <div id="daemon-banner" class="notice warn" hidden>
        <span class="b-ico"><svg class="i spin" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/></svg></span>
        <span class="b-text"><b data-i18n="banner.daemon.reconnecting"></b></span>
      </div>
      <!-- 当前会话正被 hub 远程接管：提示并可一键收回 -->
      <div id="hub-banner" class="notice warn" hidden>
        <span class="b-ico"><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="3" width="20" height="14" rx="2"/><path d="M8 21h8"/><path d="M12 17v4"/></svg></span>
        <span class="b-text"><b data-i18n="banner.hub.title"></b><small data-i18n="banner.hub.sub"></small></span>
        <button id="hub-reclaim" data-i18n="banner.hub.reclaim"></button>
      </div>
      <!-- 更新后 daemon 仍为旧版本（安装时不打断会话）：提示用户择机重启后台（updater.ts 控制显示） -->
      <div id="daemon-old-banner" class="notice" hidden>
        <span class="b-ico"><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="8" width="18" height="4" rx="1"/><path d="M12 8v13"/><path d="M19 12v7a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-7"/><path d="M7.5 8a2.5 2.5 0 0 1 0-5A4.8 8 0 0 1 12 8a4.8 8 0 0 1 4.5-5 2.5 2.5 0 0 1 0 5"/></svg></span>
        <span id="daemon-old-text" class="b-text"></span>
        <button id="daemon-restart" data-i18n="daemon.restart"></button>
      </div>
      <!-- 无会话时的空状态：最近关闭的目录 -->
      <div id="empty" hidden>
        <div id="empty-box">
          <h2 id="empty-title" data-i18n="empty.title"></h2>
          <p id="empty-sub" data-i18n="empty.sub"></p>
          <button id="empty-new" data-i18n="empty.new"></button>
          <div id="recent-head" data-i18n="empty.recent"></div>
          <ul id="recent-list"></ul>
          <p id="recent-none" data-i18n="empty.recentNone"></p>
        </div>
      </div>
    </div>`);
  const $ = <T extends HTMLElement = HTMLElement>(id: string) => el.querySelector<T>(`#${id}`)!;
  const pwshBanner = $("pwsh-banner");
  const hubBanner = $("hub-banner");
  const emptyPanel = $("empty");
  /** 终端区（槽位）：补全弹出层的定位容器、fit 的尺寸依据 */
  const area = () => el.parentElement!;

  const views = new Map<number, View>();
  /** 已请求刷新、等待快照的会话：快照到达时先清空终端 */
  const refreshing = new Set<number>();
  /** 本机 Windows build 号（读取失败为 null），本地会话的 xterm 据此设置 windowsPty */
  const windowsBuild = invoke<number | null>("windows_build");
  const autoTermFont = terminalFontSize(screen.width, devicePixelRatio);
  const currentTermFont = () => store.get().termFont ?? autoTermFont;
  /** 最近显示过的会话 id，新的在后 */
  let gpuRecent: number[] = [];
  let hintTimer: number | undefined;
  let wheelAcc = 0;

  const activeView = () => {
    const id = store.get().activeId;
    return id === undefined ? undefined : views.get(id);
  };
  const fitActive = () => {
    if (shouldFit(area().clientWidth, area().clientHeight)) activeView()?.fit.fit();
  };
  /** 会话信息以 store 为准；视图已移除时用创建时的 */
  const infoOf = (id: number, fallback: SessionInfo) => views.get(id)?.info ?? fallback;

  // 终端卡片尺寸变化（窗口、文件栏宽度、提示条 / 主机信息条显隐）时重新 fit 当前会话；fit 改变行列后由 term.onResize 同步给 daemon。
  // 尾沿防抖：拖动窗口边缘 / 文件栏宽度期间不逐帧改 pty 尺寸，停下后只 fit 一次；容器过小（最小化等）时跳过，免得 pty 被缩成 2 列 1 行
  const fitObserver = new ResizeObserver(debounce(fitActive, 120));

  function hint(text: string) {
    const h = $("copied");
    h.textContent = text;
    h.hidden = false;
    clearTimeout(hintTimer);
    hintTimer = setTimeout(() => (h.hidden = true), 1000);
  }
  const showCopied = () => hint(t("common.copied"));

  /** 按开关与当前会话加载 / 释放各终端的 WebGL；渲染器切换后单元格尺寸可能变化，当前会话重新 fit */
  function syncGpu() {
    let changed = false;
    const { activeId, gpu } = store.get();
    if (activeId !== undefined) gpuRecent = touchRecent(gpuRecent, activeId, GPU_KEEP);
    for (const v of views.values()) {
      const want = gpu && gpuRecent.includes(v.info.id) && !v.gpuFailed;
      if (want && !v.webgl) {
        const addon = new WebglAddon();
        try {
          v.term.loadAddon(addon);
        } catch (e) {
          console.warn("WebGL renderer unavailable, falling back to DOM", e);
          addon.dispose();
          v.gpuFailed = true;
          continue;
        }
        // 上下文丢失（显卡驱动重置等）：退回 DOM 渲染，本会话不再尝试
        addon.onContextLoss(() => {
          console.warn("WebGL context lost, falling back to DOM");
          v.gpuFailed = true;
          syncGpu();
        });
        v.webgl = addon;
        changed = true;
      } else if (!want && v.webgl) {
        v.webgl.dispose();
        v.webgl = undefined;
        changed = true;
      }
    }
    if (changed) fitActive();
  }

  /** 远程会话：光标行为 root 提示符（变为 root 时仅一次）且文件树未提权时，在文件栏提示同步提权；无法判断时等下一段输出 */
  function checkPrompt(v: View) {
    if (views.get(v.info.id) !== v || v.info.host === null) return;
    const b = v.term.buffer.active;
    if (b.type !== "normal") return;
    const kind = promptKind(b.getLine(b.baseY + b.cursorY)?.translateToString(false, 0, b.cursorX) ?? "");
    if (kind === null) return;
    v.promptCheck = false;
    const r = nextRootSeen(v.rootSeen, kind);
    v.rootSeen = r.seen;
    if (r.hint && !v.info.fsRoot) actions.suggestRoot(v.info.id);
  }

  /** hub 接管结束后刷新终端：重取带历史的快照（调用前已按本地尺寸 resize） */
  function refreshView(v: View) {
    v.stale = false;
    refreshing.add(v.info.id);
    send({ type: "attach", id: v.info.id, history: true });
  }

  function addView(info: SessionInfo): View {
    const term = new Terminal({
      ...TERMINAL_OPTIONS,
      fontFamily: '"JetBrains Mono", "Cascadia Mono", Consolas, "Microsoft YaHei UI", "Microsoft YaHei", monospace',
      fontSize: currentTermFont(),
      cursorBlink: true,
      // 窗口半透明时终端背景透明，由终端卡片提供半透明底色
      allowTransparency: true,
      wordSeparator: WORD_SEPARATOR,
      // 配色来自 themes.ts 的当前主题（cursorAccent 显式给出，背景透明时块光标下的字符仍可见）
      theme: xtermTheme(),
    });
    // 本地会话走 ConPTY：告知 xterm 以匹配其增行 / reflow 行为（build 号 >= 21376 时保留 reflow）；SSH 会话不设
    if (info.host === null) {
      void windowsBuild.then((build) => {
        const pty = windowsPtyFor(info.host, build);
        if (pty) term.options.windowsPty = pty;
      });
    }
    registerTerminal(term);
    const fit = new FitAddon();
    term.loadAddon(fit);
    const box = document.createElement("div");
    box.className = "term";
    el.appendChild(box);
    term.open(box);
    // 备用缓冲区（全屏程序，如 Claude Code 全屏界面）没有回滚历史，滚动条无用，隐藏
    term.buffer.onBufferChange((b) => box.classList.toggle("alt-buf", b.type === "alternate"));
    // 可见时 fit 出实际行列，先同步尺寸再 attach，使快照与当前窗口一致
    fit.fit();
    box.classList.add("hidden");
    const id = info.id;
    send({ type: "resize", id, cols: term.cols, rows: term.rows });
    send({ type: "attach", id });
    const input = (data: string) => send({ type: "input", id, data });

    term.onData((data) => {
      input(data);
      const v = views.get(id);
      if (v && data.includes("\r")) v.promptCheck = true;
    });
    term.onResize(({ cols, rows }) => send({ type: "resize", id, cols, rows }));
    // claude 补全弹出层可见时消费 ↑ / ↓ / Tab / Esc，其余交给复制粘贴处理
    const completeKey = attachClaudeComplete(
      term,
      area(),
      input,
      () => shellKind(infoOf(id, info), store.get().defaultShell),
      // Claude Code 运行中（退出 / SessionEnd 后 daemon 清空 claude）或全屏程序（alternate buffer）时不弹，按键交给程序自己
      () => completionEnabled(infoOf(id, info).claude, term.buffer.active.type),
    );
    const selectAllKey = attachClaudeSelectAll(term, () => infoOf(id, info).claude, input);
    // 全局按键（Esc 关闭最上层菜单 / 面板）先于终端
    term.attachCustomKeyEventHandler(
      (ev) => !ctx.keymap.handles(ev) && completeKey(ev) && selectAllKey(ev) && clipboardKey(term, ev, () => void pasteImage(term, infoOf(id, info), input), showCopied),
    );
    attachTableSelect(term, box);
    // OSC 8 超链接（Claude Code 输出的链接）：xterm 默认用 confirm + window.open，在 webview 中被拦截；
    // 改为左键单击时 http(s) 交给默认浏览器、file:// 在本地会话中按路径打开。拖选结束在链接上时不打开，保证链接文字可选中复制
    term.options.linkHandler = {
      activate: (ev, uri) => {
        if (ev.button !== 0 || term.hasSelection()) return;
        const target = oscLinkTarget(uri);
        if (target && "url" in target) send({ type: "openUrl", url: target.url });
        else if (target && infoOf(id, info).host === null) actions.openPath(infoOf(id, info), target.path);
      },
    };
    // 输出中的文件路径可单击打开；相对路径按会话目录（daemon 只记录启动目录，不跟踪 cd）解析
    attachFileLinks(
      term,
      () => {
        const s = infoOf(id, info);
        return { dir: s.cwd, remote: s.host !== null };
      },
      (path) => actions.openPath(infoOf(id, info), path),
    );
    // 右键粘贴，不弹菜单
    box.addEventListener("contextmenu", (ev) => {
      ev.preventDefault();
      pasteInto(term);
    });
    fitObserver.observe(box);

    const v: View = { info, term, fit, box, stale: false, promptCheck: false, rootSeen: false };
    views.set(id, v);
    return v;
  }

  function removeView(id: number) {
    const v = views.get(id)!;
    fitObserver.unobserve(v.box);
    unregisterTerminal(v.term);
    v.term.dispose();
    v.box.remove();
    views.delete(id);
    gpuRecent = gpuRecent.filter((x) => x !== id);
  }

  /** 以 store 中的会话列表为准：增删终端；hub 接管结束时补发本地尺寸并刷新 */
  function syncViews(sessions: SessionInfo[]) {
    const ids = new Set(sessions.map((i) => i.id));
    for (const id of [...views.keys()]) if (!ids.has(id)) removeView(id);
    for (const info of sessions) {
      const v = views.get(info.id) ?? addView(info);
      // hub 接管结束（网页 detach / 本地收回 / 连接断开）时 pty 还停在网页尺寸，补发一次本地尺寸
      if (wasReleased(v.info.takenOver, info.takenOver)) send({ type: "resize", id: info.id, cols: v.term.cols, rows: v.term.rows });
      // 接管期间终端按网页宽度写入：正显示的会话接管结束即刷新，其余切换过去时再刷新
      v.stale = takeoverStale(v.stale, info.takenOver);
      if (info.id === store.get().activeId && needsRefresh(v.stale, info.takenOver)) refreshView(v);
      v.info = info;
    }
  }

  /** 无会话时在终端区显示“最近关闭”：点击行新建该目录的会话，× 删除该条；远程主机已不存在的置灰不可点 */
  function renderEmpty() {
    const { sessionsLoaded, sessions, recent, hosts, hostsLoaded } = store.get();
    emptyPanel.hidden = !sessionsLoaded || sessions.length > 0;
    if (emptyPanel.hidden) return;
    $("recent-head").hidden = !recent.length;
    $("recent-none").hidden = recent.length > 0;
    $("recent-list").replaceChildren(
      ...recent.map((r) => {
        const li = document.createElement("li");
        const span = (cls: string, text: string) => {
          const e = li.appendChild(document.createElement("span"));
          e.className = cls;
          e.textContent = text;
          return e;
        };
        const tg = r.target;
        const remote = tg.kind === "remote";
        const path = tg.cwd ?? "";
        span("recent-icon", remote ? "🌐" : "📁");
        span("recent-path", middleEllipsis(path, 56)).title = path;
        if (remote) span("tag", r.host ?? "");
        const gone = tg.kind === "remote" && hostsLoaded && !hosts.some((h) => h.id === tg.hostId);
        span("recent-time", gone ? t("empty.hostGone") : relativeTime(r.closedAt));
        const del = span("close", "×");
        del.title = t("empty.removeRecent");
        del.addEventListener("click", (ev) => {
          ev.stopPropagation();
          send({ type: "removeRecent", target: r.target });
        });
        if (gone) li.classList.add("gone");
        else li.addEventListener("click", () => actions.createAt(r.target));
        return li;
      }),
    );
  }

  /** PowerShell 7 安装提示条：未检测到且未点过“不再提示”时显示 */
  function renderPwshNotice() {
    let dismissed = false;
    try {
      dismissed = localStorage.getItem(PWSH_NOTICE_KEY) === "1";
    } catch {}
    const show = !store.get().pwshDetected && !dismissed;
    if (pwshBanner.hidden === !show) return;
    pwshBanner.hidden = !show;
    syncBannerSpace();
  }

  /** 终端区顶部提示条（PowerShell 7 安装 / 后台旧版本 / hub 接管中）任一显示时内容下移 */
  function syncBannerSpace() {
    el.classList.toggle("banner", !pwshBanner.hidden || !$("daemon-old-banner").hidden || !hubBanner.hidden);
    // .term 下移不改变终端区尺寸，手动 fit
    activeView()?.fit.fit();
  }

  // PowerShell 7 安装提示：安装 = 新建本地会话运行 winget
  $("pwsh-install").addEventListener("click", () => actions.createAt({ kind: "local", cwd: "" }, PWSH_INSTALL_CMD));
  $("pwsh-dismiss").addEventListener("click", () => {
    try {
      localStorage.setItem(PWSH_NOTICE_KEY, "1");
    } catch {}
    renderPwshNotice();
  });
  // hub 接管提示条：收回当前会话的接管（daemon 通知 hub 后推送 sessions，提示条随之隐藏）
  $("hub-reclaim").addEventListener("click", () => {
    const id = store.get().activeId;
    if (id !== undefined) send({ type: "hubReclaim", id });
  });
  $("empty-new").addEventListener("click", () => actions.createLocal());
  // 空状态显示期间每分钟刷新一次相对时间
  setInterval(() => !emptyPanel.hidden && renderEmpty(), 60_000);
  onLocaleChange(renderEmpty);

  // 鼠标在终端上时 Ctrl+滚轮调终端字号（作用于全部终端并记住）。捕获阶段处理并阻止默认行为：
  // 不让 WebView2 缩放整个页面，也不让 xterm 当作滚动 / 发给程序。触控板捏合同样产生 ctrlKey 的 wheel，按累计量节流
  el.addEventListener(
    "wheel",
    (ev) => {
      if (!ev.ctrlKey || !(ev.target as Element).closest(".term")) return;
      ev.preventDefault();
      ev.stopPropagation();
      const r = wheelSteps(wheelAcc, ev.deltaY, ev.deltaMode);
      wheelAcc = r.acc;
      if (!r.steps) return;
      const size = clampTermFont(currentTermFont() + r.steps);
      if (size !== currentTermFont()) actions.setTermFont(size);
      hint(t("hint.fontSize", { n: size }));
    },
    { passive: false, capture: true },
  );

  store.select((s) => s.sessions, syncViews);
  store.select((s) => [s.sessionsLoaded, s.sessions.length, s.recent, s.hosts, s.hostsLoaded], renderEmpty, shallow);
  store.select((s) => s.pwshDetected, renderPwshNotice);
  store.select((s) => s.disconnected, (d) => ($("daemon-banner").hidden = !d));
  // 当前会话被 hub 接管时显示提示条与「收回」
  store.select(
    (s) => !!activeSession(s)?.takenOver,
    (taken) => {
      hubBanner.hidden = !taken;
      syncBannerSpace();
    },
  );
  // 终端字号：作用于全部终端；当前会话立即 fit（行列变化由 term.onResize 同步给 daemon），其余会话在激活时 fit
  store.select(
    (s) => s.termFont,
    () => {
      for (const v of views.values()) v.term.options.fontSize = currentTermFont();
      fitActive();
    },
  );
  store.select((s) => s.gpu, syncGpu);

  return {
    el,
    write(m) {
      const v = views.get(m.id);
      const term = v?.term;
      // 刷新中的会话收到快照时先清空终端，丢弃清空前已写入的内容
      if (m.type === "snapshot" && refreshing.delete(m.id)) term?.reset();
      term?.write(decode(m.data));
      // 回车后输出停顿时检查光标行是否为新提示符
      if (v?.promptCheck) {
        clearTimeout(v.promptTimer);
        v.promptTimer = setTimeout(() => checkPrompt(v), PROMPT_SETTLE);
      }
    },
    resetModes(id) {
      // 只写给本地 xterm，不发给 pty
      const term = views.get(id)?.term;
      term?.write(modeResetSeq(term.buffer.active.type));
    },
    activate(id) {
      for (const v of views.values()) v.box.classList.toggle("hidden", v.info.id !== id);
      const v = views.get(id)!;
      // 显示后再加载 WebGL（display:none 时尺寸为 0），超出最近 GPU_KEEP 个的释放；fit 在其后，按最终渲染器的单元格尺寸计算
      syncGpu();
      // 隐藏期间容器尺寸为 0，显示后再 fit；行列未变时 onResize 不触发，无条件补发一次，覆盖 hub 网页留下的尺寸
      v.fit.fit();
      send({ type: "resize", id, cols: v.term.cols, rows: v.term.rows });
      if (needsRefresh(v.stale, v.info.takenOver)) refreshView(v);
      v.term.focus();
    },
    focus(fit) {
      const v = activeView();
      if (fit) v?.fit.fit();
      v?.term.focus();
    },
    paste(text) {
      const term = activeView()?.term;
      term?.paste(text);
      term?.focus();
    },
    hint,
    termSize() {
      const term = activeView()?.term;
      return term && { cols: term.cols, rows: term.rows };
    },
    areaSize: () => ({ width: area().clientWidth, height: area().clientHeight }),
    syncBannerSpace,
    /** 从资源管理器拖到终端：插入路径（Claude 运行中为 @ 引用），走 term.paste、不回车。
     *  远程会话不插入：本机路径在远程主机上无意义，提示改拖到文件树上传 */
    dropOnTerminal(x, y, paths) {
      const v = activeView();
      const hit = document.elementFromPoint(x, y);
      if (!v || !hit || !v.box.contains(hit)) return;
      if (v.info.host !== null) return alert(t("error.dropRemote"));
      v.term.paste(dropText(paths, v.info.cwd, !!v.info.claude));
      v.term.focus();
    },
  };
}
