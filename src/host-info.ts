// 文件面板底部的主机信息卡片：显示当前激活会话所在机器（本地会话为本机，远程会话为服务器）的 CPU / 内存 / GPU / 网络。
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
/** 等待回复的最长时间（daemon 远程采样超时为 8 秒）；超时视为不认识该消息的旧版 daemon */
const TIMEOUT = 15000;

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

export function initHostInfo(d: Deps) {
  deps = d;
  document.addEventListener("visibilitychange", kick);
  // 文件树的收起 / 展开按钮（files.ts 的监听先注册，此时 collapsed 已更新）
  $("fs-toggle").addEventListener("click", kick);
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
  if (id !== undefined) renderNote(unsupported ? "后台需重启" : "加载中…");
  kick();
}

export function onHostStats(m: HostStatsServerMsg) {
  if (m.reqId !== pending?.reqId) return;
  const sessionId = pending.sessionId;
  pending = undefined;
  clearTimeout(timer);
  // 等待期间已切换会话：丢弃并立即为新会话采样
  if (sessionId !== deps.active()?.id) return schedule(0);
  if (m.stats) render(m.stats, deps.active()?.host != null);
  else renderNote(m.error?.startsWith("不支持") ? "不支持" : "不可用", m.error ?? "");
  schedule(POLL);
}

function canPoll() {
  return document.visibilityState === "visible" && !panel.classList.contains("collapsed") && panel.offsetWidth > 0;
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
    renderNote("后台需重启", "后台进程版本较旧，不支持主机信息；重启后台后可用");
    return;
  }
  if (!s || unsupported || !canPoll()) return;
  pending = { reqId: ++reqSeq, sessionId: s.id };
  deps.send({ type: "hostStats", ...pending });
  schedule(TIMEOUT);
}

// ---------------- 渲染 ----------------

function row(key: string, value: string, title = "") {
  const el = document.createElement("div");
  el.className = "hi-row";
  el.innerHTML = `<span class="hi-k"></span><span class="hi-v"></span>`;
  el.querySelector(".hi-k")!.textContent = key;
  el.querySelector(".hi-v")!.textContent = value;
  el.title = title;
  return el;
}

function renderNote(text: string, title = "") {
  const el = row("主机", text, title);
  el.classList.add("hi-note");
  card.replaceChildren(el);
}

function render(s: HostStats, remote: boolean) {
  const cpu = [shortCpu(s.cpuModel), s.cores ? `${s.cores}核` : "", s.cpuUsage == null ? "…" : `${Math.round(s.cpuUsage)}%`];
  const memPct = s.memTotal ? ` (${Math.round((s.memUsed * 100) / s.memTotal)}%)` : "";
  // 有多块显卡时显示显存最大的（独显），全部列在悬停提示中
  const gpu = [...s.gpus].sort((a, b) => (b.memTotal ?? 0) - (a.memTotal ?? 0))[0];
  const gpuTitle = s.gpus.length
    ? s.gpus.map((g) => [g.name, g.usage == null ? "" : `占用 ${Math.round(g.usage)}%`, vram(g)].filter(Boolean).join("  ")).join("\n")
    : remote
      ? "未检测到 NVIDIA 显卡（远程仅支持 nvidia-smi）"
      : "未检测到显卡";
  card.replaceChildren(
    row("CPU", cpu.filter(Boolean).join(" "), [s.cpuModel, s.cores ? `逻辑处理器 ${s.cores}` : ""].filter(Boolean).join("\n")),
    row("内存", `${gb(s.memUsed)}/${gb(s.memTotal)} GB`, `已用 ${gb(s.memUsed)} GB / 共 ${gb(s.memTotal)} GB${memPct}`),
    row("GPU", gpu ? [shortGpu(gpu.name), gpu.usage == null ? "" : `${Math.round(gpu.usage)}%`].filter(Boolean).join(" ") : "—", gpuTitle),
    row(
      "网络",
      s.netRx == null || s.netTx == null ? "…" : `↓${rate(s.netRx)} ↑${rate(s.netTx)}`,
      remote ? "物理网卡合计（不含 lo 与虚拟网卡）" : "硬件网卡合计",
    ),
  );
}

/** 显存：集成显卡的“专用显存”只是很小的保留区，不显示 */
function vram(g: GpuStats) {
  if (g.memTotal == null || g.memTotal < 512 * 2 ** 20) return "";
  return g.memUsed == null ? `显存 ${gb(g.memTotal)} GB` : `显存 ${gb(g.memUsed)}/${gb(g.memTotal)} GB`;
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
  return v >= 10 ? Math.round(v).toString() : v.toFixed(1);
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
