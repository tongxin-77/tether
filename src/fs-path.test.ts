import { describe, expect, it } from "vitest";
import { resolveInput } from "./fs-path";

describe("resolveInput 本地（Windows）", () => {
  const base = "C:\\work\\proj";
  const home = "C:\\Users\\me";
  const r = (input: string, b = base) => resolveInput(input, b, home, false);

  it("盘符绝对路径，盘符大写，正反斜杠混用", () => {
    expect(r("d:/a\\b/c")).toBe("D:\\a\\b\\c");
    expect(r("C:\\Windows\\")).toBe("C:\\Windows");
    expect(r("c:")).toBe("C:\\");
    expect(r("C:/")).toBe("C:\\");
  });

  it("~ 与 ~ 开头", () => {
    expect(r("~")).toBe("C:\\Users\\me");
    expect(r("~/Desktop")).toBe("C:\\Users\\me\\Desktop");
    expect(r("~\\a\\..\\b")).toBe("C:\\Users\\me\\b");
    expect(resolveInput("~", base, null, false)).toBeNull();
    // ~foo 是普通相对名
    expect(r("~foo")).toBe("C:\\work\\proj\\~foo");
  });

  it("相对路径、. 与 ..", () => {
    expect(r("src")).toBe("C:\\work\\proj\\src");
    expect(r("./src/../docs")).toBe("C:\\work\\proj\\docs");
    expect(r("..")).toBe("C:\\work");
    expect(r("..\\..\\..\\..")).toBe("C:\\");
    expect(r("src", "D:\\")).toBe("D:\\src");
  });

  it("\\ 开头为当前盘符根", () => {
    expect(r("\\tmp")).toBe("C:\\tmp");
    expect(r("/tmp/x")).toBe("C:\\tmp\\x");
  });

  it("UNC", () => {
    expect(r("\\\\srv\\share\\a\\..\\b")).toBe("\\\\srv\\share\\b");
    expect(r("//srv/share")).toBe("\\\\srv\\share\\");
  });

  it("引号、空输入、盘符列表下的相对路径", () => {
    expect(r('"C:\\Program Files"')).toBe("C:\\Program Files");
    expect(r("   ")).toBeNull();
    expect(r("src", "")).toBeNull();
    expect(r("E:\\x", "")).toBe("E:\\x");
  });
});

describe("resolveInput 远程（POSIX）", () => {
  const base = "/home/me/proj";
  const r = (input: string) => resolveInput(input, base, "/home/me", true);

  it("绝对路径与规范化", () => {
    expect(r("/etc/nginx/")).toBe("/etc/nginx");
    expect(r("//var///log/./x/..")).toBe("/var/log");
    expect(r("/")).toBe("/");
    expect(r("/../..")).toBe("/");
  });

  it("~、相对路径、..", () => {
    expect(r("~")).toBe("/home/me");
    expect(r("~/.config")).toBe("/home/me/.config");
    expect(r("src/lib")).toBe("/home/me/proj/src/lib");
    expect(r("../..")).toBe("/home");
    expect(resolveInput("~/x", base, null, true)).toBeNull();
  });

  it("反斜杠不当作分隔符", () => {
    expect(r("a\\b")).toBe("/home/me/proj/a\\b");
  });
});
