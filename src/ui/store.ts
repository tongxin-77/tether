// 应用状态：单一 store，组件按需 select；set 浅合并后同步通知订阅者（按订阅顺序）
import type { Host, RecentItem, SessionInfo } from "../protocol";
import type { ChangeSet } from "../git-utils";
import type { Roots } from "../workspace";

export interface Store<S> {
  get(): S;
  set(patch: Partial<S>): void;
  /** pick 的结果变化（默认 Object.is 比较）时调用 fn；返回取消订阅 */
  select<T>(pick: (s: S) => T, fn: (value: T, prev: T) => void, eq?: (a: T, b: T) => boolean): () => void;
}

export function createStore<S extends object>(initial: S): Store<S> {
  let state = initial;
  const subs = new Set<(s: S) => void>();
  return {
    get: () => state,
    set(patch) {
      state = { ...state, ...patch };
      for (const f of [...subs]) f(state);
    },
    select(pick, fn, eq = Object.is) {
      let last = pick(state);
      const f = (s: S) => {
        const v = pick(s);
        if (eq(v, last)) return;
        const prev = last;
        last = v;
        fn(v, prev);
      };
      subs.add(f);
      return () => subs.delete(f);
    },
  };
}

export interface AppState {
  /** daemon 推送的会话（不含已拖成独立窗口的），按首次出现的先后排列 */
  sessions: SessionInfo[];
  /** 已收到过会话列表（之前不显示空状态） */
  sessionsLoaded: boolean;
  activeId: number | undefined;
  /** 有未读通知的会话 */
  unread: ReadonlySet<number>;
  /** 本地会话启动目录所在的 git 仓库（侧栏分组），app 按会话列表查询 */
  roots: Roots;
  /** 工作区 key → 未提交改动（changes-refresher 维护）；没有数据、不是仓库或查询失败时没有该键 */
  changes: ReadonlyMap<string, ChangeSet>;
  /** 本地会话进程树内存（字节），daemon 每次推送全量 */
  memory: ReadonlyMap<number, number>;
  /** daemon 返回的合并主机列表（新建菜单、最近关闭） */
  hosts: Host[];
  /** 是否已收到过 config：之前 hosts 为空，不据此把远程记录判为“主机已删除” */
  hostsLoaded: boolean;
  recent: RecentItem[];
  /** 配置中的默认 shell（补全按它判断 shell 类型） */
  defaultShell: string;
  /** daemon 探测到 PowerShell 7；收到 config 前视为已安装 */
  pwshDetected: boolean;
  /** 与 daemon 的连接已断开、重连中 */
  disconnected: boolean;
  /** 用户指定的终端字号，null 为自动 */
  termFont: number | null;
  /** GPU 加速渲染开关 */
  gpu: boolean;
  /** 变更面板是否打开 */
  changesOpen: boolean;
  /** 变更面板显示的工作区 key；null 跟随当前会话（切换会话时复位） */
  changesWs: string | null;
}

/** 逐项 Object.is 比较（select 多个字段时用） */
export const shallow = (a: readonly unknown[], b: readonly unknown[]) => a.length === b.length && a.every((x, i) => Object.is(x, b[i]));

/** 当前激活的会话 */
export const activeSession = (s: AppState) => (s.activeId === undefined ? undefined : s.sessions.find((i) => i.id === s.activeId));
