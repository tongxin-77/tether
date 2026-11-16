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

/** 终端字号：4K 及以上（屏幕物理宽度 ≥ 3840）用 14，其余用更紧凑的 13 */
export function terminalFontSize(screenWidth: number, dpr: number): number {
  return screenWidth * dpr >= 3840 ? 14 : 13;
}
