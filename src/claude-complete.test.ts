import { describe, expect, it } from "vitest";
import { complete } from "./claude-complete";

const texts = (before: string, shell: Parameters<typeof complete>[1] = "powershell") =>
  complete(before, shell)?.map((c) => c.text) ?? [];

describe("complete", () => {
  it("内置常用命令按前缀匹配：git st", () => {
    const r = complete("git st", "powershell");
    expect(r?.map((c) => c.text)).toEqual(expect.arrayContaining(["git status", "git stash"]));
    expect(r?.every((c) => c.partial === "git st")).toBe(true);
  });

  it("claude 参数：claude --mo 给出 --model", () => {
    const r = complete("claude --mo", "posix");
    const model = r?.find((c) => c.text === "--model");
    expect(model).toMatchObject({ key: "flag:--model", partial: "--mo" });
  });

  it("claude 参数值：--model 之后补全模型别名", () => {
    expect(texts("claude --model s")).toEqual(["sonnet"]);
  });

  it("claude 子命令只匹配第一个参数", () => {
    expect(texts("claude mc")).toContain("mcp");
    expect(texts("claude -c mc")).not.toContain("mcp");
  });

  it("带提示符的 claude 命令仍能识别", () => {
    expect(texts("PS C:\\work> claude --mo")).toContain("--model");
  });

  it("已完整输入的项不再给出", () => {
    expect(texts("claude --model")).not.toContain("--model");
  });

  it("不区分大小写匹配命令", () => {
    expect(texts("GIT ST")).toContain("git status");
  });

  it("无匹配返回 null", () => {
    expect(complete("", "powershell")).toBeNull();
    expect(complete("zzz-not-a-command", "powershell")).toBeNull();
    expect(complete("claude ", "powershell")).toBeNull();
  });

  // 已知问题：completeCommand 从行首第一个非空白字符开始取命令，带提示符的行整行都被当作命令，返回空
  it.fails("已知问题：带提示符的行（PS C:\\work> git st）应补全 git status", () => {
    expect(texts("PS C:\\work> git st")).toContain("git status");
  });
});
