import { describe, expect, it } from "vitest";
import { claudeMention, claudeMentions, dropText, relativePath } from "./claude-paths";

describe("relativePath", () => {
  it("本地：目录内返回正斜杠相对路径，不区分大小写", () => {
    expect(relativePath("C:\\work\\p\\src\\a.ts", "C:\\work\\p", false)).toBe("src/a.ts");
    expect(relativePath("c:\\Work\\P\\a.ts", "C:\\work\\p\\", false)).toBe("a.ts");
    expect(relativePath("C:\\a.ts", "C:\\", false)).toBe("a.ts");
  });
  it("本地：目录外、目录自身、前缀相同的兄弟目录为 null", () => {
    expect(relativePath("C:\\work\\q\\a.ts", "C:\\work\\p", false)).toBeNull();
    expect(relativePath("C:\\work\\p", "C:\\work\\p", false)).toBeNull();
    expect(relativePath("C:\\work\\p2\\a.ts", "C:\\work\\p", false)).toBeNull();
    expect(relativePath("D:\\work\\p\\a.ts", "C:\\work\\p", false)).toBeNull();
  });
  it("远程：区分大小写", () => {
    expect(relativePath("/home/u/p/a.ts", "/home/u/p", true)).toBe("a.ts");
    expect(relativePath("/home/u/P/a.ts", "/home/u/p", true)).toBeNull();
    expect(relativePath("/etc/x", "/", true)).toBe("etc/x");
  });
});

describe("claudeMention", () => {
  it("目录内用相对路径", () => {
    expect(claudeMention("C:\\work\\p\\src\\a.ts", "C:\\work\\p", false)).toBe("@src/a.ts");
    expect(claudeMention("/home/u/p/src", "/home/u/p", true)).toBe("@src");
  });
  it("目录外用绝对路径，本地换成正斜杠", () => {
    expect(claudeMention("D:\\data\\x.csv", "C:\\work\\p", false)).toBe("@D:/data/x.csv");
    expect(claudeMention("/etc/hosts", "/home/u", true)).toBe("@/etc/hosts");
  });
  it("含空白时用引号包裹", () => {
    expect(claudeMention("C:\\work\\p\\my docs\\a b.md", "C:\\work\\p", false)).toBe('@"my docs/a b.md"');
    expect(claudeMention("C:\\Program Files\\x", "C:\\work", false)).toBe('@"C:/Program Files/x"');
  });
});

describe("claudeMentions", () => {
  it("空格分隔、末尾一个空格", () => {
    expect(claudeMentions(["C:\\p\\a.ts", "C:\\p\\b c.ts"], "C:\\p", false)).toBe('@a.ts @"b c.ts" ');
  });
  it("空列表为空串", () => {
    expect(claudeMentions([], "C:\\p", false)).toBe("");
  });
});

describe("dropText", () => {
  it("Claude 运行中插 @ 引用", () => {
    expect(dropText(["C:\\p\\a.ts"], "C:\\p", true)).toBe("@a.ts ");
  });
  it("否则插引号包裹的原路径", () => {
    expect(dropText(["C:\\p\\a.ts", "D:\\x y\\b"], "C:\\p", false)).toBe('"C:\\p\\a.ts" "D:\\x y\\b" ');
  });
});
