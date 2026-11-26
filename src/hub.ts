// 设置面板「管理服务器」分组：启用开关、服务器地址、token（存 Windows 凭据管理器，不回传 GUI）、允许明文、允许远程接管、测试连接与当前连接状态。
// 连接由 daemon 发起；这里只编辑设置草稿（随设置“保存”提交）并显示 daemon 推送的状态。

import { onLocaleChange, t } from "./i18n";

// 与 proto crate 的 HubConfig / HubState 保持一致
export interface HubConfig {
  enabled: boolean;
  address: string;
  allowInsecure: boolean;
  /** 允许 hub 远程接管本机会话（旧 daemon 无此字段） */
  allowTakeover?: boolean;
}

type HubState = "disabled" | "connecting" | "connected" | "failed";

export type HubClientMsg =
  | { type: "setHubToken"; token: string | null }
  | { type: "hubReclaim"; id: number }
  | { type: "testHub"; reqId: number; address: string; allowInsecure: boolean; token: string | null; useSaved: boolean };
export type HubServerMsg =
  | { type: "hubStatus"; state: HubState; error: string | null }
  | { type: "testHub"; reqId: number; elapsedMs: number | null; hubVersion: string | null; error: string | null };

interface Deps {
  send: (msg: HubClientMsg) => void;
}

/** 等待测试结果的最长时间（daemon 测试限时 10 秒）；超时视为不认识该消息的旧版 daemon */
const TEST_TIMEOUT = 15000;

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
// 设置面板中的元素，initHub 时取（设置面板由组件创建）
let enabledCheck: HTMLInputElement;
let addressInput: HTMLInputElement;
let tokenInput: HTMLInputElement;
let insecureCheck: HTMLInputElement;
let takeoverCheck: HTMLInputElement;
let testBtn: HTMLButtonElement;

let deps: Deps;
/** 凭据管理器中有已保存的 token（daemon 下发）；undefined 表示 daemon 版本较旧、不支持 hub */
let tokenSaved: boolean | undefined;
/** 设置面板中点了“清除 token”，点设置“保存”时提交 */
let clearToken = false;
let test: { reqId: number; timer: number } | undefined;
/** 与文件树一样错开 reqId 起点 */
let testSeq = Math.floor(Math.random() * 2 ** 40);
/** 最近一次显示的连接状态，切换语言时重绘 */
let shown: { state: HubState; error: string | null } = { state: "disabled", error: null };

/** 校验服务器地址（规则与 daemon 的 parse_address 一致），合法返回 null，否则返回原因 */
export function hubAddressError(input: string, allowInsecure: boolean): string | null {
  const s = input.trim();
  if (!s) return t("hub.addr.empty");
  let tls = !allowInsecure; // 未写协议：默认 wss，勾选允许明文则默认 ws
  let rest = s;
  const i = s.indexOf("://");
  if (i >= 0) {
    const scheme = s.slice(0, i).toLowerCase();
    if (scheme !== "wss" && scheme !== "ws") return t("hub.addr.scheme", { scheme });
    tls = scheme === "wss";
    rest = s.slice(i + 3);
  }
  if (!tls && !allowInsecure) return t("hub.addr.insecure", { option: t("settings.hub.insecure") });
  if (/[?#@\s]/.test(rest)) return t("hub.addr.hostPortOnly");
  const slash = rest.indexOf("/");
  const authority = slash >= 0 ? rest.slice(0, slash) : rest;
  const path = slash >= 0 ? rest.slice(slash).replace(/\/+$/, "") : "";
  if (path !== "" && path !== "/agent") return t("hub.addr.noPath");
  let host: string;
  let port: string | undefined;
  if (authority.startsWith("[")) {
    const end = authority.indexOf("]");
    if (end < 0) return t("hub.addr.ipv6Bracket");
    host = authority.slice(1, end);
    if (!/^[0-9a-fA-F:.]+$/.test(host) || !host.includes(":")) return t("hub.addr.ipv6Invalid", { host });
    const after = authority.slice(end + 1);
    if (after) {
      if (!after.startsWith(":")) return t("hub.addr.ipv6Port");
      port = after.slice(1);
    }
  } else {
    const parts = authority.split(":");
    if (parts.length > 2) return t("hub.addr.ipv6NeedBrackets");
    [host, port] = parts;
    if (host && !/^[A-Za-z0-9._-]+$/.test(host)) return t("hub.addr.badHost", { host });
  }
  if (!host) return t("hub.addr.noHost");
  if (port !== undefined && !(/^\d+$/.test(port) && Number(port) >= 1 && Number(port) <= 65535)) {
    return t("hub.addr.badPort", { port });
  }
  return null;
}

export function initHub(d: Deps) {
  enabledCheck = $("hub-enabled");
  addressInput = $("hub-address");
  tokenInput = $("hub-token");
  insecureCheck = $("hub-insecure");
  takeoverCheck = $("hub-takeover");
  testBtn = $("hub-test");
  deps = d;
  for (const el of [enabledCheck, addressInput, insecureCheck]) el.addEventListener("input", validate);
  tokenInput.addEventListener("input", renderToken);
  $("hub-token-clear").addEventListener("click", () => {
    clearToken = true;
    tokenInput.value = "";
    renderToken();
  });
  testBtn.addEventListener("click", runTest);
  renderTestBtn();
  renderStatus(shown.state, shown.error);
  onLocaleChange(() => {
    renderStatus(shown.state, shown.error);
    renderToken();
    renderTestBtn();
    validate();
  });
}

/** 收到 daemon 的 config：回填已保存的设置（旧 daemon 无 hubTokenSaved，整个分组置灰并提示） */
export function fillHub(cfg: HubConfig | undefined, saved: boolean | undefined) {
  tokenSaved = saved;
  enabledCheck.checked = !!cfg?.enabled;
  addressInput.value = cfg?.address ?? "";
  insecureCheck.checked = !!cfg?.allowInsecure;
  takeoverCheck.checked = !!cfg?.allowTakeover;
  clearToken = false;
  const supported = saved !== undefined;
  for (const el of [enabledCheck, addressInput, tokenInput, insecureCheck, takeoverCheck, testBtn]) el.disabled = !supported;
  $("hub-fields").classList.toggle("off", !supported);
  if (!supported) renderStatus("disabled", t("hub.unsupported"));
  $("hub-test-result").hidden = true;
  renderToken();
  validate();
}

/** 设置面板中编辑中的 hub 设置（随 setConfig 提交） */
export function draftHub(): HubConfig {
  return {
    enabled: enabledCheck.checked,
    address: addressInput.value.trim(),
    allowInsecure: insecureCheck.checked,
    allowTakeover: takeoverCheck.checked,
  };
}

/** 设置面板关闭：保存时在 setConfig 之后提交 token 改动（留空不改、点过清除则清除）；token 不留在 DOM 中 */
export function closeHub(save: boolean) {
  endTest();
  const token = tokenInput.value.trim();
  if (save && tokenSaved !== undefined) {
    if (token) deps.send({ type: "setHubToken", token });
    else if (clearToken) deps.send({ type: "setHubToken", token: null });
  }
  tokenInput.value = "";
  clearToken = false;
}

export function onHubMessage(m: HubServerMsg) {
  if (m.type === "hubStatus") return renderStatus(m.state, m.error);
  if (m.reqId !== test?.reqId) return;
  endTest();
  if (m.error) showResult(false, m.error);
  else showResult(true, t("hub.testOk", { ms: m.elapsedMs ?? 0, version: m.hubVersion ?? t("hub.unknownVersion") }));
}

/** 地址校验结果写入输入框的自定义有效性：设置“保存”与测试连接都会被拦下并提示原因。未启用且留空时不校验 */
function validate() {
  const addr = addressInput.value;
  const err = !enabledCheck.checked && !addr.trim() ? null : hubAddressError(addr, insecureCheck.checked);
  addressInput.setCustomValidity(err ?? "");
}

function renderToken() {
  const typed = tokenInput.value !== "";
  const saved = typed || (!clearToken && !!tokenSaved);
  const changed = typed || clearToken;
  const status = $("hub-token-status");
  status.textContent = (saved ? t("hub.tokenSaved") : t("hub.tokenNotSaved")) + (changed ? t("settings.pendingSave") : "");
  status.classList.toggle("on", saved);
  tokenInput.placeholder = tokenSaved && !clearToken ? t("hub.tokenKeep") : t("hub.tokenPaste");
  $("hub-token-clear").hidden = !saved || tokenSaved === undefined;
}

function renderStatus(state: HubState, error: string | null) {
  shown = { state, error };
  const el = $("hub-state");
  el.textContent = t(`hub.state.${state}`);
  el.dataset.state = state;
  const detail = $("hub-state-detail");
  detail.textContent = error ? (state === "connecting" ? t("hub.retrying", { error }) : error) : "";
}

/** 测试连接：用面板当前填写的地址 / 明文开关 / token（不必先保存）；token 留空时用已保存的（点过清除则没有） */
function runTest() {
  validate();
  if (!addressInput.reportValidity()) return;
  const typed = tokenInput.value.trim();
  endTest();
  const reqId = ++testSeq;
  const timer = window.setTimeout(() => {
    endTest();
    showResult(false, t("error.daemonNoReply"));
  }, TEST_TIMEOUT);
  test = { reqId, timer };
  testBtn.disabled = true;
  renderTestBtn();
  $("hub-test-result").hidden = true;
  const { address, allowInsecure } = draftHub();
  deps.send({ type: "testHub", reqId, address, allowInsecure, token: typed || null, useSaved: !typed && !clearToken });
}

function endTest() {
  if (test) clearTimeout(test.timer);
  test = undefined;
  testBtn.disabled = tokenSaved === undefined;
  renderTestBtn();
}

function renderTestBtn() {
  testBtn.textContent = test ? t("common.testing") : t("common.testConnection");
}

function showResult(ok: boolean, text: string) {
  const el = $("hub-test-result");
  el.textContent = text;
  el.className = ok ? "ok" : "fail";
  el.hidden = false;
}
