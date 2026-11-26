import { describe, expect, it } from "vitest";
import { buildChangeSet, classifyDiff, filterRefs, graphRuns, parseDecor, parseLog, parseNumstat, parseNumstatZ, parseRefs, parseStatusV2, relTime, switchTarget } from "./git-utils";

const US = "\x1f";

describe("parseRefs", () => {
  const out = ["* refs/heads/main", "  refs/heads/feature/x", "  refs/remotes/origin/HEAD", "  refs/remotes/origin/main", "  refs/remotes/origin/dev", "  refs/remotes/up/fix", ""].join("\n");

  it("区分当前分支、本地分支与按远程分组的远程分支，排除 <remote>/HEAD", () => {
    expect(parseRefs(out)).toEqual({
      current: "main",
      locals: ["main", "feature/x"],
      remotes: [
        { remote: "origin", branches: ["origin/main", "origin/dev"] },
        { remote: "up", branches: ["up/fix"] },
      ],
    });
  });

  it("CRLF 与空输出", () => {
    expect(parseRefs("* refs/heads/a\r\n  refs/heads/b\r\n").locals).toEqual(["a", "b"]);
    expect(parseRefs("")).toEqual({ current: null, locals: [], remotes: [] });
  });

  it("分离 HEAD 时没有当前分支", () => {
    expect(parseRefs("  refs/heads/main\n").current).toBeNull();
  });
});

describe("switchTarget", () => {
  const refs = parseRefs("* refs/heads/main\n  refs/heads/dev\n  refs/remotes/origin/dev\n  refs/remotes/origin/feat/a\n");

  it("已有同名本地分支：直接切换", () => {
    expect(switchTarget("origin/dev", refs)).toEqual({ name: "dev", track: false });
  });

  it("没有同名本地分支：--track 创建（传完整远程分支名）", () => {
    expect(switchTarget("origin/feat/a", refs)).toEqual({ name: "origin/feat/a", track: true });
  });
});

describe("filterRefs", () => {
  it("忽略大小写的子串匹配；空查询返回全部", () => {
    expect(filterRefs(["main", "Feature/X", "dev"], "fea")).toEqual(["Feature/X"]);
    expect(filterRefs(["main", "dev"], "  ")).toEqual(["main", "dev"]);
  });
});

describe("parseLog", () => {
  it("提交行拆出图形列与各字段；纯图形行只有 graph", () => {
    const out = [
      `* ${US}aaaa111${US}aaaa1${US}HEAD -> main, origin/main, tag: v1${US}张三${US}1700000000${US}合并 x | y`,
      "|\\  ",
      `| * ${US}bbbb222${US}bbbb2${US}${US}Li${US}1699990000${US}fix: a${US}b`,
      "|/  ",
      "",
    ].join("\n");
    expect(parseLog(out)).toEqual([
      { graph: "* ", commit: { hash: "aaaa111", short: "aaaa1", refs: ["HEAD -> main", "origin/main", "tag: v1"], author: "张三", time: 1700000000, subject: "合并 x | y" } },
      { graph: "|\\  " },
      // 标题里含分隔符时其余部分仍归入标题
      { graph: "| * ", commit: { hash: "bbbb222", short: "bbbb2", refs: [], author: "Li", time: 1699990000, subject: `fix: a${US}b` } },
      { graph: "|/  " },
    ]);
  });

  it("CRLF", () => {
    expect(parseLog(`* ${US}h${US}h${US}${US}a${US}1${US}s\r\n`)[0].commit?.subject).toBe("s");
  });
});

describe("parseDecor", () => {
  it("HEAD 指向、标签、普通引用", () => {
    expect(parseDecor("HEAD -> main")).toEqual({ name: "main", kind: "head" });
    expect(parseDecor("HEAD")).toEqual({ name: "HEAD", kind: "head" });
    expect(parseDecor("tag: v1.0")).toEqual({ name: "v1.0", kind: "tag" });
    expect(parseDecor("origin/main")).toEqual({ name: "origin/main", kind: "ref" });
  });
});

describe("graphRuns", () => {
  it("按列（每 2 字符一条 lane）分段，空白不着色", () => {
    expect(graphRuns("| * ")).toEqual([
      { text: "|", lane: 0 },
      { text: " ", lane: -1 },
      { text: "*", lane: 1 },
      { text: " ", lane: -1 },
    ]);
  });

  it("同一 lane 的相邻字符合并", () => {
    expect(graphRuns("|\\")).toEqual([{ text: "|\\", lane: 0 }]);
    expect(graphRuns("")).toEqual([]);
  });
});

describe("parseNumstat", () => {
  it("增删行数、二进制文件为 null、重命名路径原样保留", () => {
    const out = "3\t1\tsrc/a.ts\n-\t-\timg.png\n0\t0\tsrc/{old => new}.ts\r\n\n";
    expect(parseNumstat(out)).toEqual([
      { path: "src/a.ts", add: 3, del: 1 },
      { path: "img.png", add: null, del: null },
      { path: "src/{old => new}.ts", add: 0, del: 0 },
    ]);
  });
});

describe("classifyDiff", () => {
  const diff = [
    "diff --git a/x b/x",
    "index 1..2 100644",
    "--- a/x",
    "+++ b/x",
    "@@ -1,3 +1,3 @@ fn",
    " ctx",
    "-old",
    "--- removed line starting with --",
    "+new",
    "+++ added line starting with ++",
    "\\ No newline at end of file",
    "diff --git a/y b/y",
    "--- a/y",
  ].join("\n");

  it("文件头为 meta，hunk 内按首字符分类（--- / +++ 开头的改动行不误判为文件头；「\\ No newline」为 meta）", () => {
    expect(classifyDiff(diff, 100).lines.map((l) => l.kind)).toEqual([
      "meta", "meta", "meta", "meta", "hunk", "ctx", "del", "del", "add", "add", "meta", "meta", "meta",
    ]);
  });

  it("按 @@ 推算新旧行号：删行只有旧行号，增行只有新行号，上下文两者都有", () => {
    const nums = classifyDiff(diff, 100).lines.map((l) => [l.old, l.new]);
    expect(nums.slice(4, 11)).toEqual([
      [undefined, undefined],
      [1, 1],
      [2, undefined],
      [3, undefined],
      [undefined, 2],
      [undefined, 3],
      [undefined, undefined],
    ]);
    expect(nums[0]).toEqual([undefined, undefined]);
  });

  it("多个 hunk 各自从 @@ 的起始行号算起；省略行数（@@ -5 +7 @@）与新文件（-0,0）", () => {
    const text = ["@@ -10,2 +12,3 @@ ctx", " a", "+b", " c", "@@ -40 +43 @@", "-d", "+e", "@@ -0,0 +1,2 @@", "+x", "+y"].join("\n");
    expect(classifyDiff(text, 100).lines.map((l) => [l.old ?? "", l.new ?? ""].join(":"))).toEqual([
      ":", "10:12", ":13", "11:14", ":", "40:", ":43", ":", ":1", ":2",
    ]);
  });

  it("超过行数上限截断", () => {
    const r = classifyDiff(diff, 5);
    expect(r.lines).toHaveLength(5);
    expect(r.truncated).toBe(true);
    expect(classifyDiff(diff, 100).truncated).toBe(false);
  });

  it("空 diff；末尾换行不产生空行", () => {
    expect(classifyDiff("", 10).lines).toEqual([]);
    expect(classifyDiff("@@ -1 +1 @@\r\n+a\r\n", 10).lines).toEqual([
      { text: "@@ -1 +1 @@", kind: "hunk" },
      { text: "+a", kind: "add", new: 1 },
    ]);
  });
});

describe("relTime", () => {
  const now = Date.UTC(2026, 0, 31, 12);
  const ago = (s: number) => now / 1000 - s;
  it("刚刚 / 分钟 / 小时 / 天 / 月 / 年（按语言，Intl.RelativeTimeFormat）", () => {
    const rtf = new Intl.RelativeTimeFormat("zh-CN", { numeric: "auto" });
    expect(relTime(ago(10), now)).toBe("刚刚");
    expect(relTime(ago(5 * 60), now)).toBe(rtf.format(-5, "minute"));
    expect(relTime(ago(3 * 3600), now)).toBe(rtf.format(-3, "hour"));
    expect(relTime(ago(5 * 86400), now)).toBe(rtf.format(-5, "day"));
    expect(relTime(ago(65 * 86400), now)).toBe(rtf.format(-2, "month"));
    expect(relTime(ago(800 * 86400), now)).toBe(rtf.format(-2, "year"));
    expect(relTime(ago(5 * 60), now)).toMatch(/^5\s?分钟前$/);
  });
});

// 与 src-tauri 的 git_changes_kinds 用例同一个仓库状态：修改、删除、git mv 重命名、二进制修改、未跟踪
const Z = "\0";
const STATUS = [
  "# branch.oid 1234567890abcdef1234567890abcdef12345678",
  "# branch.head main",
  "# branch.upstream origin/main",
  "# branch.ab +2 -1",
  "1 .M N... 100644 100644 100644 aaa aaa bin.dat",
  "1 .D N... 100644 100644 000000 bbb bbb gone.txt",
  "1 .M N... 100644 100644 100644 ccc ccc keep.txt",
  "2 R. N... 100644 100644 100644 ddd ddd R100 new name.txt",
  "old.txt",
  "u UU N... 100644 100644 100644 100644 e1 e2 e3 merge me.txt",
  "? 新 文件.txt",
  "! ignored.log",
  "",
].join(Z);
const NUMSTAT = ["-\t-\tbin.dat", "0\t1\tgone.txt", "2\t1\tkeep.txt", "0\t0\t", "old.txt", "new name.txt", "3\t3\tmerge me.txt", ""].join(Z);

describe("parseStatusV2", () => {
  it("分支头与各类条目（路径可含空格、重命名带原路径、忽略 ! 条目）", () => {
    const st = parseStatusV2(STATUS);
    expect(st).toMatchObject({ branch: "main", oid: "1234567890abcdef1234567890abcdef12345678", upstream: "origin/main", ahead: 2, behind: 1 });
    expect(st.entries).toEqual([
      { path: "bin.dat", xy: ".M", kind: "modified" },
      { path: "gone.txt", xy: ".D", kind: "deleted" },
      { path: "keep.txt", xy: ".M", kind: "modified" },
      { path: "new name.txt", orig: "old.txt", xy: "R.", kind: "renamed" },
      { path: "merge me.txt", xy: "UU", kind: "conflict" },
      { path: "新 文件.txt", xy: "??", kind: "untracked" },
    ]);
  });

  it("空仓库与分离 HEAD；暂存的新文件为 added", () => {
    const st = parseStatusV2(["# branch.oid (initial)", "# branch.head main", "1 A. N... 000000 100644 100644 000 aaa a.txt", ""].join(Z));
    expect([st.oid, st.branch, st.upstream, st.ahead]).toEqual([null, "main", null, 0]);
    expect(st.entries[0].kind).toBe("added");
    expect(parseStatusV2(["# branch.oid abcdef0123", "# branch.head (detached)", ""].join(Z)).branch).toBeNull();
    expect(parseStatusV2("").entries).toEqual([]);
  });
});

describe("parseNumstatZ", () => {
  it("普通、二进制与重命名（原路径、新路径各占一项）", () => {
    expect(parseNumstatZ(NUMSTAT)).toEqual([
      { path: "bin.dat", add: null, del: null },
      { path: "gone.txt", add: 0, del: 1 },
      { path: "keep.txt", add: 2, del: 1 },
      { path: "new name.txt", orig: "old.txt", add: 0, del: 0 },
      { path: "merge me.txt", add: 3, del: 3 },
    ]);
    expect(parseNumstatZ("")).toEqual([]);
  });
});

describe("buildChangeSet", () => {
  it("文件以 status 为准，行数取 numstat；未跟踪只标新、不计行数；带最近提交", () => {
    const log = [["h1", "abc1234", "HEAD -> main", "me", "100", "second"].join(US), ["h0", "abc0000", "", "me", "50", "first"].join(US), ""].join("\n");
    const cs = buildChangeSet({ root: "/r", status: STATUS, numstat: NUMSTAT, log });
    expect(cs.files.map((f) => [f.path, f.kind, f.add, f.del])).toEqual([
      ["bin.dat", "modified", null, null],
      ["gone.txt", "deleted", 0, 1],
      ["keep.txt", "modified", 2, 1],
      ["new name.txt", "renamed", 0, 0],
      ["merge me.txt", "conflict", 3, 3],
      ["新 文件.txt", "untracked", null, null],
    ]);
    expect([cs.added, cs.deleted, cs.branch, cs.ahead, cs.behind]).toEqual([5, 5, "main", 2, 1]);
    expect(cs.commits.map((c) => [c.short, c.subject, c.refs])).toEqual([
      ["abc1234", "second", ["HEAD -> main"]],
      ["abc0000", "first", []],
    ]);
  });

  it("干净的仓库没有文件；分离 HEAD 时分支为提交前 7 位", () => {
    const cs = buildChangeSet({ root: "/r", status: ["# branch.oid 1234567890ab", "# branch.head (detached)", ""].join(Z), numstat: "", log: "" });
    expect([cs.files, cs.added, cs.branch, cs.commits]).toEqual([[], 0, "1234567", []]);
  });
});
