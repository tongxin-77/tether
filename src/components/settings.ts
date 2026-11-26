// 设置对话框（含主机编辑对话框）：shell、外观（语言 / 主题 / 字号 / GPU / 透明度）、提示音、Claude Code 集成、hub、关于与更新、主机、联想命令。
// 语言、主题、字号、GPU 立即生效并记在本机（不随“取消”回退）；其余改动点“保存”才提交给 daemon
import { closeHub, draftHub, fillHub, initHub } from "../hub";
import { type LocalePref, LOCALES, LOCALE_NAMES, type MessageKey, localePref, onLocaleChange, setLocalePref, t } from "../i18n";
import { promptBusy } from "../prompt";
import type { Appearance, CompletionItem, Config, Host, ShellOption } from "../protocol";
import { terminalFontSize, parseFontAdj, fontAdjLabel, FONT_ADJ_OPTIONS, TERM_FONT_MIN, TERM_FONT_MAX } from "../term-utils";
import { currentTheme, getStyles, setStyle, setTransparent } from "../themes";
import { type Component, type Ctx, html } from "../ui/component";
import { overlay } from "../ui/layout";

export interface SettingsDialog extends Component {
  open(): void;
  isOpen(): boolean;
  /** daemon 下发的配置：回填设置面板并应用外观 */
  onConfig(m: { config: Config; shells: ShellOption[]; sshHosts: Host[]; passwordSaved: string[]; hubTokenSaved?: boolean }): void;
  onClaudeHooks(m: { path: string; enabled: boolean; outdated: boolean; error: string | null }): void;
  onTestHost(reqId: number, elapsedMs: number | null, error: string | null): void;
  /** 独立窗口：只应用已保存的外观 */
  applySaved(a: Appearance): void;
}

export function createSettings(ctx: Ctx): SettingsDialog {
  const { store, send, actions } = ctx;
  const settings = html<HTMLDialogElement>(`    <dialog id="settings">
      <form method="dialog">
        <div class="m-head">
          <span class="ch-ico"><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 7h-9"/><path d="M14 17H5"/><circle cx="17" cy="17" r="3"/><circle cx="7" cy="7" r="3"/></svg></span>
          <h2 data-i18n="settings.title"></h2>
        </div>
        <!-- 可滚动区域：标题与底部按钮固定 -->
        <div class="m-body">
        <div class="rows">
          <label class="row">
            <span class="rt"><b data-i18n="settings.shell.title"></b><small><span data-i18n="settings.shell.sub"></span><span id="pwsh-hint" data-i18n="settings.shell.pwshHint" hidden></span></small></span>
            <select id="shell-select"></select>
          </label>
        </div>
        <div class="section-head" data-i18n="settings.appearance"></div>
        <div class="rows">
          <label class="row">
            <span class="rt"><b data-i18n="settings.language.title"></b><small data-i18n="settings.language.sub"></small></span>
            <select id="locale-select"></select>
          </label>
          <label class="row">
            <span class="rt"><b data-i18n="settings.theme.title"></b><small data-i18n="settings.theme.sub"></small></span>
            <select id="theme-select"></select>
          </label>
          <label class="row">
            <span class="rt"><b data-i18n="settings.fontAdj.title"></b><small data-i18n="settings.fontAdj.sub"></small></span>
            <select id="font-adj-select"></select>
          </label>
          <label class="row">
            <span class="rt"><b data-i18n="settings.termFont.title"></b><small data-i18n="settings.termFont.sub"></small></span>
            <select id="term-font-select"></select>
          </label>
          <label class="row">
            <span class="rt"><b data-i18n="settings.gpu.title"></b><small data-i18n="settings.gpu.sub"></small></span>
            <input type="checkbox" class="switch" id="gpu-check" data-i18n-aria-label="settings.gpu.title" />
          </label>
          <label class="row range">
            <span class="rt"><b data-i18n="settings.opacity.title"></b><small data-i18n="settings.opacity.sub"></small></span>
            <span class="range-ctl"><input type="range" id="opacity-range" min="30" max="100" step="1" /><output id="opacity-val"></output></span>
          </label>
        </div>
        <div class="section-head" data-i18n="settings.notify"></div>
        <div class="rows">
          <div class="row" id="sound-row">
            <span class="rt"><b data-i18n="settings.sound.title"></b><small data-i18n="settings.sound.sub"></small></span>
            <button type="button" id="sound-test" data-i18n="settings.sound.test"></button>
            <input type="checkbox" class="switch" id="sound-check" data-i18n-aria-label="settings.sound.title" />
          </div>
        </div>
        <div class="section-head" data-i18n="settings.cc.title"></div>
        <!-- 在 Claude Code 的 settings.json 中写入 hooks，会话内的 claude 把状态与通知发回 Tether -->
        <div class="rows">
          <div class="row cc-row" id="cc-row">
            <span class="rt"><b><span data-i18n="settings.cc.title"></span> · <span id="cc-status"></span></b><small data-i18n="settings.cc.sub"></small><small class="hint" id="cc-path"></small></span>
            <button type="button" id="cc-enable"></button>
            <button type="button" id="cc-remove" data-i18n="settings.cc.remove"></button>
          </div>
        </div>
        <div class="section-head" data-i18n="settings.hub.section"></div>
        <!-- 连接自托管的 Tether hub（由 daemon 出站连接）；默认关闭，关闭时不发起任何连接 -->
        <div class="rows" id="hub-rows">
          <label class="row">
            <span class="rt"><b data-i18n="settings.hub.enable"></b><small data-i18n="settings.hub.enableSub"></small></span>
            <input type="checkbox" class="switch" id="hub-enabled" data-i18n-aria-label="settings.hub.enable" />
          </label>
          <div class="row" id="hub-fields">
            <label><span data-i18n="settings.hub.address"></span><input id="hub-address" autocomplete="off" spellcheck="false" data-i18n-placeholder="settings.hub.addressPlaceholder" /></label>
            <label><span data-i18n="settings.hub.token"></span><input id="hub-token" type="password" autocomplete="off" spellcheck="false" /></label>
            <div id="hub-token-state"><span id="hub-token-status"></span><button type="button" id="hub-token-clear" class="danger" data-i18n="settings.hub.clearToken"></button></div>
            <label class="check"><input type="checkbox" id="hub-insecure" /><span data-i18n="settings.hub.insecure"></span></label>
            <label class="check"><input type="checkbox" id="hub-takeover" /><span data-i18n="settings.hub.takeover"></span></label>
            <p class="hint" id="hub-takeover-hint" data-i18n="settings.hub.takeoverHint"></p>
            <p id="hub-test-result" role="status" hidden></p>
          </div>
          <div class="row">
            <span class="rt"><b><span data-i18n="settings.hub.state"></span> · <span id="hub-state"></span></b><small id="hub-state-detail"></small></span>
            <button type="button" id="hub-test"></button>
          </div>
        </div>
        <div class="section-head" data-i18n="settings.about"></div>
        <div class="rows">
          <div class="row" id="update-row">
            <span class="rt"><b><span data-i18n="settings.version"></span> <span id="about-version"></span></b><small id="update-status"></small></span>
            <button type="button" id="update-check" data-i18n="settings.checkUpdate"></button>
          </div>
          <label class="row">
            <span class="rt"><b data-i18n="settings.autoUpdate"></b></span>
            <input type="checkbox" class="switch" id="update-auto" />
          </label>
          <div class="row">
            <span class="rt"><b><span data-i18n="settings.daemon.title"></span> <span id="daemon-version"></span></b><small data-i18n="settings.daemon.sub"></small></span>
            <button type="button" id="daemon-restart-settings" data-i18n="daemon.restart"></button>
          </div>
        </div>
        <div class="section-head"><span data-i18n="settings.hosts"></span><button type="button" id="host-add" data-i18n="settings.add"></button></div>
        <ul id="host-list"></ul>
        <div class="section-head"><span data-i18n="settings.completions"></span><button type="button" id="completion-add" data-i18n="settings.add"></button></div>
        <ul id="completion-list"></ul>
        <p class="hint" data-i18n="settings.completionsHint"></p>
        </div>
        <div class="actions">
          <!-- 保存在前：回车提交时取首个按钮；视觉顺序由 CSS row-reverse 调换 -->
          <button value="save" data-i18n="common.save"></button>
          <button value="cancel" formnovalidate data-i18n="common.cancel"></button>
        </div>
        <!-- 右上角关闭（放在表单末尾，不影响回车默认提交“保存”） -->
        <button value="cancel" class="m-x" data-i18n-title="common.close" formnovalidate><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg></button>
      </form>
    </dialog>`);
  const hostDialog = html<HTMLDialogElement>(`    <dialog id="host-dialog">
      <form method="dialog">
        <label><span data-i18n="host.name"></span><input id="h-name" required /></label>
        <label><span data-i18n="host.host"></span><input id="h-host" required /></label>
        <label><span data-i18n="host.port"></span><input id="h-port" type="number" min="1" max="65535" required /></label>
        <label><span data-i18n="host.user"></span><input id="h-user" required /></label>
        <label><span data-i18n="host.auth"></span>
          <select id="h-auth">
            <option value="key" data-i18n="host.authKey"></option>
            <option value="password" data-i18n="host.authPassword"></option>
          </select>
        </label>
        <label><span data-i18n="host.keyPath"></span><input id="h-key" data-i18n-placeholder="host.keyPathPlaceholder" /></label>
        <div id="h-pw-row">
          <label id="h-pw-label"><span data-i18n="host.password"></span><input id="h-password" type="password" autocomplete="off" data-i18n-placeholder="host.passwordPlaceholder" /></label>
          <div id="h-pw-state"><span id="h-pw-status"></span><button type="button" id="h-pw-clear" class="danger" data-i18n="host.clearPassword"></button></div>
        </div>
        <p id="h-test-result" role="status" hidden></p>
        <div class="actions">
          <button value="save" data-i18n="common.save"></button>
          <button value="cancel" formnovalidate data-i18n="common.cancel"></button>
          <button type="button" id="h-test"></button>
        </div>
      </form>
    </dialog>`);
  /** 在设置对话框与主机对话框中按 id 查找 */
  const $ = <T extends HTMLElement = HTMLElement>(id: string) => (settings.querySelector<T>(`#${id}`) ?? hostDialog.querySelector<T>(`#${id}`))!;
  const shellSelect = $<HTMLSelectElement>("shell-select");
  const hostList = $<HTMLUListElement>("host-list");
  const opacityRange = $<HTMLInputElement>("opacity-range");
  const soundCheck = $<HTMLInputElement>("sound-check");
  const completionList = $<HTMLUListElement>("completion-list");
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
  /** 当前生效（含预览中）的背景不透明度，0.3–1 */
  let shownAlpha = 1;
  /** Claude Code 的 settings.json 路径（daemon 解析），用于确认提示 */
  let claudeSettingsPath = "";
  /** 最近一次收到的 Claude Code 集成状态，切换语言时据此重绘 */
  let claudeHooks: { enabled: boolean; outdated: boolean } | undefined;

  // 主题：设置面板选“风格”，侧栏按钮在同一风格内切换亮色 / 暗色；切换立即生效并记在本机（不随设置“取消”回退）
  const themeSelect = $<HTMLSelectElement>("theme-select");
  const fillThemes = () => themeSelect.replaceChildren(...getStyles().map((s) => new Option(t(`theme.${s.id}` as MessageKey), s.id)));
  fillThemes();
  const syncThemeUi = () => (themeSelect.value = currentTheme().style);
  themeSelect.addEventListener("change", () => {
    setStyle(themeSelect.value);
    syncThemeUi();
  });
  syncThemeUi();

  // 界面语言：立即生效并记在本机（不随设置“取消”回退），其它窗口经 storage 事件同步（见 i18n.ts）
  const localeSelect = $<HTMLSelectElement>("locale-select");
  function fillLocales() {
    localeSelect.replaceChildren(new Option(t("settings.language.auto"), "auto"), ...LOCALES.map((l) => new Option(LOCALE_NAMES[l], l)));
    localeSelect.value = localePref();
  }
  localeSelect.addEventListener("change", () => setLocalePref(localeSelect.value as LocalePref));
  fillLocales();

  /** 设置面板中 Claude Code 集成的状态文字与启用按钮 */
  function renderClaudeHooks() {
    const m = claudeHooks;
    const status = $("cc-status");
    // 旧版只写了部分事件：提示更新，“启用”按钮改为“更新”（重新启用即补齐）
    status.textContent = m?.outdated ? t("settings.cc.outdated") : m?.enabled ? t("settings.cc.enabled") : t("settings.cc.disabled");
    status.classList.toggle("on", !!m && m.enabled && !m.outdated);
    status.classList.toggle("outdated", !!m?.outdated);
    $("cc-enable").textContent = m?.outdated ? t("settings.cc.update") : t("settings.cc.enable");
  }

  // 字号：界面整体偏移（styles.css 的 --font-adj）与终端字号（固定值，或无存储值时按分辨率“自动”），立即生效并记在本机；
  // 其它窗口（拖出的独立窗口）修改时经 storage 事件同步
  const FONT_ADJ_KEY = "tether.ui.fontAdj";
  const autoTermFont = terminalFontSize(screen.width, devicePixelRatio);
  const fontAdjSelect = $<HTMLSelectElement>("font-adj-select");
  const termFontSelect = $<HTMLSelectElement>("term-font-select");
  /** 字号下拉的选项文字随语言变化，重填后恢复选中值 */
  function fillFontOptions() {
    const [adj, term] = [fontAdjSelect.value, termFontSelect.value];
    fontAdjSelect.replaceChildren(...FONT_ADJ_OPTIONS.map((n) => new Option(fontAdjLabel(n), String(n))));
    termFontSelect.replaceChildren(
      new Option(t("settings.termFont.auto", { n: autoTermFont }), ""),
      ...Array.from({ length: TERM_FONT_MAX - TERM_FONT_MIN + 1 }, (_, i) => new Option(String(TERM_FONT_MIN + i), String(TERM_FONT_MIN + i))),
    );
    [fontAdjSelect.value, termFontSelect.value] = [adj, term];
  }
  fillFontOptions();
  const readStore = (key: string) => {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  };
  const writeStore = (key: string, value: string) => {
    try {
      localStorage.setItem(key, value);
    } catch {}
  };
  function loadFontAdj() {
    const adj = parseFontAdj(readStore(FONT_ADJ_KEY));
    document.documentElement.style.setProperty("--font-adj", `${adj}px`);
    fontAdjSelect.value = String(adj);
  }
  /** 终端字号下拉跟随 store（本窗口 Ctrl+滚轮、其它窗口修改时同步） */
  const syncTermFont = () => (termFontSelect.value = store.get().termFont === null ? "" : String(store.get().termFont));
  fontAdjSelect.addEventListener("change", () => {
    writeStore(FONT_ADJ_KEY, fontAdjSelect.value);
    loadFontAdj();
  });
  termFontSelect.addEventListener("change", () => actions.setTermFont(termFontSelect.value ? Number(termFontSelect.value) : null));
  window.addEventListener("storage", (ev) => {
    if (ev.key === FONT_ADJ_KEY) loadFontAdj();
  });
  loadFontAdj();
  syncTermFont();
  store.select((s) => s.termFont, syncTermFont);

  // GPU 加速渲染开关（终端区按 store 加载 / 释放 WebGL）
  const gpuCheck = $<HTMLInputElement>("gpu-check");
  gpuCheck.checked = store.get().gpu;
  gpuCheck.addEventListener("change", () => actions.setGpu(gpuCheck.checked));
  store.select((s) => s.gpu, (on) => (gpuCheck.checked = on));

  // 设置对话框：打开时向 daemon 取最新配置，收到 config 消息时回填
  function openSettings() {
    send({ type: "getConfig" });
    send({ type: "getClaudeHooks" });
    // Esc 关闭不会改 returnValue，清掉上次的值以免误保存
    settings.returnValue = "";
    settings.showModal();
  }
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
    actions.focusTerminal();
  });

  // 外观：拖动滑块只改草稿并实时预览，点保存才持久化
  opacityRange.addEventListener("input", () => {
    draftAppearance.opacity = Number(opacityRange.value);
    previewAppearance();
  });

  // 试听：不受提示音开关影响
  $("sound-test").addEventListener("click", () => send({ type: "playSound" }));

  // Claude Code 集成：由 daemon 修改 settings.json（写前备份），写入前确认
  for (const enabled of [true, false]) {
    $(enabled ? "cc-enable" : "cc-remove").addEventListener("click", async () => {
      const msg = t(enabled ? "settings.cc.confirmEnable" : "settings.cc.confirmRemove", { path: claudeSettingsPath });
      if ((await actions.ask(msg, t(enabled ? "settings.cc.enable" : "settings.cc.remove"))) === null) return;
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

  // 用 daemon 探测到的 shell 填充下拉框；当前配置不在列表中（手改过 config.json）时额外列出并选中，保证不丢失
  function fillShells(current: string, shells: ShellOption[]) {
    const options = shells.map((s) => new Option(s.name, s.command));
    if (current && !shells.some((s) => s.command === current)) options.push(new Option(current, current));
    shellSelect.replaceChildren(...options);
    shellSelect.value = current;
  }

  /** 应用外观：设置背景不透明度 CSS 变量（对话框 / 菜单至少 88%），同步所有已存在终端的背景 */
  function applyAppearance(a: Appearance) {
    const root = document.documentElement;
    shownAlpha = Math.min(100, Math.max(30, a.opacity)) / 100;
    root.style.setProperty("--alpha", String(shownAlpha));
    root.style.setProperty("--alpha-strong", String(Math.max(shownAlpha, 0.88)));
    setTransparent(shownAlpha < 1);
  }

  /** 应用草稿外观并回填设置面板控件 */
  function previewAppearance() {
    const a = draftAppearance;
    applyAppearance(a);
    opacityRange.value = String(a.opacity);
    $("opacity-val").textContent = `${a.opacity}%`;
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
      btn(t("common.edit"), () => editHost(h));
      if (h.source !== "sshConfig") btn(t("common.delete"), remove, "danger");
      else if (draftHosts.some((d) => d.id === h.id)) btn(t("settings.hostReset"), remove);
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
        field("text", t("settings.completionCommand"));
        field("desc", t("settings.completionDesc"));
        const b = li.appendChild(document.createElement("button"));
        b.type = "button";
        b.className = "danger";
        b.textContent = t("common.delete");
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
      if (!promptBusy()) hostTest.left -= 500;
      if (hostTest.left <= 0) {
        endHostTest();
        showTestResult(false, t("error.daemonNoReply"));
      }
    }, 500);
    hostTest = { reqId, left: HOST_TEST_TIMEOUT, timer };
    $<HTMLButtonElement>("h-test").disabled = true;
    renderHostTestBtn();
    $("h-test-result").hidden = true;
    send({ type: "testHost", reqId, host: hostFromDialog(), password, useSaved });
  }

  function onTestHost(reqId: number, elapsedMs: number | null, error: string | null) {
    if (reqId !== hostTest?.reqId) return;
    endHostTest();
    if (error) showTestResult(false, error);
    else showTestResult(true, t("host.testOk", { ms: elapsedMs ?? 0 }));
  }

  /** 结束等待中的测试（收到结果 / 超时 / 关闭对话框），按钮恢复 */
  function endHostTest() {
    if (hostTest) clearInterval(hostTest.timer);
    hostTest = undefined;
    $<HTMLButtonElement>("h-test").disabled = false;
    renderHostTestBtn();
  }

  function renderHostTestBtn() {
    $("h-test").textContent = hostTest ? t("common.testing") : t("common.testConnection");
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
    status.textContent = (saved ? t("host.passwordSaved") : t("host.passwordNotSaved")) + (changed ? t("settings.pendingSave") : "");
    status.classList.toggle("on", saved);
    $("h-pw-clear").hidden = !saved;
  }

  renderClaudeHooks();
  renderHostTestBtn();
  // 动态生成的文字随语言重新渲染
  onLocaleChange(() => {
    fillLocales();
    fillThemes();
    syncThemeUi();
    fillFontOptions();
    renderClaudeHooks();
    renderHostTestBtn();
    renderHosts();
    renderCompletions();
    renderPasswordRow();
  });

  return {
    el: settings,
    mount() {
      overlay().append(hostDialog);
      // hub 设置区按 id 取元素，放入页面后再初始化
      initHub({ send });
    },
    unmount: () => hostDialog.remove(),
    open: openSettings,
    isOpen: () => settings.open,
    onConfig(m) {
      fillShells(m.config.shell, m.shells);
      $("pwsh-hint").hidden = m.shells.some((s) => s.name === "PowerShell 7");
      savedAppearance = m.config.appearance;
      draftAppearance = { ...savedAppearance };
      previewAppearance();
      soundCheck.checked = m.config.sound;
      draftCompletions = m.config.completions.map((c) => ({ ...c }));
      renderCompletions();
      sshHosts = m.sshHosts;
      draftHosts = m.config.hosts.map((h) => ({ ...h }));
      savedPasswords = new Set(m.passwordSaved);
      fillHub(m.config.hub, m.hubTokenSaved);
      renderHosts();
    },
    onClaudeHooks(m) {
      claudeSettingsPath = m.path;
      // 旧版只写了部分事件：提示更新，“启用”按钮改为“更新”（重新启用即补齐）
      claudeHooks = { enabled: m.enabled, outdated: m.outdated };
      renderClaudeHooks();
      $("cc-path").textContent = m.path;
      if (m.error) alert(m.error);
    },
    onTestHost,
    applySaved(a) {
      savedAppearance = a;
      applyAppearance(a);
    },
  };
}
