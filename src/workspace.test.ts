import { describe, expect, it } from "vitest";
import type { SessionInfo } from "./protocol";
import { type GitHead, baseName, groupSessions, remoteRootKey, matchSession, normPath, relSubdir, rowState, summarize } from "./workspace";

let seq = 0;
const s = (cwd: string, extra: Partial<SessionInfo> = {}): SessionInfo => ({
  id: ++seq,
  name: `s${seq}`,
  cwd,
  command: "pwsh.exe",
  status: "running",
  startedAt: 0,
  lastActivityAt: 0,
  host: null,
  ...extra,
});
const remote = (hostId: string, host: string, cwd: string, extra: Partial<SessionInfo> = {}) => s(cwd, { host, hostId, command: `ssh ${host}`, ...extra });
const roots = (entries: [string, GitHead | null][]) => new Map(entries);

describe("groupSessions", () => {
  it("本地按仓库根分组，Windows 下忽略大小写与分隔符差异", () => {
    const a = s("C:\\work\\tether");
    const b = s("C:\\work\\tether\\src-tauri");
    const c = s("c:/Work/Tether/");
    const r = roots([
      [a.cwd, { root: "C:\\work\\tether", branch: "main" }],
      [b.cwd, { root: "C:\\work\\tether", branch: "main" }],
      [c.cwd, { root: "c:/Work/Tether", branch: "main" }],
    ]);
    const g = groupSessions([a, b, c], r);
    expect(g).toHaveLength(1);
    expect(g[0]).toMatchObject({ name: "tether", dir: "C:\\work\\tether", branch: "main", host: null, target: { kind: "local", cwd: "C:\\work\\tether" } });
    expect(g[0].sessions).toEqual([a, b, c]);
  });

  it("不在仓库里（或还未查询）按启动目录分组", () => {
    const a = s("C:\\Users\\me\\Downloads");
    const b = s("C:\\Users\\me\\Downloads\\x");
    const c = s("C:\\Users\\me\\Downloads");
    const g = groupSessions([a, b, c], roots([[a.cwd, null]]));
    expect(g.map((w) => [w.name, w.branch, w.sessions.length])).toEqual([
      ["Downloads", null, 2],
      ["x", null, 1],
    ]);
  });

  it("远程按 hostId:cwd 分组；不同主机上的相同路径分开；本地同路径也分开", () => {
    const a = remote("h1", "dev", "/srv/app");
    const b = remote("h1", "dev", "/srv/app");
    const c = remote("h2", "prod", "/srv/app");
    const d = s("/srv/app");
    const g = groupSessions([a, b, c, d], roots([]));
    expect(g.map((w) => [w.key, w.host, w.sessions.length])).toEqual([
      ["h1:/srv/app", "dev", 2],
      ["h2:/srv/app", "prod", 1],
      ["/srv/app", null, 1],
    ]);
    expect(g[0].target).toEqual({ kind: "remote", hostId: "h1", cwd: "/srv/app" });
    expect(g[0].name).toBe("app");
  });

  it("远程查到仓库根后按 hostId:仓库根 分组，子目录会话归到同组并带分支", () => {
    const a = remote("h1", "dev", "/srv/app");
    const b = remote("h1", "dev", "/srv/app/web");
    const head = { root: "/srv/app", branch: "feat/x" };
    const g = groupSessions([a, b], roots([[remoteRootKey(a), head], [remoteRootKey(b), head]]));
    expect(g.map((w) => [w.key, w.dir, w.branch, w.sessions.length])).toEqual([["h1:/srv/app", "/srv/app", "feat/x", 2]]);
    expect(g[0].target).toEqual({ kind: "remote", hostId: "h1", cwd: "/srv/app" });
    // 本地同路径的键不受远程条目影响
    expect(remoteRootKey(s("/srv/app"))).not.toBe("/srv/app");
  });

  it("组按第一个会话出现的先后排列", () => {
    const a = s("C:\\a");
    const b = remote("h1", "dev", "/x");
    const c = s("C:\\A\\");
    const g = groupSessions([b, a, c], roots([]));
    expect(g.map((w) => w.sessions.map((i) => i.id))).toEqual([[b.id], [a.id, c.id]]);
  });

  it("远程会话缺 hostId 时没有新建目标", () => {
    const g = groupSessions([s("/x", { host: "old" })], roots([]));
    expect(g[0].target).toBeUndefined();
    expect(g[0].key).toBe("old:/x");
  });
});

describe("rowState / summarize", () => {
  it("按 Claude 状态与会话状态得出行状态", () => {
    expect(rowState(s("C:\\", { claude: { state: "thinking", at: 0 } }))).toBe("thinking");
    expect(rowState(s("C:\\", { claude: { state: "background", at: 0, count: 2 } }))).toBe("background");
    expect(rowState(s("C:\\", { claude: { state: "waiting", at: 0 } }))).toBe("waiting");
    expect(rowState(s("C:\\", { status: "waiting" }))).toBe("waiting");
    expect(rowState(s("C:\\", { claude: { state: "done", at: 0 } }))).toBe("done");
    expect(rowState(s("C:\\", { claude: { state: "idle", at: 0 } }))).toBe("shell");
    expect(rowState(s("C:\\"))).toBe("shell");
    expect(rowState(s("C:\\", { status: "exited", claude: { state: "thinking", at: 0 } }))).toBe("exited");
  });

  it("汇总运行中、待确认、完成、shell，不计已退出", () => {
    const list = [
      s("C:\\", { claude: { state: "thinking", at: 0 } }),
      s("C:\\", { claude: { state: "background", at: 0 } }),
      s("C:\\", { claude: { state: "waiting", at: 0 } }),
      s("C:\\", { claude: { state: "done", at: 0 } }),
      s("C:\\"),
      s("C:\\", { status: "exited" }),
    ];
    expect(summarize(list)).toEqual({ run: 2, wait: 1, done: 1, shell: 1 });
  });
});

describe("relSubdir", () => {
  it("仓库子目录显示相对路径，保留原始大小写", () => {
    expect(relSubdir("C:\\work\\tether\\src-tauri\\src", "C:\\work\\tether")).toBe("src-tauri\\src");
    expect(relSubdir("c:/Work/Tether/Docs/", "C:\\work\\tether")).toBe("Docs");
    expect(relSubdir("/srv/app/logs", "/srv/app")).toBe("logs");
  });

  it("就是根目录、不在其下或仅前缀相同时为空", () => {
    expect(relSubdir("C:\\work\\tether\\", "C:\\work\\tether")).toBe("");
    expect(relSubdir("C:\\work\\tether-hub", "C:\\work\\tether")).toBe("");
    expect(relSubdir("/srv/app", "/srv/app")).toBe("");
    expect(relSubdir("D:\\x", "C:\\")).toBe("");
    expect(relSubdir("C:\\x", "C:\\")).toBe("x");
  });
});

describe("路径辅助与筛选", () => {
  it("normPath / baseName", () => {
    expect(normPath("C:/A/b/")).toBe("c:\\a\\b");
    expect(normPath("C:")).toBe("c:\\");
    expect(normPath("/Home/x/")).toBe("/Home/x");
    expect(normPath("/")).toBe("/");
    expect(baseName("C:\\work\\tether\\")).toBe("tether");
    expect(baseName("/home/deploy")).toBe("deploy");
    expect(baseName("C:\\")).toBe("C:\\");
    expect(baseName("/")).toBe("/");
  });

  it("按会话名、路径、主机筛选，忽略大小写", () => {
    const a = remote("h1", "Prod-Box", "/srv/app", { name: "日志排查" });
    expect(matchSession(a, "")).toBe(true);
    expect(matchSession(a, "prod")).toBe(true);
    expect(matchSession(a, "SRV")).toBe(true);
    expect(matchSession(a, "日志")).toBe(true);
    expect(matchSession(a, "tether")).toBe(false);
  });
});
