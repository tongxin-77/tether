// 工作区：侧栏按工作区给会话分组。本地按所在 git 仓库根（不在仓库里按启动目录），远程按「主机 id:仓库根」（还没查到仓库根时按启动目录）
import type { SessionInfo, Target } from "./protocol";

/** Tauri git_head 的结果：仓库根目录与当前分支 */
export interface GitHead {
  root: string;
  branch: string;
}

/** 启动目录 → 仓库根与分支；null 为不在仓库里，还没查询的目录没有该键。本地键为启动目录，远程键见 remoteRootKey */
export type Roots = ReadonlyMap<string, GitHead | null>;

/** 远程会话在 Roots 中的键（由 git 改动查询的结果写入）；本地路径不会以 ssh: 开头 */
export const remoteRootKey = (s: SessionInfo) => `ssh:${s.hostId ?? s.host}:${s.cwd}`;

export interface Workspace {
  /** 分组键：本地为规范化的仓库根 / 启动目录，远程为 `hostId:仓库根`（未知时为 `hostId:cwd`） */
  key: string;
  /** 显示名：目录名 */
  name: string;
  /** 仓库根或启动目录的完整路径 */
  dir: string;
  /** 远程主机显示名，本地为 null */
  host: string | null;
  /** 仓库的当前分支；不在仓库里或还没查到为 null */
  branch: string | null;
  /** 在此新建的目标；远程会话缺 hostId 时为 undefined */
  target: Target | undefined;
  sessions: SessionInfo[];
}

/** 侧栏行的状态：● thinking / background，▲ waiting，✓ done，○ shell（无 Claude 或空闲），exited 已退出 */
export type RowState = "thinking" | "background" | "waiting" | "done" | "shell" | "exited";

const isWinPath = (p: string) => /^[a-zA-Z]:([\\/]|$)/.test(p) || p.startsWith("\\\\");

/** 比较用的路径：去掉末尾分隔符；Windows 路径统一为反斜杠并忽略大小写 */
export function normPath(p: string): string {
  if (!isWinPath(p)) return p.length > 1 ? p.replace(/\/+$/, "") : p;
  const s = p.replace(/\//g, "\\").replace(/\\+$/, "");
  return (/^[a-zA-Z]:$/.test(s) ? `${s}\\` : s).toLowerCase();
}

/** 路径最后一段（根目录返回原样） */
export function baseName(p: string): string {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts.length > 1 || (parts.length === 1 && !/^[a-zA-Z]:$/.test(parts[0])) ? parts[parts.length - 1] : p;
}

/** 会话所属工作区（不含会话列表） */
export function workspaceOf(s: SessionInfo, roots: Roots): Omit<Workspace, "sessions"> {
  if (s.host !== null) {
    const head = roots.get(remoteRootKey(s));
    const dir = head?.root ?? s.cwd;
    return {
      key: `${s.hostId ?? s.host}:${dir}`,
      name: baseName(dir) || "~",
      dir,
      host: s.host,
      branch: head?.branch ?? null,
      target: s.hostId ? { kind: "remote", hostId: s.hostId, cwd: dir || undefined } : undefined,
    };
  }
  const head = roots.get(s.cwd);
  const dir = head?.root ?? s.cwd;
  return { key: normPath(dir), name: baseName(dir), dir, host: null, branch: head?.branch ?? null, target: { kind: "local", cwd: dir } };
}

/** 按工作区分组：组按其第一个会话在列表中的先后排列，组内保持原顺序 */
export function groupSessions(sessions: readonly SessionInfo[], roots: Roots): Workspace[] {
  const groups = new Map<string, Workspace>();
  for (const s of sessions) {
    const w = workspaceOf(s, roots);
    const g = groups.get(w.key);
    if (g) g.sessions.push(s);
    else groups.set(w.key, { ...w, sessions: [s] });
  }
  return [...groups.values()];
}

export function rowState(s: SessionInfo): RowState {
  if (s.status === "exited") return "exited";
  if (s.status === "waiting" || s.claude?.state === "waiting") return "waiting";
  const c = s.claude?.state;
  return c === "thinking" || c === "background" || c === "done" ? c : "shell";
}

/** 组头的状态汇总：运行中（思考 / 后台）、待确认、已完成、shell；已退出的不计 */
export function summarize(sessions: readonly SessionInfo[]) {
  const n = { run: 0, wait: 0, done: 0, shell: 0 };
  for (const s of sessions) {
    const st = rowState(s);
    if (st === "thinking" || st === "background") n.run++;
    else if (st === "waiting") n.wait++;
    else if (st === "done") n.done++;
    else if (st === "shell") n.shell++;
  }
  return n;
}

/** 会话启动目录相对工作区目录的子路径（就是工作区目录本身或不在其下时为空串），按原始大小写返回 */
export function relSubdir(cwd: string, dir: string): string {
  const c = normPath(cwd);
  const d = normPath(dir);
  if (c === d) return "";
  const sep = isWinPath(dir) ? "\\" : "/";
  const prefix = d.endsWith(sep) ? d : d + sep;
  if (!c.startsWith(prefix)) return "";
  return (isWinPath(cwd) ? cwd.replace(/\//g, "\\") : cwd).slice(prefix.length).replace(/[\\/]+$/, "");
}

/** 筛选：会话名、启动目录、主机名包含关键字（忽略大小写）；空关键字全部匹配 */
export function matchSession(s: SessionInfo, query: string): boolean {
  const q = query.trim().toLowerCase();
  return !q || [s.name, s.claudeTitle ?? "", s.cwd, s.host ?? ""].some((v) => v.toLowerCase().includes(q));
}
