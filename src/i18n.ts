// 界面国际化：词条在 src/locales（zh-CN 为源语言），t() 取当前语言文字，缺失时按 en → zh-CN 回退。
// 语言偏好存 localStorage（tether.ui.locale：auto 或语言代码），auto 跟随 navigator.language（WebView2 下即 Windows 显示语言）。
// 静态文字（index.html）用 data-i18n* 属性标注，applyDom 统一刷新；动态文字由各模块订阅 onLocaleChange 后重新渲染。
import zhCN from "./locales/zh-CN";
import en from "./locales/en";
import zhTW from "./locales/zh-TW";
import ja from "./locales/ja";

export type Locale = "zh-CN" | "en" | "zh-TW" | "ja";
export type LocalePref = Locale | "auto";
export type MessageKey = keyof typeof zhCN;
export type Dict = Record<string, string>;

export const LOCALES: readonly Locale[] = ["zh-CN", "en", "zh-TW", "ja"];
/** 语言下拉中的名称：各语言的自称，不随界面语言翻译 */
export const LOCALE_NAMES: Record<Locale, string> = { "zh-CN": "简体中文", en: "English", "zh-TW": "繁體中文", ja: "日本語" };
export const LOCALE_KEY = "tether.ui.locale";
export const DICTS: Record<Locale, Dict> = { "zh-CN": zhCN, en, "zh-TW": zhTW, ja };

/** 系统语言标签 → 支持的语言：同语种归并（繁体脚本 / 港澳台 → zh-TW，其余中文 → zh-CN），非中文、日文、英文一律 en */
export function matchLocale(tag: string | null | undefined): Locale {
  const t = (tag ?? "").toLowerCase();
  if (t === "zh" || t.startsWith("zh-")) {
    if (/^zh-hant\b/.test(t)) return "zh-TW";
    if (/^zh-hans\b/.test(t)) return "zh-CN";
    return /^zh-(tw|hk|mo)\b/.test(t) ? "zh-TW" : "zh-CN";
  }
  if (t === "ja" || t.startsWith("ja-")) return "ja";
  return "en";
}

/** 偏好（存储值）+ 系统语言 → 实际语言；存储值非法时按 auto */
export function resolveLocale(pref: string | null | undefined, systemTag: string | null | undefined): Locale {
  return LOCALES.includes(pref as Locale) ? (pref as Locale) : matchLocale(systemTag);
}

function readPref(): LocalePref {
  try {
    const v = localStorage.getItem(LOCALE_KEY);
    return LOCALES.includes(v as Locale) ? (v as Locale) : "auto";
  } catch {
    return "auto";
  }
}

const systemTag = () => (typeof navigator === "undefined" ? undefined : navigator.language);

let pref: LocalePref = readPref();
let current: Locale = resolveLocale(pref, systemTag());
const listeners = new Set<() => void>();

export const locale = () => current;
export const localePref = () => pref;

/** 改变偏好（设置面板），立即生效并通知订阅者；其它窗口经 storage 事件同步 */
export function setLocalePref(p: LocalePref) {
  try {
    if (p === "auto") localStorage.removeItem(LOCALE_KEY);
    else localStorage.setItem(LOCALE_KEY, p);
  } catch {}
  apply(p);
}

/** 直接设置语言（测试用），不写存储 */
export function setLocale(l: Locale) {
  apply(l);
}

function apply(p: LocalePref) {
  pref = p;
  const next = resolveLocale(p, systemTag());
  if (next === current) return;
  current = next;
  for (const f of listeners) f();
}

/** 订阅语言变化，返回取消函数 */
export function onLocaleChange(f: () => void): () => void {
  listeners.add(f);
  return () => listeners.delete(f);
}

if (typeof window !== "undefined") {
  window.addEventListener("storage", (ev) => {
    if (ev.key === LOCALE_KEY) apply(readPref());
  });
}

/** 取词条：当前语言 → en → zh-CN → 键名本身；{name} 替换为 vars 中的值 */
export function t(key: MessageKey, vars?: Record<string, string | number>): string {
  const s = DICTS[current][key] ?? en[key as string] ?? zhCN[key] ?? key;
  return vars ? s.replace(/\{(\w+)\}/g, (m, k: string) => (k in vars ? String(vars[k]) : m)) : s;
}

/** 复数词条：key 为不带后缀的基名，en 等有 `_one` 时按 Intl.PluralRules 选用，其余用 `_other`；vars 自动带 n */
export function tn(base: string, n: number, vars?: Record<string, string | number>): string {
  const one = `${base}_one`;
  const cat = new Intl.PluralRules(current).select(n);
  const key = (cat === "one" && DICTS[current][one] !== undefined ? one : `${base}_other`) as MessageKey;
  return t(key, { n: fmtNum(n), ...vars });
}

// ---------------- 格式化（按当前语言） ----------------

/** 数字；digits 给出时固定小数位 */
export function fmtNum(n: number, digits?: number): string {
  return new Intl.NumberFormat(current, digits === undefined ? undefined : { minimumFractionDigits: digits, maximumFractionDigits: digits }).format(n);
}

export const fmtDateTime = (ms: number) => new Date(ms).toLocaleString(current);
export const fmtDate = (ms: number) => new Date(ms).toLocaleDateString(current);

/** 相对时间：diffSec 为距今的秒数（过去为正），unit 自动选择；不足一分钟为“刚刚” */
export function fmtAgo(diffSec: number): string {
  const s = Math.max(0, diffSec);
  if (s < 60) return t("time.justNow");
  const rtf = new Intl.RelativeTimeFormat(current, { numeric: "auto" });
  if (s < 3600) return rtf.format(-Math.floor(s / 60), "minute");
  if (s < 86400) return rtf.format(-Math.floor(s / 3600), "hour");
  const days = Math.floor(s / 86400);
  if (days < 30) return rtf.format(-days, "day");
  if (days < 365) return rtf.format(-Math.floor(days / 30), "month");
  return rtf.format(-Math.floor(days / 365), "year");
}

/** 按天的相对时间（昨天 / N 天前），由调用方算好日历天数 */
export function fmtDaysAgo(days: number): string {
  return new Intl.RelativeTimeFormat(current, { numeric: "auto" }).format(-days, "day");
}

// ---------------- 静态 DOM ----------------

const ATTRS: [string, string][] = [
  ["data-i18n-title", "title"],
  ["data-i18n-placeholder", "placeholder"],
  ["data-i18n-aria-label", "aria-label"],
];

/** 刷新 root 下带 data-i18n（textContent）/ data-i18n-title / -placeholder / -aria-label 的元素，并同步 <html lang> */
export function applyDom(root: ParentNode = document) {
  for (const el of root.querySelectorAll<HTMLElement>("[data-i18n]")) el.textContent = t(el.dataset.i18n as MessageKey);
  for (const [data, attr] of ATTRS) {
    for (const el of root.querySelectorAll<HTMLElement>(`[${data}]`)) el.setAttribute(attr, t(el.getAttribute(data) as MessageKey));
  }
  if (root === document) document.documentElement.lang = current;
}
