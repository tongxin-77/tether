// 设置面板「管理服务器」分组：启用开关、服务器地址、token（存 Windows 凭据管理器，不回传 GUI）、允许明文、测试连接与当前连接状态。
// 连接由 daemon 发起；这里只编辑设置草稿（随设置“保存”提交）并显示 daemon 推送的状态。

// 与 proto crate 的 HubConfig / HubState 保持一致
export interface HubConfig {
  enabled: boolean;
  address: string;
  allowInsecure: boolean;
}

type HubState = "disabled" | "connecting" | "connected" | "failed";

export type HubClientMsg =
  | { type: "setHubToken"; token: string | null }
  | { type: "testHub"; reqId: number; address: string; allowInsecure: boolean; token: string | null; useSaved: boolean };
export type HubServerMsg =
  | { type: "hubStatus"; state: HubState; error: string | null }
  | { type: "testHub"; reqId: number; elapsedMs: number | null; hubVersion: string | null; error: string | null };

interface Deps {
  send: (msg: HubClientMsg) => void;
}

/** 等待测试结果的最长时间（daemon 测试限时 10 秒）；超时视为不认识该消息的旧版 daemon */
const TEST_TIMEOUT = 15000;
const STATE_LABELS: Record<HubState, string> = {
  disabled: "未启用",
  connecting: "连接中…",
  connected: "已连接",
  failed: "连接失败",
};

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const enabledCheck = $<HTMLInputElement>("hub-enabled");
const addressInput = $<HTMLInputElement>("hub-address");
const tokenInput = $<HTMLInputElement>("hub-token");
const insecureCheck = $<HTMLInputElement>("hub-insecure");
const testBtn = $<HTMLButtonElement>("hub-test");

let deps: Deps;
/** 凭据管理器中有已保存的 token（daemon 下发）；undefined 表示 daemon 版本较旧、不支持 hub */
let tokenSaved: boolean | undefined;
/** 设置面板中点了“清除 token”，点设置“保存”时提交 */
let clearToken = false;
let test: { reqId: number; timer: number } | undefined;
/** 与文件树一样错开 reqId 起点 */
let testSeq = Math.floor(Math.random() * 2 ** 40);

/** 校验服务器地址（规则与 daemon 的 parse_address 一致），合法返回 null，否则返回原因 */
export function hubAddressError(input: string, allowInsecure: boolean): string | null {
  const s = input.trim();
  if (!s) return "未填写服务器地址";
  let tls = !allowInsecure; // 未写协议：默认 wss，勾选允许明文则默认 ws
  let rest = s;
  const i = s.indexOf("://");
  if (i >= 0) {
    const scheme = s.slice(0, i).toLowerCase();
    if (scheme !== "wss" && scheme !== "ws") return `不支持的协议 ${scheme}://，只能是 wss:// 或 ws://`;
    tls = scheme === "wss";
    rest = s.slice(i + 3);
  }
  if (!tls && !allowInsecure) return "ws:// 为明文连接，需勾选「允许明文连接（仅限局域网调试）」";
  if (/[?#@\s]/.test(rest)) return "地址只能包含主机和端口";
  const slash = rest.indexOf("/");
  const authority = slash >= 0 ? rest.slice(0, slash) : rest;
  const path = slash >= 0 ? rest.slice(slash).replace(/\/+$/, "") : "";
  if (path !== "" && path !== "/agent") return "地址不能包含路径（连接路径固定为 /agent）";
  let host: string;
  let port: string | undefined;
  if (authority.startsWith("[")) {
    const end = authority.indexOf("]");
    if (end < 0) return "IPv6 地址缺少右方括号";
    host = authority.slice(1, end);
    if (!/^[0-9a-fA-F:.]+$/.test(host) || !host.includes(":")) return `${host} 不是合法的 IPv6 地址`;
    const after = authority.slice(end + 1);
    if (after) {
      if (!after.startsWith(":")) return "IPv6 地址后只能跟 :端口";
      port = after.slice(1);
    }
  } else {
    const parts = authority.split(":");
    if (parts.length > 2) return "IPv6 地址需用方括号括起，如 [fe80::1]:8443";
    [host, port] = parts;
    if (host && !/^[A-Za-z0-9._-]+$/.test(host)) return `主机名 ${host} 含非法字符`;
  }
  if (!host) return "缺少主机名";
  if (port !== undefined && !(/^\d+$/.test(port) && Number(port) >= 1 && Number(port) <= 65535)) {
    return `端口 ${port} 无效，应为 1–65535`;
  }
  return null;
}

export function initHub(d: Deps) {
  deps = d;
  for (const el of [enabledCheck, addressInput, insecureCheck]) el.addEventListener("input", validate);
  tokenInput.addEventListener("input", renderToken);
  $("hub-token-clear").addEventListener("click", () => {
    clearToken = true;
    tokenInput.value = "";
    renderToken();
  });
  testBtn.addEventListener("click", runTest);
}

/** 收到 daemon 的 config：回填已保存的设置（旧 daemon 无 hubTokenSaved，整个分组置灰并提示） */
export function fillHub(cfg: HubConfig | undefined, saved: boolean | undefined) {
  tokenSaved = saved;
  enabledCheck.checked = !!cfg?.enabled;
  addressInput.value = cfg?.address ?? "";
  insecureCheck.checked = !!cfg?.allowInsecure;
  clearToken = false;
  const supported = saved !== undefined;
  for (const el of [enabledCheck, addressInput, tokenInput, insecureCheck, testBtn]) el.disabled = !supported;
  $("hub-fields").classList.toggle("off", !supported);
  if (!supported) renderStatus("disabled", "后台版本较旧，不支持管理服务器；重启后台后可用");
  $("hub-test-result").hidden = true;
  renderToken();
  validate();
}

/** 设置面板中编辑中的 hub 设置（随 setConfig 提交） */
export function draftHub(): HubConfig {
  return { enabled: enabledCheck.checked, address: addressInput.value.trim(), allowInsecure: insecureCheck.checked };
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
  else showResult(true, `连接成功（耗时 ${m.elapsedMs ?? 0} ms，hub 版本 ${m.hubVersion ?? "未知"}）`);
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
  status.textContent = (saved ? "token：已保存" : "token：未保存") + (changed ? "（点设置“保存”后生效）" : "");
  status.classList.toggle("on", saved);
  tokenInput.placeholder = tokenSaved && !clearToken ? "留空则不修改已保存的 token" : "粘贴 hub 签发的 token";
  $("hub-token-clear").hidden = !saved || tokenSaved === undefined;
}

function renderStatus(state: HubState, error: string | null) {
  const el = $("hub-state");
  el.textContent = STATE_LABELS[state];
  el.dataset.state = state;
  const detail = $("hub-state-detail");
  detail.textContent = error ? (state === "connecting" ? `上次失败：${error}，稍后自动重试` : error) : "";
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
    showResult(false, "未收到后台响应：后台版本较旧，重启后台后可用");
  }, TEST_TIMEOUT);
  test = { reqId, timer };
  testBtn.disabled = true;
  testBtn.textContent = "测试中…";
  $("hub-test-result").hidden = true;
  const { address, allowInsecure } = draftHub();
  deps.send({ type: "testHub", reqId, address, allowInsecure, token: typed || null, useSaved: !typed && !clearToken });
}

function endTest() {
  if (test) clearTimeout(test.timer);
  test = undefined;
  testBtn.disabled = tokenSaved === undefined;
  testBtn.textContent = "测试连接";
}

function showResult(ok: boolean, text: string) {
  const el = $("hub-test-result");
  el.textContent = text;
  el.className = ok ? "ok" : "fail";
  el.hidden = false;
}
