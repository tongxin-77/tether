// 文件面板底部的主机信息状态条（悬停或点击弹出详情）：显示当前激活会话所在机器（本地会话为本机，远程会话为服务器）的 CPU / 内存 / GPU / 网络。
// 仅在窗口可见、文件面板展开且有激活会话时每 3 秒向 daemon 取一次采样，否则停止。

// 与 proto crate 的 HostStats 保持一致
interface GpuStats {
  name: string;
  usage: number | null;
  memUsed: number | null;
  memTotal: number | null;
}

interface HostStats {
  cpuModel: string;
  cores: number;
  cpuUsage: number | null;
  memUsed: number;
  memTotal: number;
  gpus: GpuStats[];
  netRx: number | null;
  netTx: number | null;
}

export type HostStatsClientMsg = { type: "hostStats"; reqId: number; sessionId: number };
export type HostStatsServerMsg = { type: "hostStats"; reqId: number; stats: HostStats | null; error: string | null };

interface Deps {
  send: (msg: HostStatsClientMsg) => void;
  active: () => { id: number; host: string | null } | undefined;
}

const POLL = 3000;
/** daemon “不支持”类错误的前缀（daemon 按其当前语言生成，与 daemon/src/i18n.rs 的 stats.not_linux 四种语言一致） */
const UNSUPPORTED_RE = /^(不支持|不支援|Not supported|サポートされていません)/;
/** 等待回复的最长时间（daemon 远程采样超时为 8 秒）；超时视为不认识该消息的旧版 daemon */
const TIMEOUT = 15000;

import { icon } from "./icons";
import { fmtNum, onLocaleChange, t, tn } from "./i18n";

const $ = (id: string) => document.getElementById(id) as HTMLElement;
const panel = $("files");
const card = $("host-info");

let deps: Deps;
let timer: ReturnType<typeof setTimeout> | undefined;
let pending: { reqId: number; sessionId: number } | undefined;
/** 当前连接的 daemon 不支持主机信息（等待超时）；会话列表清空（daemon 断开重连）时重置 */
let unsupported = false;
let shownId: number | undefined;
/** 与文件树一样错开 reqId 起点，避免误认别的窗口的回复 */
let reqSeq = Math.floor(Math.random() * 2 ** 40);
/** 最近一次渲染，切换语言时重绘 */
let redraw: (() => void) | undefined;

export function initHostInfo(d: Deps) {
  deps = d;
  document.addEventListener("visibilitychange", kick);
  // 点击状态条固定 / 收起详情（悬停时由 CSS 显示），点别处收起
  card.addEventListener("click", (ev) => {
    ev.stopPropagation();
    card.classList.toggle("open");
  });
  document.addEventListener("click", () => card.classList.remove("open"));
  onLocaleChange(() => redraw?.());
}

/** 激活会话变化时调用：清空旧数据并立即采样 */
export function showHostInfo() {
  const id = deps.active()?.id;
  if (id === shownId) return;
  shownId = id;
  if (id === undefined) {
    // daemon 断开时会话全部移除：丢弃未回复的请求（不能算作超时），重连后重新判断是否支持
    unsupported = false;
    pending = undefined;
    clearTimeout(timer);
  }
  card.hidden = id === undefined;
  if (id !== undefined) renderNote(unsupported ? "hostInfo.needRestart" : "common.loading");
  kick();
}

export function onHostStats(m: HostStatsServerMsg) {
  if (m.reqId !== pending?.reqId) return;
  const sessionId = pending.sessionId;
  pending = undefined;
  clearTimeout(timer);
  // 等待期间已切换会话：丢弃并立即为新会话采样
  if (sessionId !== deps.active()?.id) return schedule(0);
  if (m.stats) render(m.stats, deps.active()?.host ?? null);
  // daemon 的错误文字随其语言；“不支持”类错误（如远程非 Linux）以各语言的前缀识别
  else renderNote(UNSUPPORTED_RE.test(m.error ?? "") ? "hostInfo.unsupported" : "hostInfo.unavailable", m.error ?? "");
  schedule(POLL);
}

function canPoll() {
  // 文件栏不可见（独立窗口）时不采样；文件栏收起不影响（状态条在终端下方）
  return document.visibilityState === "visible" && panel.offsetWidth > 0;
}

/** 条件可能变化时调用：没有进行中的请求则立即采样 */
function kick() {
  if (!pending) schedule(0);
}

function schedule(delay: number) {
  clearTimeout(timer);
  timer = setTimeout(tick, delay);
}

function tick() {
  const s = deps.active();
  if (pending) {
    // 等待超时：daemon 不认识该消息
    pending = undefined;
    unsupported = true;
    renderNote("hostInfo.needRestart", t("hostInfo.oldDaemon"));
    return;
  }
  if (!s || unsupported || !canPoll()) return;
  pending = { reqId: ++reqSeq, sessionId: s.id };
  deps.send({ type: "hostStats", ...pending });
  schedule(TIMEOUT);
}

// ---------------- 渲染 ----------------

function el(tag: string, cls: string, text = "") {
  const e = document.createElement(tag);
  e.className = cls;
  e.textContent = text;
  return e;
}

/** 状态条一项：彩色圆点 + 名称 + 数值 */
function barItem(color: string, key: string, value: string) {
  const s = el("span", "hb-item");
  const dot = s.appendChild(el("i", "hb-dot"));
  dot.style.background = color;
  s.append(key, " ");
  s.appendChild(el("b", "", value));
  return s;
}

/** 环形仪表：pct 为 null 时只画轨道 */
function gauge(color: string, pct: number | null, label: string, sub: string, title = "") {
  const g = el("div", "gauge");
  g.title = title;
  const C = 138.2; // 2π × 22
  const p = pct == null ? 0 : Math.max(0, Math.min(100, pct));
  g.innerHTML = `<svg class="ring" viewBox="0 0 54 54"><circle class="trk" cx="27" cy="27" r="22"/><circle class="val" cx="27" cy="27" r="22" stroke="${color}" stroke-dasharray="${C}" stroke-dashoffset="${(C * (1 - p / 100)).toFixed(1)}"/><text x="27" y="31" text-anchor="middle"></text></svg>`;
  g.querySelector("text")!.textContent = pct == null ? "—" : `${Math.round(pct)}%`;
  g.append(el("span", "gl", label), el("span", "gs", sub));
  return g;
}

function renderNote(key: "hostInfo.needRestart" | "common.loading" | "hostInfo.unsupported" | "hostInfo.unavailable", title = "") {
  redraw = () => renderNote(key, key === "hostInfo.needRestart" ? t("hostInfo.oldDaemon") : title);
  const text = t(key);
  card.classList.remove("open");
  card.innerHTML = icon("cpu");
  card.appendChild(el("span", "hb hi-note", text));
  card.title = title;
}

function render(s: HostStats, host: string | null) {
  redraw = () => render(s, host);
  const remote = host !== null;
  const memPct = s.memTotal ? (s.memUsed * 100) / s.memTotal : null;
  // 有多块显卡时显示显存最大的（独显），全部列在悬停提示中
  const gpu = [...s.gpus].sort((a, b) => (b.memTotal ?? 0) - (a.memTotal ?? 0))[0];
  const gpuTitle = s.gpus.length
    ? s.gpus.map((g) => [g.name, g.usage == null ? "" : t("hostInfo.gpuUsage", { pct: Math.round(g.usage) }), vram(g)].filter(Boolean).join("  ")).join("\n")
    : remote
      ? t("hostInfo.noNvidia")
      : t("hostInfo.noGpu");
  const net = s.netRx == null || s.netTx == null ? "…" : `↓${rate(s.netRx)} ↑${rate(s.netTx)}`;
  const pct = (v: number | null) => (v == null ? "…" : `${Math.round(v)}%`);

  const bar = el("span", "hb");
  bar.append(barItem("var(--chart-1)", "CPU", pct(s.cpuUsage)), el("span", "sep", "·"), barItem("var(--chart-2)", t("hostInfo.memory"), pct(memPct)));
  // 第三项：有显卡占用时显示 GPU，否则显示网络
  bar.append(el("span", "sep", "·"));
  if (gpu?.usage != null) bar.append(barItem("var(--chart-3)", "GPU", pct(gpu.usage)));
  else bar.append(barItem("var(--chart-3)", t("hostInfo.network"), net));

  const pop = el("div", "host-pop");
  pop.setAttribute("role", "dialog");
  pop.setAttribute("aria-label", t("hostInfo.details"));
  const head = pop.appendChild(el("div", "p-head"));
  const h3 = head.appendChild(el("h3", ""));
  h3.innerHTML = icon("cpu");
  h3.append(`${t("hostInfo.host")} `);
  h3.appendChild(el("small", "", host ?? t("hostInfo.local")));
  const gauges = pop.appendChild(el("div", "gauges"));
  gauges.append(
    gauge("var(--chart-1)", s.cpuUsage, "CPU", s.cores ? tn("hostInfo.threads", s.cores) : "—"),
    gauge("var(--chart-2)", memPct, t("hostInfo.memory"), `${gb(s.memUsed)}/${gb(s.memTotal)} GB`),
    gauge("var(--chart-3)", gpu?.usage ?? null, "GPU", gpu ? shortGpu(gpu.name) : t("hostInfo.none"), gpuTitle),
  );
  const line = (ico: "monitor" | "arrow-down-up", name: string, value: string, title: string) => {
    const l = pop.appendChild(el("div", "sysline"));
    l.title = title;
    l.innerHTML = icon(ico);
    l.append(el("b", "", name), el("span", "sp", value));
  };
  line("monitor", shortCpu(s.cpuModel) || "CPU", s.cores ? tn("hostInfo.cores", s.cores) : "", [s.cpuModel, s.cores ? t("hostInfo.logical", { n: s.cores }) : ""].filter(Boolean).join("\n"));
  line("arrow-down-up", t("hostInfo.network"), net, remote ? t("hostInfo.netRemote") : t("hostInfo.netLocal"));

  card.title = "";
  card.innerHTML = icon("cpu");
  card.append(bar, pop);
}

/** 显存：集成显卡的“专用显存”只是很小的保留区，不显示 */
function vram(g: GpuStats) {
  if (g.memTotal == null || g.memTotal < 512 * 2 ** 20) return "";
  return t("hostInfo.vram", { size: g.memUsed == null ? gb(g.memTotal) : `${gb(g.memUsed)}/${gb(g.memTotal)}` });
}

/** “Intel(R) Core(TM) i7-10700 CPU @ 2.90GHz” → “i7-10700”，“AMD Ryzen 7 5800X 8-Core Processor” → “Ryzen 7 5800X” */
function shortCpu(model: string) {
  return model
    .replace(/\((R|TM)\)|[®™]|@.*$|\b\d+(st|nd|rd|th) Gen\b|\b\d+-Core\b|\b(Intel|AMD|Core|CPU|Processor)\b/gi, " ")
    .replace(/\s+/g, " ")
    .trim();
}

/** “NVIDIA GeForce RTX 3060” → “RTX 3060” */
function shortGpu(name: string) {
  return (
    name
      .replace(/\((R|TM)\)|[®™]|\b(NVIDIA|GeForce|Intel|AMD|Graphics)\b/gi, " ")
      .replace(/\s+/g, " ")
      .trim() || name
  );
}

function gb(bytes: number) {
  const v = bytes / 2 ** 30;
  return v >= 10 ? fmtNum(Math.round(v)) : fmtNum(v, 1);
}

/** 字节/秒 → “80K”“1.2M” */
function rate(bps: number) {
  const units = ["B", "K", "M", "G"];
  let i = 0;
  while (bps >= 1024 && i < units.length - 1) {
    bps /= 1024;
    i++;
  }
  return `${i && bps < 10 ? bps.toFixed(1) : Math.round(bps)}${units[i]}`;
}
