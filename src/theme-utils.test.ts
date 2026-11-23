import { describe, expect, it } from "vitest";
// @ts-expect-error 未安装 @types/node（vitest 在 node 中运行）；CSS 的 ?raw 导入在 vitest 中会被置空，故直接读文件
import { readFileSync } from "node:fs";
import { DEFAULT_THEME, parseTheme, STYLES, themeFor, THEMES, toggledTheme, xtermThemeFor } from "./theme-utils";
import { type MessageKey, t } from "./i18n";

const css: string = readFileSync(new URL("./styles.css", import.meta.url), "utf8");
const html: string = readFileSync(new URL("../index.html", import.meta.url), "utf8");

describe("主题注册表", () => {
  it("6 个风格，顺序与名称固定", () => {
    expect(STYLES.map((s) => t(`theme.${s.id}` as MessageKey))).toEqual(["奶油风1", "极简", "奶油风2", "毛玻璃", "北欧莫兰迪", "现代暗色"]);
  });
  it("每个风格恰有亮 / 暗两个主题，id 为 <风格>-light / <风格>-dark，dark 字段正确", () => {
    expect(THEMES).toHaveLength(12);
    for (const s of STYLES) {
      const ts = THEMES.filter((t) => t.style === s.id);
      expect(ts.map((t) => [t.id, t.dark])).toEqual([
        [`${s.id}-light`, false],
        [`${s.id}-dark`, true],
      ]);
    }
  });
  it("主题 id 唯一", () => {
    expect(new Set(THEMES.map((t) => t.id)).size).toBe(THEMES.length);
  });
  it("默认为奶油亮色", () => {
    expect(DEFAULT_THEME).toBe("cream-light");
  });
  it("xterm 配色完整：不透明背景、16 色 ANSI、光标、选区、滚动条", () => {
    const keys = ["foreground", "cursor", "cursorAccent", "selectionBackground", "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
    const bright = ["Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White"].map((c) => `bright${c}`);
    const sb = ["scrollbarSliderBackground", "scrollbarSliderHoverBackground", "scrollbarSliderActiveBackground"];
    for (const t of THEMES) {
      expect(t.xterm.background, t.id).toMatch(/^#[0-9a-f]{6}$/i);
      expect(t.xterm.cursorAccent, t.id).toBe(t.xterm.background);
      for (const k of [...keys, ...bright, ...sb]) expect(t.xterm[k as keyof typeof t.xterm], `${t.id}.${k}`).toBeTruthy();
    }
  });
});

describe("themeFor", () => {
  it("由（风格, 明暗）得到主题", () => {
    expect(themeFor("minimal", true).id).toBe("minimal-dark");
    expect(themeFor("cream", false).id).toBe("cream-light");
    expect(themeFor("modern", false).id).toBe("modern-light");
  });
  it("未知风格回退到默认", () => {
    expect(themeFor("nope", true).id).toBe(DEFAULT_THEME);
  });
});

describe("toggledTheme", () => {
  it("同一风格内亮 ↔ 暗", () => {
    for (const t of THEMES) {
      const o = toggledTheme(t.id);
      expect(o.style).toBe(t.style);
      expect(o.dark).toBe(!t.dark);
      expect(toggledTheme(o.id).id).toBe(t.id);
    }
  });
  it("未知 id 视为默认主题再切换", () => {
    expect(toggledTheme("bogus").id).toBe("cream-dark");
  });
});

describe("parseTheme（读取存储值）", () => {
  it("新格式原样识别", () => {
    for (const t of THEMES) expect(parseTheme(t.id).id).toBe(t.id);
  });
  it("旧值迁移", () => {
    expect(parseTheme("cream").id).toBe("cream-light");
    expect(parseTheme("cream-night").id).toBe("cream-dark");
    expect(parseTheme("glass").id).toBe("glass-light");
  });
  it("空值 / 未知值回退到奶油亮色", () => {
    for (const v of [null, "", "bogus", "constructor", "glass-night", "cream-light "]) expect(parseTheme(v).id).toBe("cream-light");
  });
});

// index.html 头部的防闪烁内联脚本无法 import，这里直接执行它，确保与 parseTheme 结论一致
describe("index.html 防闪烁脚本", () => {
  const script = html.match(/<script>([\s\S]*?)<\/script>/)![1];
  const initial = html.match(/<html[^>]*data-theme="([^"]+)"/)![1];
  const run = (stored: string | null) => {
    const dataset: Record<string, string> = { theme: initial };
    new Function("localStorage", "document", script)({ getItem: () => stored }, { documentElement: { dataset } });
    return dataset.theme;
  };
  it("html 默认 data-theme 为默认主题", () => {
    expect(initial).toBe(DEFAULT_THEME);
  });
  it("各种存储值得到与 parseTheme 相同的主题", () => {
    for (const v of [null, "", "bogus", "constructor", "cream", "cream-night", "glass", "glass-night", ...THEMES.map((t) => t.id)]) {
      expect(run(v), String(v)).toBe(parseTheme(v).id);
    }
  });
});

describe("styles.css 主题块完整性", () => {
  // 取出每个 :root[data-theme="<id>"] { ... } 块内声明的属性名（自定义属性与 color-scheme）
  const blocks = new Map<string, Set<string>>();
  for (const m of css.matchAll(/:root\[data-theme="([^"]+)"\]\s*\{([^}]*)\}/g)) {
    const props = new Set([...m[2].replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/(--[\w-]+|color-scheme)\s*:/g)].map((p) => p[1]));
    blocks.set(m[1], props);
  }
  it("主题块与注册表一一对应", () => {
    expect([...blocks.keys()].sort()).toEqual(THEMES.map((t) => t.id).sort());
  });
  it("每个主题块都包含 cream-light 块的全部变量", () => {
    const base = blocks.get("cream-light")!;
    expect(base.size).toBeGreaterThan(40);
    for (const t of THEMES) {
      const missing = [...base].filter((p) => !blocks.get(t.id)?.has(p));
      expect(missing, t.id).toEqual([]);
    }
  });
  it("color-scheme 与 dark 字段一致", () => {
    for (const t of THEMES) {
      const m = css.match(new RegExp(`:root\\[data-theme="${t.id}"\\]\\s*\\{[^}]*?color-scheme:\\s*(\\w+)`));
      expect(m?.[1], t.id).toBe(t.dark ? "dark" : "light");
    }
  });
});

describe("xtermThemeFor", () => {
  const byId = (id: string) => THEMES.find((t) => t.id === id)!;
  it("毛玻璃亮 / 暗：终端背景始终透明，透出卡片的烟色玻璃（不与卡片边缘形成色差）", () => {
    for (const id of ["glass-light", "glass-dark"]) expect(xtermThemeFor(byId(id), false).background).toBe("rgba(0, 0, 0, 0)");
  });
  it("其它主题：不透明窗口用注册的底色，半透明窗口透明", () => {
    const cream = byId("cream-light");
    expect(xtermThemeFor(cream, false).background).toBe(cream.xterm.background);
    expect(xtermThemeFor(cream, true).background).toBe("rgba(0, 0, 0, 0)");
  });
});
