// 未提交改动的刷新：按工作区 key 合并请求（同一 key 同时只有一个请求在跑，期间再要求刷新则结束后补一次），结果写入 store.changes。
// 时机：Claude 一轮结束（thinking → done / background / waiting）后 800ms；窗口获得焦点、切换会话立即；新出现的工作区查一次；
// 轮询本地当前工作区 5s、其他本地工作区 30s，远程只靠事件；变更面板打开时立即查其工作区，并轮询（本地 5s、远程 15s）；窗口隐藏时跳过。
// 远程的结果同时带回仓库根与分支，写入 store.roots，侧栏据此把远程会话按仓库根分组
import { type GitSource, type GitTarget, GitTimeout } from "./git-source";
import type { ChangeSet } from "./git-utils";
import { type AppState, type Store, activeSession, shallow } from "./ui/store";
import { type GitHead, type Workspace, groupSessions, remoteRootKey, workspaceOf } from "./workspace";

const TURN_DELAY = 800;
const ACTIVE_POLL = 5000;
const OTHERS_POLL = 30_000;
const PANEL_REMOTE_POLL = 15_000;
const TURN_END = new Set(["done", "background", "waiting"]);

export function createChangesRefresher(store: Store<AppState>, source: GitSource) {
  /** 正在查询的工作区，与查询期间又被要求刷新的 */
  const running = new Set<string>();
  const again = new Set<string>();
  /** 已查询过的工作区（新出现的工作区查一次） */
  const seen = new Set<string>();
  /** 当前 daemon 不认识 Git 消息（等待超时）：不再发远程请求、不显示；重连后重新判断 */
  let remoteUnsupported = false;

  const workspaces = () => groupSessions(store.get().sessions, store.get().roots);
  const keyOf = (id: number | undefined) => {
    const st = store.get();
    const s = st.sessions.find((i) => i.id === id);
    return s && workspaceOf(s, st.roots);
  };

  /** 本地：git_head 确认在仓库里才查（branch 非 null）；远程：借用一个未退出的会话的 SSH 连接 */
  function targetOf(w: Workspace): GitTarget | undefined {
    if (!w.host) return w.branch !== null ? { kind: "local" } : undefined;
    if (remoteUnsupported) return undefined;
    const s = w.sessions.find((i) => i.status !== "exited");
    return s && { kind: "remote", sessionId: s.id };
  }

  function refresh(key: string | undefined) {
    if (key === undefined || document.hidden) return;
    if (running.has(key)) {
      again.add(key);
      return;
    }
    const w = workspaces().find((i) => i.key === key);
    const target = w && targetOf(w);
    if (!w || !target) return;
    seen.add(key);
    running.add(key);
    void run(w, target).finally(() => {
      running.delete(key);
      if (again.delete(key)) refresh(key);
    });
  }

  async function run(w: Workspace, target: GitTarget) {
    let cs: ChangeSet | undefined;
    try {
      cs = await source.changes(target, w.dir);
    } catch (e) {
      // 不是仓库、没装 git、SSH 断开等：不显示；旧 daemon 超时后不再请求
      if (e instanceof GitTimeout) remoteUnsupported = true;
    }
    const st = store.get();
    let roots = st.roots;
    if (cs && w.host) {
      // 远程：记下各会话启动目录所在的仓库根与分支，结果存到按仓库根算出的新 key 下
      const head: GitHead = { root: cs.root, branch: cs.branch };
      const next = new Map(roots);
      for (const s of w.sessions) next.set(remoteRootKey(s), head);
      if (w.sessions.some((s) => JSON.stringify(roots.get(remoteRootKey(s))) !== JSON.stringify(head))) roots = next;
    }
    const key = cs && w.host ? workspaceOf(w.sessions[0], roots).key : w.key;
    seen.add(key);
    const changes = new Map(st.changes);
    if (key !== w.key) changes.delete(w.key);
    if (cs) changes.set(key, cs);
    else changes.delete(key);
    const same = roots === st.roots && key === w.key && JSON.stringify(st.changes.get(key)) === JSON.stringify(cs);
    if (!same) store.set({ roots, changes });
  }

  const refreshActive = (localOnly = false) => {
    const s = activeSession(store.get());
    if (s && !(localOnly && s.host)) refresh(keyOf(s.id)?.key);
  };
  /** 变更面板显示的工作区（打开时）：选定的、仍存在的工作区，否则为当前会话的 */
  const panelWs = () => {
    const st = store.get();
    if (!st.changesOpen) return undefined;
    const ws = workspaces();
    return ws.find((w) => w.key === st.changesWs) ?? ws.find((w) => w.key === keyOf(st.activeId)?.key);
  };

  // Claude 一轮结束：稍等文件写完再查
  store.select(
    (s) => s.sessions,
    (cur, prev) => {
      for (const s of cur) {
        const old = prev.find((p) => p.id === s.id);
        if (old?.claude?.state === "thinking" && s.claude && TURN_END.has(s.claude.state)) setTimeout(() => refresh(keyOf(s.id)?.key), TURN_DELAY);
      }
    },
  );
  // 新出现的工作区查一次；已不存在的工作区丢弃数据
  store.select(
    (s) => [s.sessions, s.roots],
    () => {
      const ws = workspaces();
      const keys = new Set(ws.map((w) => w.key));
      for (const k of seen) if (!keys.has(k)) seen.delete(k);
      const changes = store.get().changes;
      if ([...changes.keys()].some((k) => !keys.has(k))) store.set({ changes: new Map([...changes].filter(([k]) => keys.has(k))) });
      for (const w of ws) if (!seen.has(w.key)) refresh(w.key);
    },
    shallow,
  );
  store.select((s) => s.activeId, () => refreshActive());
  // daemon 断开：未回复的远程请求作废；重连后的 daemon 可能是新版本，重新判断是否支持
  store.select(
    (s) => s.disconnected,
    (d) => {
      if (!d) return;
      source.reset();
      remoteUnsupported = false;
    },
  );
  // 变更面板打开、切换工作区：立即查
  store.select(
    (s) => [s.changesOpen, s.changesWs],
    () => refresh(panelWs()?.key),
    shallow,
  );
  window.addEventListener("focus", () => refreshActive());
  setInterval(() => {
    refreshActive(true);
    const w = panelWs();
    if (w && !w.host && w.key !== keyOf(store.get().activeId)?.key) refresh(w.key);
  }, ACTIVE_POLL);
  setInterval(() => {
    const w = panelWs();
    if (w?.host) refresh(w.key);
  }, PANEL_REMOTE_POLL);
  setInterval(() => {
    const active = keyOf(store.get().activeId)?.key;
    for (const w of workspaces()) if (!w.host && w.key !== active) refresh(w.key);
  }, OTHERS_POLL);
}
