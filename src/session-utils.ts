// 会话的显示辅助：Claude 状态胶囊、shell 名称 / 类型、相对时间等（侧栏、卡片头、终端区共用）
import type { ShellKind } from "./claude-complete";
import { fmtAgo, fmtDate, fmtDaysAgo, t } from "./i18n";
import type { ClaudeState, SessionInfo } from "./protocol";

/** PowerShell 7 安装命令：新建本地会话运行 winget（用户可见进度、可响应 UAC / 协议确认） */
export const PWSH_INSTALL_CMD = "winget install --id Microsoft.PowerShell -e --source winget";

/** 卡片标题行中本地 shell 的显示名（按程序文件名，不含 .exe） */
const SHELL_NAMES: Record<string, string> = {
  pwsh: "PowerShell 7",
  powershell: "Windows PowerShell",
  bash: "Git Bash",
  wsl: "WSL",
};

const claudeLabel = (state: ClaudeState["state"]) => (state === "idle" ? "Claude" : t(`claude.${state}`));

/** Claude 状态胶囊：圆点 + 状态文字（思考中圆点呼吸），等待确认时悬停显示消息；无状态时隐藏 */
export function renderClaude(el: HTMLElement, c: ClaudeState | undefined) {
  el.hidden = !c;
  if (!c) return;
  el.className = `cc ${c.state}`;
  el.innerHTML = "<i></i>";
  el.append(`${claudeLabel(c.state)}${c.state === "background" && c.count ? ` ×${c.count}` : ""}`);
  el.title = c.message ?? "Claude Code";
}

/** 补全用的 shell 类型：远程为 posix；本地按命令的程序名，识别不了时按默认 shell（auto 为 PowerShell） */
export function shellKind(s: SessionInfo, defaultShell: string): ShellKind {
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

/** 侧栏与卡片头显示的会话名：手动改过的名字 > Claude 对话标题 > 原名 */
export function displayName(s: Pick<SessionInfo, "name" | "nameCustom" | "claudeTitle">): string {
  return (!s.nameCustom && s.claudeTitle) || s.name;
}

/** 悬停提示中的名字：显示的是对话标题时附上原名 */
export function nameTooltip(s: SessionInfo): string {
  const shown = displayName(s);
  return shown === s.name ? shown : `${shown}\n${t("sidebar.originalName", { name: s.name })}`;
}

export function shellLabel(s: SessionInfo) {
  // 远程会话的 command 即 "ssh user@host"
  if (s.host) return s.command;
  const m = s.command.match(/^"([^"]*)"|^(\S*)/);
  const prog = m?.[1] ?? m?.[2] ?? "";
  const base = (prog.split(/[\\/]/).pop() ?? "").replace(/\.exe$/i, "").toLowerCase();
  if (base === "cmd") return t("shell.cmd");
  return SHELL_NAMES[base] ?? base;
}

/** 过长时保留首尾、中间用 … 省略 */
export function middleEllipsis(s: string, max: number) {
  if (s.length <= max) return s;
  const head = Math.ceil((max - 1) / 2);
  return `${s.slice(0, head)}…${s.slice(s.length - (max - 1 - head))}`;
}

export function relativeTime(ms: number) {
  const min = Math.floor((Date.now() - ms) / 60_000);
  if (min < 24 * 60) return fmtAgo((Date.now() - ms) / 1000);
  const day = new Date(ms);
  day.setHours(0, 0, 0, 0);
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const days = Math.round((today.getTime() - day.getTime()) / 86_400_000);
  if (days < 30) return fmtDaysAgo(Math.max(1, days));
  return fmtDate(ms);
}
