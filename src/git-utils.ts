// git 输出解析的小纯函数（无副作用，便于单元测试）；git 命令由后端 src-tauri/src/lib.rs 执行
import { fmtAgo } from "./i18n";

export interface Refs {
  /** 当前分支，分离 HEAD 时为 null */
  current: string | null;
  locals: string[];
  /** 按远程分组，分支名带远程前缀（如 origin/main） */
  remotes: { remote: string; branches: string[] }[];
}

/** `git for-each-ref --format='%(HEAD) %(refname)' refs/heads refs/remotes` 的输出；排除 <remote>/HEAD */
export function parseRefs(out: string): Refs {
  const refs: Refs = { current: null, locals: [], remotes: [] };
  for (const line of out.split(/\r?\n/)) {
    const ref = line.slice(2);
    if (ref.startsWith("refs/heads/")) {
      const name = ref.slice(11);
      refs.locals.push(name);
      if (line[0] === "*") refs.current = name;
    } else if (ref.startsWith("refs/remotes/")) {
      const name = ref.slice(13);
      const remote = name.split("/")[0];
      if (name === `${remote}/HEAD` || name === remote) continue;
      let g = refs.remotes.find((r) => r.remote === remote);
      if (!g) refs.remotes.push((g = { remote, branches: [] }));
      g.branches.push(name);
    }
  }
  return refs;
}

/** 选远程分支 origin/x：已有同名本地分支 x 则直接切换，否则 `git switch --track origin/x` */
export function switchTarget(remoteBranch: string, refs: Refs): { name: string; track: boolean } {
  const local = remoteBranch.slice(remoteBranch.indexOf("/") + 1);
  return refs.locals.includes(local) ? { name: local, track: false } : { name: remoteBranch, track: true };
}

/** 按名称过滤分支（忽略大小写的子串匹配） */
export function filterRefs(names: string[], query: string): string[] {
  const q = query.trim().toLowerCase();
  return q ? names.filter((n) => n.toLowerCase().includes(q)) : names;
}

export interface Commit {
  hash: string;
  short: string;
  /** %D 的各项，如 `HEAD -> main`、`origin/main`、`tag: v1` */
  refs: string[];
  author: string;
  /** 作者时间，Unix 秒 */
  time: number;
  subject: string;
}

/** 一行 `git log --graph`：图形列 + 提交（纯图形连接行没有 commit） */
export interface LogRow {
  graph: string;
  commit?: Commit;
}

/** `git log --graph --format=%x1f%H%x1f%h%x1f%D%x1f%an%x1f%at%x1f%s`：图形列在第一个 \x1f 之前 */
export function parseLog(out: string): LogRow[] {
  const rows: LogRow[] = [];
  for (const line of out.split(/\r?\n/)) {
    if (!line) continue;
    const [graph, hash, short, decor, author, time, ...subject] = line.split("\x1f");
    if (hash === undefined) {
      rows.push({ graph });
      continue;
    }
    const refs = decor ? decor.split(", ") : [];
    rows.push({ graph, commit: { hash, short, refs, author, time: Number(time), subject: subject.join("\x1f") } });
  }
  return rows;
}

/** refs 装饰项分类：`HEAD -> x` / `HEAD` 为 head，`tag: x` 为 tag，其余为 ref */
export function parseDecor(d: string): { name: string; kind: "head" | "tag" | "ref" } {
  if (d.startsWith("HEAD -> ")) return { name: d.slice(8), kind: "head" };
  if (d === "HEAD") return { name: d, kind: "head" };
  if (d.startsWith("tag: ")) return { name: d.slice(5), kind: "tag" };
  return { name: d, kind: "ref" };
}

/** 图形列按 lane 着色：每 2 个字符一条 lane（git --graph 的列宽），空白 lane 为 -1；相邻同 lane 字符合并 */
export function graphRuns(graph: string): { text: string; lane: number }[] {
  const runs: { text: string; lane: number }[] = [];
  [...graph].forEach((ch, i) => {
    const lane = ch === " " ? -1 : i >> 1;
    const last = runs[runs.length - 1];
    if (last && last.lane === lane) last.text += ch;
    else runs.push({ text: ch, lane });
  });
  return runs;
}

export interface FileStat {
  path: string;
  /** 二进制文件为 null */
  add: number | null;
  del: number | null;
}

/** `git show --numstat --format=` 的输出：`增\t删\t路径`，二进制文件为 `-\t-\t路径` */
export function parseNumstat(out: string): FileStat[] {
  const files: FileStat[] = [];
  for (const line of out.split(/\r?\n/)) {
    const m = line.match(/^(\d+|-)\t(\d+|-)\t(.*)$/);
    if (m) files.push({ path: m[3], add: m[1] === "-" ? null : Number(m[1]), del: m[2] === "-" ? null : Number(m[2]) });
  }
  return files;
}

export type DiffKind = "meta" | "hunk" | "add" | "del" | "ctx";

export interface DiffLine {
  text: string;
  kind: DiffKind;
  /** 旧文件 / 新文件中的行号（按 @@ -a,b +c,d @@ 推算）；增行无 old，删行无 new，meta / hunk 都没有 */
  old?: number;
  new?: number;
}

/** diff 逐行分类，最多 maxLines 行。文件头（diff --git / index / --- / +++ 等）为 meta；
 *  只有 hunk 内才按首字符判断增删，避免把内容以 -- / ++ 开头的改动行误判为文件头；
 *  hunk 内的「\ No newline at end of file」为 meta，不占行号 */
export function classifyDiff(text: string, maxLines: number): { lines: DiffLine[]; truncated: boolean } {
  const all = text.split(/\r?\n/);
  if (all[all.length - 1] === "") all.pop();
  const lines: DiffLine[] = [];
  let inHunk = false;
  let o = 0;
  let n = 0;
  for (const line of all.slice(0, maxLines)) {
    if (line.startsWith("diff ")) {
      inHunk = false;
      lines.push({ text: line, kind: "meta" });
    } else if (line.startsWith("@@")) {
      inHunk = true;
      const m = /^@@+ -(\d+)(?:,\d+)? \+(\d+)/.exec(line);
      if (m) [o, n] = [Number(m[1]), Number(m[2])];
      lines.push({ text: line, kind: "hunk" });
    } else if (!inHunk || line[0] === "\\") lines.push({ text: line, kind: "meta" });
    else if (line[0] === "+") lines.push({ text: line, kind: "add", new: n++ });
    else if (line[0] === "-") lines.push({ text: line, kind: "del", old: o++ });
    else lines.push({ text: line, kind: "ctx", old: o++, new: n++ });
  }
  return { lines, truncated: all.length > maxLines };
}

/** 提交的相对时间（sec 为 Unix 秒，now 为毫秒） */
export function relTime(sec: number, now: number): string {
  return fmtAgo(now / 1000 - sec);
}

// ---------------- 未提交改动（git status --porcelain=v2 -z / git diff --numstat -z） ----------------

/** 改动类型；untracked 只标「新」，不统计行数 */
export type ChangeKind = "modified" | "added" | "deleted" | "renamed" | "conflict" | "untracked";

export interface StatusEntry {
  path: string;
  /** 重命名 / 复制前的路径 */
  orig?: string;
  /** 两位状态码（暂存区、工作区），未跟踪为 `??` */
  xy: string;
  kind: ChangeKind;
}

export interface StatusV2 {
  /** 当前分支；分离 HEAD 时为 null */
  branch: string | null;
  /** HEAD 提交；空仓库为 null */
  oid: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  entries: StatusEntry[];
}

const kindOf = (xy: string): ChangeKind => (xy.includes("D") ? "deleted" : xy.includes("A") ? "added" : "modified");

/** `git status --porcelain=v2 -z --branch` 的输出：条目以 NUL 分隔，重命名条目（2）后紧跟原路径一项；忽略的文件（!）跳过 */
export function parseStatusV2(out: string): StatusV2 {
  const st: StatusV2 = { branch: null, oid: null, upstream: null, ahead: 0, behind: 0, entries: [] };
  const items = out.split("\0");
  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    if (it.startsWith("# ")) {
      const [, key, ...rest] = it.split(" ");
      const v = rest.join(" ");
      if (key === "branch.oid") st.oid = v === "(initial)" ? null : v;
      else if (key === "branch.head") st.branch = v === "(detached)" ? null : v;
      else if (key === "branch.upstream") st.upstream = v;
      else if (key === "branch.ab") {
        const m = v.match(/^\+(\d+) -(\d+)$/);
        if (m) [st.ahead, st.behind] = [Number(m[1]), Number(m[2])];
      }
      continue;
    }
    // 各类条目的字段数固定，路径为最后一个字段（可含空格）
    const f = (n: number) => {
      const parts = it.split(" ");
      return { xy: parts[1], path: parts.slice(n).join(" ") };
    };
    if (it.startsWith("1 ")) {
      const { xy, path } = f(8);
      st.entries.push({ path, xy, kind: kindOf(xy) });
    } else if (it.startsWith("2 ")) {
      const { xy, path } = f(9);
      st.entries.push({ path, orig: items[++i], xy, kind: "renamed" });
    } else if (it.startsWith("u ")) {
      const { xy, path } = f(10);
      st.entries.push({ path, xy, kind: "conflict" });
    } else if (it.startsWith("? ")) {
      st.entries.push({ path: it.slice(2), xy: "??", kind: "untracked" });
    }
  }
  return st;
}

/** `git diff --numstat -z`：`增\t删\t路径`，重命名为 `增\t删\t` 后跟原路径、新路径两项；二进制文件增删为 null */
export function parseNumstatZ(out: string): (FileStat & { orig?: string })[] {
  const files: (FileStat & { orig?: string })[] = [];
  const items = out.split("\0");
  for (let i = 0; i < items.length; i++) {
    const m = items[i].match(/^(\d+|-)\t(\d+|-)\t([\s\S]*)$/);
    if (!m) continue;
    const num = (s: string) => (s === "-" ? null : Number(s));
    if (m[3]) files.push({ path: m[3], add: num(m[1]), del: num(m[2]) });
    else {
      files.push({ orig: items[i + 1], path: items[i + 2] ?? "", add: num(m[1]), del: num(m[2]) });
      i += 2;
    }
  }
  return files;
}

/** git_changes / daemon Git 的原始输出（与 proto::GitChanges 一致） */
export interface GitChanges {
  root: string;
  status: string;
  numstat: string;
  log: string;
}

export interface ChangedFile extends StatusEntry {
  /** 相对 HEAD 的增删行数；二进制与未跟踪文件为 null */
  add: number | null;
  del: number | null;
}

/** 一个工作区的未提交改动 */
export interface ChangeSet {
  root: string;
  /** 分支名；分离 HEAD 时为提交前 7 位，空仓库为 branch.head */
  branch: string;
  ahead: number;
  behind: number;
  files: ChangedFile[];
  /** 已跟踪文件的增删行数合计（未跟踪文件不计） */
  added: number;
  deleted: number;
  /** 最近的提交 */
  commits: Commit[];
}

/** 合并 status 与 numstat：文件列表以 status 为准，行数按（新）路径取 numstat */
export function buildChangeSet(raw: GitChanges): ChangeSet {
  const st = parseStatusV2(raw.status);
  const nums = new Map(parseNumstatZ(raw.numstat).map((f) => [f.path, f]));
  const files = st.entries.map((e): ChangedFile => {
    const n = e.kind === "untracked" ? undefined : nums.get(e.path);
    return { ...e, add: n?.add ?? null, del: n?.del ?? null };
  });
  const commits = parseLog(raw.log.replace(/^(?=.)/gm, "\x1f")).flatMap((r) => (r.commit ? [r.commit] : []));
  return {
    root: raw.root,
    branch: st.branch ?? st.oid?.slice(0, 7) ?? "",
    ahead: st.ahead,
    behind: st.behind,
    files,
    added: files.reduce((s, f) => s + (f.add ?? 0), 0),
    deleted: files.reduce((s, f) => s + (f.del ?? 0), 0),
    commits,
  };
}
