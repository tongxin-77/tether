// 主题注册表。界面颜色全部来自 styles.css 中按 data-theme 定义的语义变量，终端配色来自这里的 xterm 字段。
//
// 新增一个主题只需两步：
//   1. 在下面的 THEMES 中加一项 { id, label, dark, xterm }（id 即 <html data-theme> 的值）；
//   2. 在 styles.css 的“主题”段加一个 :root[data-theme="<id>"] { ... } 块，填全同一组语义变量（照抄现有块改值）。
// 设置面板的主题下拉框与侧栏切换按钮都从 getThemes() 生成 / 轮换，不需要改界面代码。
import type { ITheme, Terminal } from "@xterm/xterm";

export interface ThemeDef {
  id: string;
  /** 设置面板中显示的名称 */
  label: string;
  /** 是否暗色（color-scheme 由 CSS 块自己声明，这里供逻辑判断） */
  dark: boolean;
  /** 终端配色；background 为不透明时的底色，窗口半透明时由 xtermThemeFor 换成透明 */
  xterm: ITheme;
}

/** 暖棕深底终端的共用 ANSI 色（奶油 / 奶咖夜只是底色深浅不同） */
const WARM_ANSI: ITheme = {
  foreground: "#efe2cf",
  cursor: "#e3a867",
  selectionBackground: "rgba(227, 168, 103, 0.3)",
  black: "#5a4636",
  red: "#e59a86",
  green: "#b4c58f",
  yellow: "#f0c66b",
  blue: "#a9c2c9",
  magenta: "#cfa7cf",
  cyan: "#9cc4b2",
  white: "#d9c8b2",
  brightBlack: "#9c8670",
  brightRed: "#f0ad9b",
  brightGreen: "#c6d6a3",
  brightYellow: "#f7d488",
  brightBlue: "#bfd4da",
  brightMagenta: "#dcbadc",
  brightCyan: "#b3d6c7",
  brightWhite: "#fbf3e6",
  // 与 styles.css 的滚动条颜色一致
  scrollbarSliderBackground: "rgba(239, 226, 207, 0.15)",
  scrollbarSliderHoverBackground: "rgba(239, 226, 207, 0.28)",
  scrollbarSliderActiveBackground: "rgba(239, 226, 207, 0.42)",
};

/** 玻璃（docs/design/fd-03-glass.html）：烟色玻璃终端。注明“设计稿”的取自其 .term / .t-* 示例色，其余为按冷色调补齐 */
const GLASS_XTERM: ITheme = {
  background: "#606574", // 设计稿：--smoke 叠在壁纸上的合成色
  foreground: "#d8dee9", // 设计稿 .term
  cursor: "#d8dee9", // 设计稿 .cursor
  cursorAccent: "#606574",
  selectionBackground: "rgba(127, 180, 202, 0.3)", // 补齐：取 .t-ps 色
  black: "#353b4c", // 补齐
  red: "#ff8a8a", // 设计稿 .t-red
  green: "#9fd89a", // 设计稿 .t-grn
  yellow: "#f2cc8f", // 设计稿 .t-yel
  blue: "#7fb4ca", // 设计稿 .t-ps
  magenta: "#c9a7e8", // 设计稿 .t-box
  cyan: "#7fd1c7", // 设计稿 .t-cy
  white: "#d8dee9", // 设计稿 .term
  brightBlack: "#8a93a6", // 设计稿 .t-dim
  brightRed: "#ffa8a8", // 补齐
  brightGreen: "#b6e6b1", // 补齐
  brightYellow: "#f7dcaa", // 补齐
  brightBlue: "#a3cbe0", // 补齐
  brightMagenta: "#b8a9ff", // 设计稿 .t-vio
  brightCyan: "#a0e0d8", // 补齐
  brightWhite: "#f2f4f8", // 设计稿 .t-cmd
  // 设计稿 .term 滚动条
  scrollbarSliderBackground: "rgba(255, 255, 255, 0.15)",
  scrollbarSliderHoverBackground: "rgba(255, 255, 255, 0.28)",
  scrollbarSliderActiveBackground: "rgba(255, 255, 255, 0.42)",
};

const THEMES: ThemeDef[] = [
  { id: "cream", label: "奶油", dark: false, xterm: { ...WARM_ANSI, background: "#3a2d24", cursorAccent: "#3a2d24" } },
  { id: "cream-night", label: "奶咖夜", dark: true, xterm: { ...WARM_ANSI, background: "#2e231b", cursorAccent: "#2e231b" } },
  { id: "glass", label: "玻璃", dark: false, xterm: GLASS_XTERM },
];

const KEY = "tether.theme";
const DEFAULT = THEMES[0].id;
/** 已创建的终端：切换主题时整体替换其 theme */
const terms = new Set<Terminal>();
let current = load();
/** 窗口半透明时终端背景透明（由终端卡片提供半透明底色） */
let transparent = false;

export function getThemes(): readonly ThemeDef[] {
  return THEMES;
}

export function currentTheme(): ThemeDef {
  return THEMES.find((t) => t.id === current)!;
}

/** 切换主题：设置 data-theme、更新全部终端、记住选择 */
export function applyTheme(id: string) {
  if (!THEMES.some((t) => t.id === id)) id = DEFAULT;
  current = id;
  document.documentElement.dataset.theme = id;
  try {
    localStorage.setItem(KEY, id);
  } catch {}
  refreshTerminals();
}

/** 轮换到下一个主题（侧栏按钮） */
export function nextTheme() {
  const i = THEMES.findIndex((t) => t.id === current);
  applyTheme(THEMES[(i + 1) % THEMES.length].id);
}

/** 当前主题下的 xterm 配色 */
export function xtermTheme(): ITheme {
  const x = currentTheme().xterm;
  return transparent ? { ...x, background: "rgba(0, 0, 0, 0)" } : { ...x };
}

/** 登记终端（创建后调用，dispose 时 unregister） */
export function registerTerminal(t: Terminal) {
  terms.add(t);
}
export function unregisterTerminal(t: Terminal) {
  terms.delete(t);
}

/** 窗口透明度变化时调用 */
export function setTransparent(on: boolean) {
  if (on === transparent) return;
  transparent = on;
  refreshTerminals();
}

function refreshTerminals() {
  // 整体替换 theme 才会生效
  for (const t of terms) t.options.theme = xtermTheme();
}

function load() {
  let id: string | null = null;
  try {
    id = localStorage.getItem(KEY);
  } catch {}
  return THEMES.some((t) => t.id === id) ? id! : DEFAULT;
}

// 模块加载即应用（index.html 头部的内联脚本已提前设置过，避免首帧闪烁；这里兜底校验）
document.documentElement.dataset.theme = current;
// 其它窗口（拖出的独立窗口）切换主题时同步
window.addEventListener("storage", (ev) => {
  if (ev.key === KEY) {
    current = load();
    document.documentElement.dataset.theme = current;
    refreshTerminals();
  }
});
