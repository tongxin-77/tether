// 终端相关的小纯函数（无副作用，便于单元测试）
import type { ITerminalOptions, IWindowsPty } from "@xterm/xterm";

/** 所有终端共用的固定选项：表格单元格选择（table-select.ts）用到 registerDecoration，属 proposed API，须开启 */
export const TERMINAL_OPTIONS: ITerminalOptions = { allowProposedApi: true };

/** 是否弹出命令补全：Claude Code 运行中（claude 非空）或全屏程序（alternate buffer）时不弹 */
export function completionEnabled(claude: unknown, bufferType: string): boolean {
  return claude == null && bufferType !== "alternate";
}

/** 本地会话（host 为 null）走 ConPTY 的 windowsPty 选项；远程会话或 build 号未知时为 undefined */
export function windowsPtyFor(host: string | null, build: number | null): IWindowsPty | undefined {
  return host === null && build ? { backend: "conpty", buildNumber: build } : undefined;
}

/** 文件栏宽度夹在 [200, min(640, 窗口宽 * 0.5)]，取整 */
export function clampFilesWidth(w: number, innerWidth: number): number {
  return Math.round(Math.max(200, Math.min(w, 640, innerWidth * 0.5)));
}

/** 终端容器过小（最小化等）时不 fit，免得 pty 被缩成 2 列 1 行 */
export function shouldFit(width: number, height: number): boolean {
  return width >= 100 && height >= 50;
}

/** 尾沿防抖：连续调用只在最后一次之后 ms 毫秒执行一次 */
export function debounce(fn: () => void, ms: number): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return () => {
    clearTimeout(timer);
    timer = setTimeout(fn, ms);
  };
}

/** hub 接管是否刚结束（由接管变为未接管） */
export function wasReleased(prev: boolean | undefined, next: boolean | undefined): boolean {
  return !!prev && !next;
}

/** 本地终端是否因 hub 接管而待刷新：接管过即标记，刷新后由调用方清除 */
export function takeoverStale(stale: boolean, takenOver: boolean | undefined): boolean {
  return stale || !!takenOver;
}

/** 显示会话时是否刷新终端（清空后重取快照）：接管过且已结束 */
export function needsRefresh(stale: boolean, takenOver: boolean | undefined): boolean {
  return stale && !takenOver;
}

/** 查 git 分支的目录：仅本地会话（host 为 null）的启动目录，远程或目录为空时为 null */
export function branchDir(host: string | null, cwd: string): string | null {
  return host === null && cwd ? cwd : null;
}

/** 卡片头部的分支文字，如 `⎇ main`；无分支为空串（CSS 以 :empty 隐藏） */
export function branchLabel(branch: string | null | undefined): string {
  return branch ? `⎇ ${branch}` : "";
}

/** 终端字号“自动”值：4K 及以上（屏幕物理宽度 ≥ 3840）用 12，其余用更紧凑的 11 */
export function terminalFontSize(screenWidth: number, dpr: number): number {
  return screenWidth * dpr >= 3840 ? 12 : 11;
}

/** 终端字号可调范围（设置面板与 Ctrl+滚轮） */
export const TERM_FONT_MIN = 8;
export const TERM_FONT_MAX = 28;

/** 钳制终端字号到可调范围并取整 */
export function clampTermFont(size: number): number {
  return Math.round(Math.max(TERM_FONT_MIN, Math.min(size, TERM_FONT_MAX)));
}

/** 解析存储的终端字号：无值或非数字为 null（自动），数字取整并钳制到 8–28 */
export function parseTermFont(stored: string | null): number | null {
  const n = stored?.trim() ? Number(stored) : NaN;
  return Number.isFinite(n) ? clampTermFont(n) : null;
}

/** 界面字号偏移（相对原始字号，单位 px）：可选 -4…+2，默认小 2 号 */
export const FONT_ADJ_DEFAULT = -2;
export const FONT_ADJ_OPTIONS = [-4, -3, -2, -1, 0, 1, 2];

/** 解析存储的界面字号偏移；非法值为默认。index.html 头部的内联脚本用同一正则，须保持一致 */
export function parseFontAdj(stored: string | null): number {
  return stored !== null && /^(-[1-4]|[0-2])$/.test(stored) ? Number(stored) : FONT_ADJ_DEFAULT;
}

/** 界面字号偏移的选项文字，如“小 2 号（默认）”“原始大小”“大 1 号” */
export function fontAdjLabel(adj: number): string {
  const s = adj < 0 ? `小 ${-adj} 号` : adj > 0 ? `大 ${adj} 号` : "原始大小";
  return adj === FONT_ADJ_DEFAULT ? `${s}（默认）` : s;
}

/** Ctrl+滚轮每累计这么多像素的 deltaY 调一号 */
const WHEEL_STEP = 50;

/** Ctrl+滚轮换算为字号步数（上滚为正）：单个事件最多计一步（鼠标一格 ≈ 100px 只调一号），
 *  触控板捏合的小增量累计跨过阈值才调；换向时清零累计。deltaMode 非像素（按行 / 页）时每个事件计一步。返回步数与新的累计值 */
export function wheelSteps(acc: number, deltaY: number, deltaMode: number): { steps: number; acc: number } {
  const d = deltaMode ? Math.sign(deltaY) * WHEEL_STEP : Math.max(-WHEEL_STEP, Math.min(deltaY, WHEEL_STEP));
  if (Math.sign(d) !== Math.sign(acc)) acc = 0;
  acc += d;
  const n = Math.trunc(acc / WHEEL_STEP);
  return { steps: n === 0 ? 0 : -n, acc: acc - n * WHEEL_STEP };
}

/** 终端 OSC 8 超链接（Claude Code 等输出）的打开目标：http(s) → 浏览器；file:// → 本机路径；其它协议忽略 */
export function oscLinkTarget(uri: string): { url: string } | { path: string } | undefined {
  let u: URL;
  try {
    u = new URL(uri);
  } catch {
    return undefined;
  }
  if (u.protocol === "http:" || u.protocol === "https:") return { url: uri };
  if (u.protocol === "file:") return { path: decodeURIComponent(u.pathname).replace(/^\/(?=[A-Za-z]:)/, "").replace(/\//g, "\\") };
  return undefined;
}

const MIB = 1024 ** 2;
const GIB = 1024 ** 3;

/** 会话内存占用显示：不足 1 GB 按 MB 取整（如 812 MB），否则按 GB 保留一位（如 1.3 GB） */
export function formatMemory(bytes: number): string {
  const mb = Math.round(bytes / MIB);
  return mb < 1024 ? `${mb} MB` : `${(bytes / GIB).toFixed(1)} GB`;
}

/** 内存占用超过 1 GB 时用警示色标出 */
export function memoryHigh(bytes: number): boolean {
  return bytes > GIB;
}

/** 粘贴给 Claude Code 的图片路径：含空格时加双引号（与 daemon session::paste_path 一致） */
export function pastedPath(path: string): string {
  return path.includes(" ") ? `"${path}"` : path;
}
