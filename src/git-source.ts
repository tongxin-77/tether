// git 数据源：屏蔽本地 / 远程差异。本地会话由 Tauri 直接运行 git（git_changes / git_file_diff），
// 远程会话经 daemon 在 SSH 连接上运行（ClientMsg::Git，以 reqId 对应回复）
import { invoke } from "@tauri-apps/api/core";
import { type ChangeSet, type Commit, type GitChanges, buildChangeSet, parseLog } from "./git-utils";
import type { ClientMsg, GitOp, GitOut, ServerMsg } from "./protocol";

/** 本地，或远程会话（借用该会话的 SSH 连接） */
export type GitTarget = { kind: "local" } | { kind: "remote"; sessionId: number };

/** 远程请求等待超时：daemon 远程命令超时为 8 秒，超过这个时间仍未回复视为不认识该消息的旧版 daemon */
export class GitTimeout extends Error {}

const TIMEOUT = 15000;

export interface GitSource {
  /** 未提交改动与最近 logN 个提交 */
  changes(target: GitTarget, dir: string, logN?: number): Promise<ChangeSet>;
  /** 单个文件相对 HEAD 的 diff；untracked 与 /dev/null 比较 */
  diff(target: GitTarget, dir: string, path: string, untracked: boolean): Promise<{ text: string; truncated: boolean }>;
  /** 最近 limit 个提交（仅本地，远程还没有通道） */
  log(dir: string, limit: number): Promise<Commit[]>;
  /** 提交改动：numstat 与 patch（仅本地） */
  show(dir: string, hash: string): Promise<{ numstat: string; diff: string; truncated: boolean }>;
  onMessage(m: Extract<ServerMsg, { type: "git" }>): void;
  /** 与 daemon 的连接断开：未回复的请求不会再有回复，以普通错误结束（不算超时） */
  reset(): void;
}

export function createGitSource(send: (msg: ClientMsg) => void): GitSource {
  /** 与文件树一样错开 reqId 起点，避免误认别的窗口的回复 */
  let seq = Math.floor(Math.random() * 2 ** 40);
  const pending = new Map<number, { resolve(out: GitOut): void; reject(e: Error): void; timer: number }>();

  function request(sessionId: number, op: GitOp): Promise<GitOut> {
    const reqId = ++seq;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(reqId);
        reject(new GitTimeout());
      }, TIMEOUT);
      pending.set(reqId, { resolve, reject, timer });
      send({ type: "git", reqId, sessionId, op });
    });
  }

  return {
    async changes(target, dir, logN = 5) {
      let raw: GitChanges;
      if (target.kind === "local") raw = await invoke<GitChanges>("git_changes", { dir, logN });
      else {
        const out = await request(target.sessionId, { kind: "changes", dir, logN });
        if (out.kind !== "changes") throw new Error("unexpected git reply");
        raw = out;
      }
      return buildChangeSet(raw);
    },
    async diff(target, dir, path, untracked) {
      if (target.kind === "local") {
        const [text, truncated] = await invoke<[string, boolean]>("git_file_diff", { dir, path, untracked });
        return { text, truncated };
      }
      const out = await request(target.sessionId, { kind: "diff", dir, path, untracked });
      if (out.kind !== "diff") throw new Error("unexpected git reply");
      return { text: out.text, truncated: out.truncated };
    },
    async log(dir, limit) {
      const out = await invoke<string>("git_log", { dir, limit });
      return parseLog(out).flatMap((r) => (r.commit ? [r.commit] : []));
    },
    async show(dir, hash) {
      const [numstat, diff, truncated] = await invoke<[string, string, boolean]>("git_show", { dir, hash });
      return { numstat, diff, truncated };
    },
    onMessage(m) {
      const p = pending.get(m.reqId);
      if (!p) return;
      pending.delete(m.reqId);
      clearTimeout(p.timer);
      if (m.out) p.resolve(m.out);
      else p.reject(new Error(m.error ?? "git failed"));
    },
    reset() {
      for (const p of pending.values()) {
        clearTimeout(p.timer);
        p.reject(new Error("disconnected"));
      }
      pending.clear();
    },
  };
}
