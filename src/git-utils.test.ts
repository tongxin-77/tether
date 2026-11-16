import { describe, expect, it } from "vitest";
import { classifyDiff, filterRefs, graphRuns, parseDecor, parseLog, parseNumstat, parseRefs, relTime, switchTarget } from "./git-utils";

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

  it("文件头为 meta，hunk 内按首字符分类（--- / +++ 开头的改动行不误判为文件头）", () => {
    expect(classifyDiff(diff, 100).lines.map((l) => l.kind)).toEqual([
      "meta", "meta", "meta", "meta", "hunk", "ctx", "del", "del", "add", "add", "ctx", "meta", "meta",
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
      { text: "+a", kind: "add" },
    ]);
  });
});

describe("relTime", () => {
  const now = Date.UTC(2026, 0, 31, 12);
  const ago = (s: number) => now / 1000 - s;
  it("刚刚 / 分钟 / 小时 / 天 / 月 / 年", () => {
    expect(relTime(ago(10), now)).toBe("刚刚");
    expect(relTime(ago(5 * 60), now)).toBe("5 分钟前");
    expect(relTime(ago(3 * 3600), now)).toBe("3 小时前");
    expect(relTime(ago(2 * 86400), now)).toBe("2 天前");
    expect(relTime(ago(65 * 86400), now)).toBe("2 个月前");
    expect(relTime(ago(800 * 86400), now)).toBe("2 年前");
  });
});
