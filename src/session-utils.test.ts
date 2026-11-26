import { describe, expect, it } from "vitest";
import type { SessionInfo } from "./protocol";
import { displayName, nameTooltip } from "./session-utils";

const s = (extra: Partial<SessionInfo> = {}): SessionInfo => ({
  id: 1,
  name: "tether",
  cwd: "C:\\workspace\\tether",
  command: "pwsh.exe",
  status: "running",
  startedAt: 0,
  lastActivityAt: 0,
  host: null,
  ...extra,
});

describe("displayName", () => {
  it("没有对话标题时用原名", () => {
    expect(displayName(s())).toBe("tether");
    expect(displayName(s({ claudeTitle: "" }))).toBe("tether");
  });

  it("没手动改过名时用对话标题", () => {
    expect(displayName(s({ claudeTitle: "修复登录 bug" }))).toBe("修复登录 bug");
    expect(displayName(s({ nameCustom: false, claudeTitle: "修复登录 bug" }))).toBe("修复登录 bug");
  });

  it("手动改过的名字优先于对话标题", () => {
    expect(displayName(s({ name: "我的会话", nameCustom: true, claudeTitle: "修复登录 bug" }))).toBe("我的会话");
    expect(displayName(s({ name: "我的会话", nameCustom: true }))).toBe("我的会话");
  });
});

describe("nameTooltip", () => {
  it("显示对话标题时附上原名", () => {
    expect(nameTooltip(s())).toBe("tether");
    expect(nameTooltip(s({ claudeTitle: "修复登录" }))).toBe("修复登录\n原名：tether");
    expect(nameTooltip(s({ nameCustom: true, claudeTitle: "修复登录" }))).toBe("tether");
  });
});
