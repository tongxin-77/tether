import { describe, expect, it } from "vitest";
import { marked } from "marked";
import { MAX_EDIT, chunkLines, editBlock, editorLang, groupBlocks, htmlDepth, previewKind } from "./preview-utils";
import { yamlProblems } from "./yaml-check";

describe("previewKind", () => {
  it("md / markdown 按 Markdown，常用文本 / 配置文件按文本编辑，大小写不敏感", () => {
    expect(previewKind("a/README.md")).toBe("md");
    expect(previewKind("C:\\x\\Doc.MARKDOWN")).toBe("md");
    for (const p of ["/var/log/app.log", "notes.TXT", "a.yaml", "b.YML", "x.json", "x.jsonc", "app.ini", "nginx.conf", "setup.cfg", "a.properties", "Cargo.toml", "pom.xml", "run.sh", "x.bash", "a.ps1", "b.bat", "c.cmd", "data.csv"]) {
      expect(previewKind(p), p).toBe("text");
    }
  });
  it("按文件名识别的配置文件", () => {
    for (const p of [".gitignore", "/r/.gitattributes", ".gitmodules", ".dockerignore", ".editorconfig", ".npmrc", ".yarnrc", ".nvmrc", ".prettierrc", ".eslintrc", ".env", ".env.local", ".env.production", "Dockerfile", "C:\\p\\Makefile"]) {
      expect(previewKind(p), p).toBe("text");
    }
  });
  it("模板副本按文本打开，去掉后缀后是 Markdown 的仍按 Markdown", () => {
    expect(previewKind("env.example")).toBe("text");
    expect(previewKind(".env.example")).toBe("text");
    expect(previewKind("config.yaml.example")).toBe("text");
    expect(previewKind("a.bin.sample")).toBe("text");
    expect(previewKind("x.template")).toBe("text");
    expect(previewKind("README.md.template")).toBe("md");
  });
  it("其它类型不预览", () => {
    expect(previewKind("a.html")).toBeNull();
    expect(previewKind("a.txt.gz")).toBeNull();
    expect(previewKind("md")).toBeNull();
    expect(previewKind("a.exe")).toBeNull();
    expect(previewKind("environment")).toBeNull();
    expect(previewKind("a.examples")).toBeNull();
  });
});

describe("editorLang", () => {
  it("按扩展名选高亮，模板副本按真实类型", () => {
    expect(editorLang("a.md")).toBe("markdown");
    expect(editorLang("a.yml")).toBe("yaml");
    expect(editorLang("config.yaml.example")).toBe("yaml");
    expect(editorLang("tsconfig.JSON")).toBe("json");
    expect(editorLang("x.jsonc.sample")).toBe("json");
    expect(editorLang("pyproject.toml")).toBe("toml");
    expect(editorLang("run.sh")).toBe("shell");
    expect(editorLang("app.ini")).toBe("properties");
    expect(editorLang(".env")).toBe("properties");
    expect(editorLang(".env.local")).toBe("properties");
    expect(editorLang("env.example")).toBe("properties");
    expect(editorLang(".editorconfig")).toBe("properties");
  });
  it("没有语言包的按纯文本", () => {
    for (const p of ["a.txt", "a.log", "pom.xml", "Dockerfile", ".gitignore", "a.ps1", "x.csv", "a.bin.example"]) {
      expect(editorLang(p), p).toBe("plain");
    }
  });
});

describe("editBlock", () => {
  it("超过上限或编码无法识别时只读", () => {
    expect(editBlock(MAX_EDIT, "utf8")).toBeUndefined();
    expect(editBlock(0, "ansi")).toBeUndefined();
    expect(editBlock(MAX_EDIT + 1, "utf8")).toContain("5 MB");
    expect(editBlock(10, null)).toContain("无法识别");
  });
});

describe("yamlProblems", () => {
  it("合法 YAML（含多文档、空文本）无错误", () => {
    expect(yamlProblems("")).toEqual([]);
    expect(yamlProblems("a: 1\nb:\n  - x\n---\nc: 中文\n")).toEqual([]);
  });
  it("报告错误的行号与位置", () => {
    const text = "a: 1\nb: [1, 2\nc: 3\n";
    const [p] = yamlProblems(text);
    expect(p.line).toBeGreaterThanOrEqual(2);
    expect(p.from).toBeLessThanOrEqual(text.length);
    expect(p.message).not.toContain("\n");
  });
  it("第二个文档中的错误也能找到，结果按位置排序", () => {
    const text = "a: 1\n---\nb: 1\n  c: 2\n";
    const ps = yamlProblems(text);
    expect(ps.length).toBeGreaterThan(0);
    expect(ps[0].line).toBeGreaterThanOrEqual(3);
    expect(ps.map((p) => p.from)).toEqual([...ps.map((p) => p.from)].sort((a, b) => a - b));
  });
  it("重复键算错误", () => {
    expect(yamlProblems("a: 1\na: 2\n")[0]?.line).toBe(2);
  });
});

describe("chunkLines", () => {
  it("按行数分块，兼容 CRLF，末尾换行不产生空行", () => {
    const text = Array.from({ length: 5 }, (_, i) => `l${i}`).join("\r\n") + "\r\n";
    expect(chunkLines(text, 2)).toEqual([
      { text: "l0\nl1", lines: 2 },
      { text: "l2\nl3", lines: 2 },
      { text: "l4", lines: 1 },
    ]);
  });
  it("保留中间与结尾的空行", () => {
    expect(chunkLines("a\n\nb\n\n", 2)).toEqual([
      { text: "a\n", lines: 2 },
      { text: "b\n", lines: 2 },
    ]);
  });
  it("空文本为一个空块", () => {
    expect(chunkLines("")).toEqual([{ text: "", lines: 1 }]);
  });
  it("按字符数分块", () => {
    expect(chunkLines("aaa\nbbb\nccc", 100, 8).map((c) => c.text)).toEqual(["aaa\nbbb", "ccc"]);
  });
  it("超长单行切开，不拆代理对，拼回与原文一致", () => {
    const line = "x".repeat(4) + "😀" + "y".repeat(5);
    const chunks = chunkLines(`a\n${line}\nb`, 100, 5);
    expect(chunks.map((c) => c.text)).toEqual(["a", "xxxx", "😀yyy", "yy\nb"]);
    expect(chunks.every((c) => !/[\ud800-\udbff]$/.test(c.text))).toBe(true);
  });
});

describe("htmlDepth", () => {
  it("开闭标签计数，忽略空元素、自闭合与注释", () => {
    expect(htmlDepth("<details>\n<summary>x</summary>")).toBe(1);
    expect(htmlDepth("</details>")).toBe(-1);
    expect(htmlDepth("<div><br><img src=a /><!-- c --></div>")).toBe(0);
  });
});

describe("groupBlocks", () => {
  const tok = (raw: string, type = "paragraph") => ({ type, raw });
  it("累计长度达到上限后切组", () => {
    const groups = groupBlocks([tok("aaaa"), tok("bbbb"), tok("cc"), tok("dddd")], 6);
    expect(groups.map((g) => g.map((t) => t.raw).join(""))).toEqual(["aaaabbbb", "ccdddd"]);
  });
  it("块级 HTML 未闭合时不切开", () => {
    const groups = groupBlocks([tok("<details>", "html"), tok("aaaaaaaa"), tok("bbbbbbbb"), tok("</details>", "html"), tok("c")], 6);
    expect(groups.map((g) => g.length)).toEqual([4, 1]);
  });
  it("分组渲染与整体渲染结果一致（含引用式链接）", () => {
    const md = "# T\n\n[x][r]\n\n- a\n- b\n\n```\ncode\n```\n\n| a |\n|---|\n| 1 |\n\n[r]: https://e.com\n";
    const whole = marked.parse(md, { async: false, gfm: true });
    const parts = groupBlocks(marked.lexer(md, { gfm: true }), 1).map((g) => marked.parser(g, { gfm: true }));
    expect(parts.length).toBeGreaterThan(3);
    expect(parts.join("")).toBe(whole);
  });
});
