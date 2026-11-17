import { afterEach, describe, expect, it, vi } from "vitest";
import { oscLinkTarget, terminalFontSize, branchDir, branchLabel, clampFilesWidth, completionEnabled, debounce, needsRefresh, shouldFit, takeoverStale, wasReleased, windowsPtyFor } from "./term-utils";

describe("completionEnabled", () => {
  it("无 Claude、普通 buffer 时启用", () => {
    expect(completionEnabled(undefined, "normal")).toBe(true);
    expect(completionEnabled(null, "normal")).toBe(true);
  });
  it("Claude 运行中时禁用", () => {
    expect(completionEnabled({ state: "idle", at: 0 }, "normal")).toBe(false);
  });
  it("全屏程序（alternate buffer）时禁用", () => {
    expect(completionEnabled(undefined, "alternate")).toBe(false);
    expect(completionEnabled({ state: "busy", at: 0 }, "alternate")).toBe(false);
  });
});

describe("windowsPtyFor", () => {
  it("本地会话且有 build 号时给出 conpty", () => {
    expect(windowsPtyFor(null, 26200)).toEqual({ backend: "conpty", buildNumber: 26200 });
  });
  it("远程会话为 undefined", () => {
    expect(windowsPtyFor("server-1", 26200)).toBeUndefined();
    expect(windowsPtyFor("", 26200)).toBeUndefined();
  });
  it("build 号未知（null / 0）时为 undefined", () => {
    expect(windowsPtyFor(null, null)).toBeUndefined();
    expect(windowsPtyFor(null, 0)).toBeUndefined();
  });
});

describe("clampFilesWidth", () => {
  it("范围内原样（取整）", () => {
    expect(clampFilesWidth(300, 1600)).toBe(300);
    expect(clampFilesWidth(300.6, 1600)).toBe(301);
  });
  it("下限 200", () => {
    expect(clampFilesWidth(50, 1600)).toBe(200);
    expect(clampFilesWidth(-100, 1600)).toBe(200);
  });
  it("上限 640", () => {
    expect(clampFilesWidth(1000, 2000)).toBe(640);
  });
  it("上限为窗口宽的一半", () => {
    expect(clampFilesWidth(600, 1000)).toBe(500);
    expect(clampFilesWidth(600, 999)).toBe(500); // 499.5 取整
  });
  it("窗口过窄时下限优先", () => {
    expect(clampFilesWidth(300, 300)).toBe(200);
  });
});

describe("shouldFit", () => {
  it("尺寸足够时 fit", () => {
    expect(shouldFit(800, 600)).toBe(true);
    expect(shouldFit(100, 50)).toBe(true);
  });
  it("宽或高低于阈值时跳过", () => {
    expect(shouldFit(99, 600)).toBe(false);
    expect(shouldFit(800, 49)).toBe(false);
    expect(shouldFit(0, 0)).toBe(false);
  });
});

describe("debounce", () => {
  afterEach(() => vi.useRealTimers());

  it("连续触发只在最后一次之后执行一次", () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    const d = debounce(fn, 120);
    d();
    vi.advanceTimersByTime(100);
    d();
    vi.advanceTimersByTime(100);
    d();
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(119);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("最后一次生效：执行时读到的是最新状态", () => {
    vi.useFakeTimers();
    let size = 0;
    const seen: number[] = [];
    const d = debounce(() => seen.push(size), 120);
    for (let i = 1; i <= 5; i++) {
      size = i;
      d();
      vi.advanceTimersByTime(50);
    }
    vi.advanceTimersByTime(120);
    expect(seen).toEqual([5]);
  });

  it("间隔超过延时后再次触发会再次执行", () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    const d = debounce(fn, 120);
    d();
    vi.advanceTimersByTime(120);
    d();
    vi.advanceTimersByTime(120);
    expect(fn).toHaveBeenCalledTimes(2);
  });

  it("不触发则不执行", () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    debounce(fn, 120);
    vi.advanceTimersByTime(1000);
    expect(fn).not.toHaveBeenCalled();
  });
});

describe("wasReleased", () => {
  it("接管 → 未接管时为 true", () => {
    expect(wasReleased(true, false)).toBe(true);
    expect(wasReleased(true, undefined)).toBe(true);
  });
  it("持续接管或持续未接管时为 false", () => {
    expect(wasReleased(true, true)).toBe(false);
    expect(wasReleased(undefined, undefined)).toBe(false);
    expect(wasReleased(false, false)).toBe(false);
  });
  it("开始接管时为 false", () => {
    expect(wasReleased(undefined, true)).toBe(false);
  });
});

describe("takeoverStale / needsRefresh", () => {
  it("从未接管：不标记、不刷新", () => {
    expect(takeoverStale(false, undefined)).toBe(false);
    expect(takeoverStale(false, false)).toBe(false);
    expect(needsRefresh(false, undefined)).toBe(false);
  });
  it("接管中：标记，但不刷新", () => {
    expect(takeoverStale(false, true)).toBe(true);
    expect(needsRefresh(true, true)).toBe(false);
  });
  it("接管结束未刷新：保持标记，需要刷新", () => {
    expect(takeoverStale(true, false)).toBe(true);
    expect(takeoverStale(true, undefined)).toBe(true);
    expect(needsRefresh(true, false)).toBe(true);
    expect(needsRefresh(true, undefined)).toBe(true);
  });
  it("刷新后（标记清除）再切换：不再刷新", () => {
    expect(needsRefresh(false, false)).toBe(false);
    expect(takeoverStale(false, false)).toBe(false);
  });
});

describe("branchDir", () => {
  it("本地会话查启动目录", () => {
    expect(branchDir(null, "C:\workspace\tether")).toBe("C:\workspace\tether");
  });
  it("远程会话或目录为空时不查", () => {
    expect(branchDir("server-1", "/home/u")).toBeNull();
    expect(branchDir(null, "")).toBeNull();
  });
});

describe("branchLabel", () => {
  it("有分支时带 ⎇ 前缀，保留斜杠", () => {
    expect(branchLabel("main")).toBe("⎇ main");
    expect(branchLabel("feature/x")).toBe("⎇ feature/x");
  });
  it("无分支时为空串", () => {
    expect(branchLabel(null)).toBe("");
    expect(branchLabel(undefined)).toBe("");
    expect(branchLabel("")).toBe("");
  });
});

describe("terminalFontSize", () => {
  it("4K 及以上（物理宽度 ≥ 3840）用 14", () => {
    expect(terminalFontSize(3840, 1)).toBe(14);
    expect(terminalFontSize(2560, 1.5)).toBe(14); // 4K 150% 缩放
    expect(terminalFontSize(1920, 2)).toBe(14); // 4K 200% 缩放
  });
  it("低于 4K 用更紧凑的 13", () => {
    expect(terminalFontSize(1920, 1)).toBe(13);
    expect(terminalFontSize(2560, 1)).toBe(13); // 2K
    expect(terminalFontSize(1536, 1.25)).toBe(13); // 1080p 125% 缩放
  });
});

describe("oscLinkTarget", () => {
  it("http(s) 链接交给浏览器", () => {
    expect(oscLinkTarget("https://example.com/a?b=1")).toEqual({ url: "https://example.com/a?b=1" });
    expect(oscLinkTarget("http://localhost:3000")).toEqual({ url: "http://localhost:3000" });
  });
  it("file:// 转为本机路径", () => {
    expect(oscLinkTarget("file:///C:/work/a%20b/x.ts")).toEqual({ path: "C:\\work\\a b\\x.ts" });
    expect(oscLinkTarget("file://localhost/C:/x.ts")).toEqual({ path: "C:\\x.ts" });
  });
  it("其它协议忽略", () => {
    expect(oscLinkTarget("javascript:alert(1)")).toBeUndefined();
    expect(oscLinkTarget("not a url")).toBeUndefined();
  });
});
