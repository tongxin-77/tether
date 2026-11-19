import { afterEach, describe, expect, it, vi } from "vitest";
// @ts-expect-error 未安装 @types/node（vitest 在 node 中运行）
import { readFileSync } from "node:fs";
import { oscLinkTarget, terminalFontSize, clampTermFont, parseTermFont, parseFontAdj, parseGpu, fontAdjLabel, wheelSteps, FONT_ADJ_DEFAULT, FONT_ADJ_OPTIONS, branchDir, branchLabel, clampFilesWidth, completionEnabled, debounce, formatMemory, memoryHigh, modeResetSeq, needsRefresh, nextRootSeen, pastedPath, promptKind, shouldFit, takeoverStale, wasReleased, windowsPtyFor } from "./term-utils";

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
  it("4K 及以上（物理宽度 ≥ 3840）用 12", () => {
    expect(terminalFontSize(3840, 1)).toBe(12);
    expect(terminalFontSize(2560, 1.5)).toBe(12); // 4K 150% 缩放
    expect(terminalFontSize(1920, 2)).toBe(12); // 4K 200% 缩放
  });
  it("低于 4K 用更紧凑的 11", () => {
    expect(terminalFontSize(1920, 1)).toBe(11);
    expect(terminalFontSize(2560, 1)).toBe(11); // 2K
    expect(terminalFontSize(1536, 1.25)).toBe(11); // 1080p 125% 缩放
  });
});

describe("终端字号存储", () => {
  it("无值或非数字为自动（null）", () => {
    for (const v of [null, "", " ", "auto", "abc", "NaN", "Infinity"]) expect(parseTermFont(v), String(v)).toBeNull();
  });
  it("数字取整并钳制到 8–28", () => {
    expect(parseTermFont("14")).toBe(14);
    expect(parseTermFont("13.6")).toBe(14);
    expect(parseTermFont("3")).toBe(8);
    expect(parseTermFont("100")).toBe(28);
    expect(clampTermFont(7)).toBe(8);
    expect(clampTermFont(29)).toBe(28);
  });
});

describe("GPU 加速渲染开关", () => {
  it("无值或非 \"0\" 为开启（默认开启）", () => {
    for (const v of [null, "", "1", "true", "abc"]) expect(parseGpu(v), String(v)).toBe(true);
  });
  it("\"0\" 为关闭", () => {
    expect(parseGpu("0")).toBe(false);
  });
});

describe("界面字号偏移", () => {
  it("-4…+2 的整数原样解析", () => {
    for (const n of FONT_ADJ_OPTIONS) expect(parseFontAdj(String(n))).toBe(n);
  });
  it("无值或非法值为默认 -2", () => {
    expect(FONT_ADJ_DEFAULT).toBe(-2);
    for (const v of [null, "", "-5", "3", "1.5", "-0", "abc", " 1"]) expect(parseFontAdj(v), String(v)).toBe(-2);
  });
  it("选项文字", () => {
    expect(fontAdjLabel(-2)).toBe("小 2 号（默认）");
    expect(fontAdjLabel(-4)).toBe("小 4 号");
    expect(fontAdjLabel(0)).toBe("原始大小");
    expect(fontAdjLabel(1)).toBe("大 1 号");
  });
  // index.html 头部的防闪烁脚本（第二段）无法 import，这里直接执行，确保与 parseFontAdj 结论一致
  it("index.html 防闪烁脚本与 parseFontAdj 一致", () => {
    const html: string = readFileSync(new URL("../index.html", import.meta.url), "utf8");
    const script = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].find((m) => m[1].includes("tether.ui.fontAdj"))![1];
    for (const v of [null, "", "-5", "3", "-0", "abc", ...FONT_ADJ_OPTIONS.map(String)]) {
      const props: Record<string, string> = {};
      const style = { setProperty: (k: string, val: string) => (props[k] = val) };
      new Function("localStorage", "document", script)({ getItem: () => v }, { documentElement: { style } });
      expect(props["--font-adj"] ?? `${FONT_ADJ_DEFAULT}px`, String(v)).toBe(`${parseFontAdj(v)}px`);
    }
  });
});

describe("wheelSteps", () => {
  const run = (deltas: number[], mode = 0) => {
    let acc = 0;
    let total = 0;
    for (const d of deltas) {
      const r = wheelSteps(acc, d, mode);
      acc = r.acc;
      total += r.steps;
    }
    return total;
  };
  it("鼠标一格（100 / 120px）只调一号，上滚为正", () => {
    expect(run([-100])).toBe(1);
    expect(run([120])).toBe(-1);
    expect(run([-100, -100, -100])).toBe(3);
  });
  it("触控板小增量累计跨过阈值才调", () => {
    expect(run([-10, -10, -10, -10])).toBe(0);
    expect(run(Array(10).fill(-10))).toBe(2);
  });
  it("换向时清零累计", () => {
    expect(run([-40, 40])).toBe(0);
    expect(run([-40, 40, 20])).toBe(-1); // 不清零则累计为 20，不调
  });
  it("按行 / 页滚动时每个事件一号", () => {
    expect(run([-3, -3], 1)).toBe(2);
    expect(run([1], 2)).toBe(-1);
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

describe("formatMemory / memoryHigh", () => {
  it("不足 1 GB 按 MB 取整，否则按 GB 保留一位", () => {
    expect(formatMemory(0)).toBe("0 MB");
    expect(formatMemory(812 * 1024 ** 2 + 300_000)).toBe("812 MB");
    expect(formatMemory(1023.4 * 1024 ** 2)).toBe("1023 MB");
    expect(formatMemory(1023.6 * 1024 ** 2)).toBe("1.0 GB");
    expect(formatMemory(1.3 * 1024 ** 3)).toBe("1.3 GB");
  });
  it("超过 1 GB 才警示", () => {
    expect(memoryHigh(1024 ** 3)).toBe(false);
    expect(memoryHigh(1024 ** 3 + 1)).toBe(true);
    expect(memoryHigh(500 * 1024 ** 2)).toBe(false);
  });
});

describe("pastedPath", () => {
  it("含空格时加双引号，否则原样", () => {
    expect(pastedPath("C:\\Temp\\tether-img\\a.png")).toBe("C:\\Temp\\tether-img\\a.png");
    expect(pastedPath("C:\\Users\\A B\\tether-img\\a.png")).toBe('"C:\\Users\\A B\\tether-img\\a.png"');
  });
});

describe("promptKind", () => {
  it("以 # 结尾为 root", () => {
    expect(promptKind("root@srv:~# ")).toBe("root");
    expect(promptKind("[root@srv ~]#")).toBe("root");
    expect(promptKind("# ")).toBe("root");
  });
  it("以 $ / % / ❯ 结尾为普通用户", () => {
    expect(promptKind("me@srv:~$ ")).toBe("user");
    expect(promptKind("me@srv ~ % ")).toBe("user");
    expect(promptKind("~/p ❯ ")).toBe("user");
  });
  it("无法判断时为 null：已输入命令、续行提示、空行、多个空格、过长", () => {
    expect(promptKind("root@srv:~# ls")).toBeNull();
    expect(promptKind("> ")).toBeNull();
    expect(promptKind("")).toBeNull();
    expect(promptKind("abc #  ")).toBeNull();
    expect(promptKind("x".repeat(300) + "# ")).toBeNull();
  });
});

describe("nextRootSeen", () => {
  it("变为 root 时提示一次，之后不再提示", () => {
    expect(nextRootSeen(false, "root")).toEqual({ seen: true, hint: true });
    expect(nextRootSeen(true, "root")).toEqual({ seen: true, hint: false });
  });
  it("回到普通用户后再变 root 才再次提示；无法判断时保持", () => {
    expect(nextRootSeen(true, "user")).toEqual({ seen: false, hint: false });
    expect(nextRootSeen(true, null)).toEqual({ seen: true, hint: false });
    expect(nextRootSeen(false, null)).toEqual({ seen: false, hint: false });
  });
});
describe("modeResetSeq", () => {
  it("关闭鼠标跟踪、显示光标、关闭焦点上报、恢复 DECCKM，不动 bracketed paste", () => {
    const s = modeResetSeq("normal");
    for (const m of ["1000", "1002", "1003", "1005", "1006", "1015", "1004", "1"]) expect(s).toContain(`\x1b[?${m}l`);
    expect(s).toContain("\x1b[?25h");
    expect(s).not.toContain("2004");
  });
  it("仅备用缓冲时退出备用缓冲", () => {
    expect(modeResetSeq("normal")).not.toContain("1049");
    expect(modeResetSeq("alternate")).toContain("\x1b[?1049l");
  });
});
