import { afterEach, describe, expect, it } from "vitest";
// @ts-expect-error 未安装 @types/node（vitest 在 node 中运行）
import { readFileSync, readdirSync } from "node:fs";
import { DICTS, LOCALES, fmtAgo, matchLocale, resolveLocale, setLocale, t, tn } from "./i18n";
import zhCN from "./locales/zh-CN";

const placeholders = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
/** 英文等可另有 _one 复数形式（须有对应 _other），其余键集合须与 zh-CN 完全一致 */
const baseKeys = (d: Record<string, string>) => Object.keys(d).filter((k) => !(k.endsWith("_one") && `${k.slice(0, -4)}_other` in d));

describe("词条一致性", () => {
  const source = Object.keys(zhCN).sort();
  for (const l of LOCALES.filter((l) => l !== "zh-CN")) {
    it(`${l} 的键集合与 zh-CN 一致（缺键 / 多键都报错）`, () => {
      const d = DICTS[l];
      expect(source.filter((k) => !(k in d)), "缺少的键").toEqual([]);
      expect(baseKeys(d).filter((k) => !(k in zhCN)), "多余的键").toEqual([]);
      expect(Object.keys(d).filter((k) => k.endsWith("_one") && !(`${k.slice(0, -4)}_other` in zhCN)), "无对应 _other 的 _one").toEqual([]);
    });
    it(`${l} 的插值占位符与 zh-CN 一致`, () => {
      const d = DICTS[l];
      const diff = Object.entries(d)
        .map(([k, v]) => [k, placeholders(v), placeholders((zhCN as Record<string, string>)[k.replace(/_one$/, "_other")] ?? "")] as const)
        // _one 可省略 {n}（如 "1 item" 写成 "one item"）
        .filter(([k, a, b]) => (k.endsWith("_one") ? a.some((p) => !b.includes(p)) : a.join() !== b.join()));
      expect(diff).toEqual([]);
    });
    it(`${l} 没有空词条`, () => {
      expect(Object.entries(DICTS[l]).filter(([, v]) => !v.trim())).toEqual([]);
    });
  }

  it("界面引用的词条都存在：index.html 与组件模板的 data-i18n*、claude-complete.ts 的内置说明", () => {
    const sources = [new URL("../index.html", import.meta.url), ...["./", "./ui/", "./components/"].flatMap((d) => (readdirSync(new URL(d, import.meta.url)) as string[]).filter((f) => f.endsWith(".ts") && !f.endsWith(".test.ts")).map((f) => new URL(d + f, import.meta.url)))];
    const html = sources.map((u) => readFileSync(u, "utf8") as string).join("\n");
    const keys = [...html.matchAll(/data-i18n(?:-[a-z-]+)?="([^"]+)"/g)].map((m) => m[1]);
    const cc: string = readFileSync(new URL("./claude-complete.ts", import.meta.url), "utf8");
    keys.push(...[...cc.matchAll(/"(complete\.[\w.]+)"/g)].map((m) => m[1]));
    expect(keys.length).toBeGreaterThan(100);
    expect(keys.filter((k) => !(k in zhCN))).toEqual([]);
  });

  it("每种语言都有词条文件", () => {
    const files: string[] = readdirSync(new URL("./locales", import.meta.url));
    expect(LOCALES.map((l) => `${l}.ts`).filter((f) => !files.includes(f))).toEqual([]);
  });
});

describe("语言解析（回退链）", () => {
  it("精确匹配四种语言", () => {
    expect(["zh-CN", "en", "zh-TW", "ja"].map(matchLocale)).toEqual(["zh-CN", "en", "zh-TW", "ja"]);
  });
  it("同语种归并", () => {
    expect(["zh-HK", "zh-MO", "zh-Hant", "zh-Hant-TW", "zh-hant-hk"].map(matchLocale)).toEqual(Array(5).fill("zh-TW"));
    expect(["zh", "zh-SG", "zh-Hans", "zh-Hans-HK", "ZH-cn"].map(matchLocale)).toEqual(Array(5).fill("zh-CN"));
    expect(["ja-JP", "ja"].map(matchLocale)).toEqual(["ja", "ja"]);
    expect(["en-US", "en-GB"].map(matchLocale)).toEqual(["en", "en"]);
  });
  it("非中文 / 日文系统一律 en；空值也为 en", () => {
    expect(["fr-FR", "de", "ko-KR", "", undefined, null].map(matchLocale)).toEqual(Array(6).fill("en"));
  });
  it("偏好为具体语言时优先，auto 或非法值跟随系统", () => {
    expect(resolveLocale("ja", "zh-CN")).toBe("ja");
    expect(resolveLocale("auto", "zh-TW")).toBe("zh-TW");
    expect(resolveLocale(null, "en-US")).toBe("en");
    expect(resolveLocale("xx", "zh-HK")).toBe("zh-TW");
  });
});

describe("t / tn", () => {
  afterEach(() => setLocale("zh-CN"));
  it("插值", () => {
    expect(t("git.switchTo", { name: "main" })).toBe("切换到 main");
  });
  it("缺失词条按 en → zh-CN → 键名回退", () => {
    setLocale("ja");
    DICTS.en["__test.onlyEn"] = "EN";
    DICTS["zh-CN"]["__test.onlyZh"] = "中";
    try {
      expect(t("__test.onlyEn" as never)).toBe("EN");
      expect(t("__test.onlyZh" as never)).toBe("中");
      expect(t("__test.missing" as never)).toBe("__test.missing");
    } finally {
      delete DICTS.en["__test.onlyEn"];
      delete DICTS["zh-CN"]["__test.onlyZh"];
    }
  });
  it("复数：英文 1 用 _one，其余 _other；中文只有 _other", () => {
    setLocale("en");
    if (DICTS.en["git.limit_one"]) expect(tn("git.limit", 1)).toBe(t("git.limit_one" as never, { n: 1 }));
    expect(tn("git.limit", 200)).toBe(t("git.limit_other", { n: 200 }));
    setLocale("zh-CN");
    expect(tn("git.limit", 1)).toBe("仅显示最近 1 个提交");
  });
  it("相对时间按语言（Intl.RelativeTimeFormat）", () => {
    expect(fmtAgo(10)).toBe("刚刚");
    expect(fmtAgo(5 * 60)).toBe(new Intl.RelativeTimeFormat("zh-CN", { numeric: "auto" }).format(-5, "minute"));
    setLocale("en");
    expect(fmtAgo(3 * 3600)).toBe("3 hours ago");
    expect(fmtAgo(86400)).toBe("yesterday");
  });
});
