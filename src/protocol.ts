// 与 proto crate 的协议类型保持一致；会话数据全部由 daemon 维护，前端只做展示
import type { ClaudeResume } from "./claude-history";
import type { FsClientMsg, FsServerMsg } from "./files";
import type { GitChanges } from "./git-utils";
import type { HostStatsClientMsg, HostStatsServerMsg } from "./host-info";
import type { HubClientMsg, HubConfig, HubServerMsg } from "./hub";

export type Status = "running" | "waiting" | "exited";

/** 会话中 Claude Code 的状态（Claude hooks 上报） */
export interface ClaudeState {
  state: "idle" | "thinking" | "waiting" | "background" | "done";
  message?: string;
  /** background 时仍在运行的后台任务数 */
  count?: number;
  at: number;
}

export interface SessionInfo {
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
  /** 正被 hub 远程接管（未接管时不带该字段） */
  takenOver?: boolean;
  /** 会话中 Claude Code 对话的 session_id */
  claudeSession?: string;
  /** SSH 会话的文件树正以 root 浏览（未提权时不带该字段） */
  fsRoot?: boolean;
  /** 用户手动改过名：显示名用 name，不被 Claude 对话标题覆盖（未改过时不带该字段） */
  nameCustom?: boolean;
  /** 会话中 Claude Code 对话的标题（Claude 退出后保留最后一次的值） */
  claudeTitle?: string;
}

export interface Host {
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

export type PromptMsg = {
  type: "prompt";
  reqId: number;
  /** sudo：文件树以 root 浏览时的 sudo 密码（不提供保存） */
  kind: "password" | "passphrase" | "hostKey" | "sudo";
  host: string;
  message: string;
  fingerprint?: string;
};

export type Target = { kind: "local"; cwd: string } | { kind: "remote"; hostId: string; cwd?: string };

/** 最近关闭的会话目录，daemon 维护，最新在前 */
export interface RecentItem {
  target: Target;
  /** 远程主机显示名（记录时），本地为 null */
  host: string | null;
  closedAt: number;
}

export type ServerMsg =
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
  | { type: "testHost"; reqId: number; elapsedMs: number | null; error: string | null }
  /** 本地会话进程树内存（字节）全量，旧 daemon 不发 */
  | { type: "memory"; items: { id: number; bytes: number }[] }
  | { type: "resetModes"; id: number }
  /** Git 的结果（旧 daemon 不回复） */
  | { type: "git"; reqId: number; out: GitOut | null; error: string | null };

/** 远程会话的 git 查询（本地由 Tauri 直接运行 git） */
export type GitOp = { kind: "changes"; dir: string; logN?: number } | { kind: "diff"; dir: string; path: string; untracked: boolean };
export type GitOut = ({ kind: "changes" } & GitChanges) | { kind: "diff"; text: string; truncated: boolean };

export interface ShellOption {
  name: string;
  command: string;
}

export interface Config {
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

export interface CompletionItem {
  text: string;
  desc: string;
}

/** 外观：窗口透明度 */
export interface Appearance {
  /** 窗口背景不透明度（%），30–100 */
  opacity: number;
}

export type ClientMsg =
  | { type: "list" }
  | { type: "create"; target: Target; cols: number; rows: number; command?: string; resume?: ClaudeResume }
  | { type: "attach"; id: number; history?: boolean }
  | { type: "close"; id: number }
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
  /** 界面语言（解析后的代码），daemon 据此生成通知与提示文字；旧 daemon 忽略 */
  | { type: "setLocale"; locale: string }
  | FsClientMsg
  | HostStatsClientMsg
  | HubClientMsg
  | { type: "testHost"; reqId: number; host: Host; password: string | null; useSaved: boolean }
  | { type: "git"; reqId: number; sessionId: number; op: GitOp };
