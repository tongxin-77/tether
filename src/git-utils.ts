// git 输出解析的小纯函数（无副作用，便于单元测试）；git 命令由后端 src-tauri/src/lib.rs 执行

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

/** diff 逐行分类，最多 maxLines 行。文件头（diff --git / index / --- / +++ 等）为 meta；
 *  只有 hunk 内才按首字符判断增删，避免把内容以 -- / ++ 开头的改动行误判为文件头 */
export function classifyDiff(text: string, maxLines: number): { lines: { text: string; kind: DiffKind }[]; truncated: boolean } {
  const all = text.split(/\r?\n/);
  if (all[all.length - 1] === "") all.pop();
  const lines: { text: string; kind: DiffKind }[] = [];
  let inHunk = false;
  for (const line of all.slice(0, maxLines)) {
    let kind: DiffKind;
    if (line.startsWith("diff ")) {
      inHunk = false;
      kind = "meta";
    } else if (line.startsWith("@@")) {
      inHunk = true;
      kind = "hunk";
    } else if (!inHunk) kind = "meta";
    else if (line[0] === "+") kind = "add";
    else if (line[0] === "-") kind = "del";
    else kind = "ctx";
    lines.push({ text: line, kind });
  }
  return { lines, truncated: all.length > maxLines };
}

/** 提交的相对时间（sec 为 Unix 秒，now 为毫秒） */
export function relTime(sec: number, now: number): string {
  const s = Math.max(0, now / 1000 - sec);
  if (s < 60) return "刚刚";
  if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时前`;
  const days = Math.floor(s / 86400);
  if (days < 30) return `${days} 天前`;
  if (days < 365) return `${Math.floor(days / 30)} 个月前`;
  return `${Math.floor(days / 365)} 年前`;
}
