// 主题注册表与纯逻辑（无副作用，便于单元测试）；DOM / 存储 / 终端刷新在 themes.ts。
//
// 主题 = 风格 × 明暗：6 个风格各有亮 / 暗两个主题，id 统一为 "<风格>-light" / "<风格>-dark"（即 <html data-theme> 的值）。
// 新增一个风格：
//   1. 在 STYLES 加一项，在 THEMES 加 light / dark 两项（xterm 为终端配色，background 为不透明时的底色）；
//   2. 在 styles.css 的“主题”段加两个 :root[data-theme="<id>"] 块，填全同一组语义变量（theme-utils.test.ts 会检查）；
//   3. index.html 防闪烁脚本里的风格列表同步加上（同样有测试检查）。
import type { ITheme } from "@xterm/xterm";

export interface StyleDef {
  /** 设置面板中显示的名称为词条 theme.<id> */
  id: string;
}

export interface ThemeDef {
  id: string;
  style: string;
  /** 是否暗色（color-scheme 由 CSS 块自己声明，这里供逻辑判断） */
  dark: boolean;
  /** 终端配色；background 为不透明时的底色，窗口半透明时由 themes.ts 换成透明 */
  xterm: ITheme;
  /** 终端背景始终透明（毛玻璃：由卡片的烟色玻璃提供底色，避免不透明底色与卡片边缘的壁纸合成色不一致） */
  glassTerm?: boolean;
}

/** 主题的实际 xterm 配色：窗口半透明或主题要求玻璃终端时背景透明 */
export function xtermThemeFor(def: ThemeDef, windowTransparent: boolean): ITheme {
  return windowTransparent || def.glassTerm ? { ...def.xterm, background: "rgba(0, 0, 0, 0)" } : { ...def.xterm };
}

export const STYLES: readonly StyleDef[] = [
  { id: "cream" },
  { id: "minimal" }, // docs/design/fd-01-minimal.html
  { id: "milk" }, // docs/design/fd-02-cream.html
  { id: "glass" }, // docs/design/fd-03-glass.html
  { id: "morandi" }, // docs/design/fd-04-morandi.html
  { id: "modern" }, // docs/design/fd-05-dark.html
];

/** 滚动条三色：同一颜色的三档透明度 */
const sb = (rgb: string, a = [0.15, 0.28, 0.42]): ITheme => ({
  scrollbarSliderBackground: `rgba(${rgb}, ${a[0]})`,
  scrollbarSliderHoverBackground: `rgba(${rgb}, ${a[1]})`,
  scrollbarSliderActiveBackground: `rgba(${rgb}, ${a[2]})`,
});

/** 以 ANSI 色为底，补上背景 / 光标底色（cursorAccent 与背景一致，块光标下的字符可见） */
const on = (ansi: ITheme, background: string, extra: ITheme = {}): ITheme => ({ ...ansi, ...extra, background, cursorAccent: background });

/** 奶油：暖棕深底终端（亮 / 暗只是底色深浅不同） */
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
  ...sb("239, 226, 207"),
};

/** 极简亮：白纸终端。设计稿 .term / .t-*；浅底下 white / brightWhite 取可读的灰 */
const MINIMAL_LIGHT: ITheme = {
  foreground: "#2a2f36", // 设计稿 .term
  cursor: "#16191d", // 设计稿 .cursor
  selectionBackground: "rgba(51, 85, 224, 0.18)",
  black: "#16191d",
  red: "#c53030", // .t-red
  green: "#2f855a", // .t-str
  yellow: "#b7791f", // .t-amber
  blue: "#3355e0", // .t-ps
  magenta: "#7357e8", // .t-vio
  cyan: "#2f6f8f", // .t-path
  white: "#5d646c",
  brightBlack: "#9aa0a7", // .t-dim
  brightRed: "#e04848",
  brightGreen: "#38a169",
  brightYellow: "#d69e2e",
  brightBlue: "#5571ea",
  brightMagenta: "#8e76f0",
  brightCyan: "#3d8bb0",
  brightWhite: "#7a8189",
  ...sb("22, 25, 29"),
};

/** 极简暗：取原版 01-minimal.html 暗色的 --t-* */
const MINIMAL_DARK: ITheme = {
  foreground: "#d7d9df",
  cursor: "#e9eaee",
  selectionBackground: "rgba(125, 147, 255, 0.28)",
  black: "#3a3d45",
  red: "#f07a72",
  green: "#6fcf97",
  yellow: "#e7b464",
  blue: "#8ea2ff",
  magenta: "#b897f2",
  cyan: "#6cc7d6",
  white: "#d7d9df",
  brightBlack: "#666b76",
  brightRed: "#f59a94",
  brightGreen: "#8fdcae",
  brightYellow: "#efc888",
  brightBlue: "#aab9ff",
  brightMagenta: "#cbb2f6",
  brightCyan: "#8dd5e1",
  brightWhite: "#f4f5f7",
  ...sb("233, 234, 238"),
};

/** 奶油风亮：燕麦奶纸面终端。设计稿 .term 各色；其余按同色系补齐 */
const MILK_LIGHT: ITheme = {
  foreground: "#4e3929", // 设计稿 .term
  cursor: "#b9824a", // 设计稿 .cursor
  selectionBackground: "rgba(185, 130, 74, 0.25)",
  black: "#3e2c1f", // .cmd
  red: "#b4676e", // .mod
  green: "#5e7d4f", // .add / .ok
  yellow: "#9a6630", // .path
  blue: "#4f6c8a", // .ps
  magenta: "#8e6a8f",
  cyan: "#4f8079",
  white: "#8a6b52",
  brightBlack: "#ad957f", // .dim
  brightRed: "#c5767d",
  brightGreen: "#7e9a6a", // .br
  brightYellow: "#b9824a", // .cl
  brightBlue: "#6a87a6",
  brightMagenta: "#a784a8",
  brightCyan: "#6a9a92",
  brightWhite: "#9c8068",
  ...sb("173, 140, 100", [0.3, 0.45, 0.6]),
};

/** 毛玻璃：烟色玻璃终端。注明“设计稿”的取自 fd-03-glass.html 的 .term / .t-*，其余为按冷色调补齐；亮 / 暗共用，只换底色 */
const GLASS_ANSI: ITheme = {
  foreground: "#d8dee9", // 设计稿 .term
  cursor: "#d8dee9", // 设计稿 .cursor
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
  ...sb("255, 255, 255"),
};

/** 北欧莫兰迪：石墨板终端。设计稿 .t-*（fd-04）与原版 04-morandi.html 的 --t-*；亮 / 暗共用，只换底色 */
const MORANDI_ANSI: ITheme = {
  foreground: "#d5d1c8", // 设计稿 --term-fg
  cursor: "#d5d1c8",
  selectionBackground: "rgba(169, 186, 198, 0.28)",
  black: "#4a5253", // 设计稿终端滚动条色
  red: "#d3a59d", // .t-r
  green: "#a9b9a2", // .t-g
  yellow: "#d4c095", // .t-y
  blue: "#a9bac6", // .t-ps
  magenta: "#c9a3a6", // 原版 --t-rose
  cyan: "#9fbdb8",
  white: "#d5d1c8",
  brightBlack: "#8c918d", // --term-dim
  brightRed: "#e0b8b0",
  brightGreen: "#bccab5",
  brightYellow: "#e0d0a8",
  brightBlue: "#bccad4",
  brightMagenta: "#d6b6b9",
  brightCyan: "#b3cec9",
  brightWhite: "#eae6de", // .t-cmd
  ...sb("213, 209, 200", [0.18, 0.3, 0.42]),
};

/** 现代暗色暗：设计稿 fd-05 .term 各色 */
const MODERN_DARK: ITheme = {
  foreground: "#c9cdd6", // .term
  cursor: "#c9cdd6", // .caret
  selectionBackground: "rgba(139, 147, 255, 0.3)", // .sel 的强调色
  black: "#2a2d35",
  red: "#f2677e", // .r
  green: "#57d69b", // .g
  yellow: "#f0b65a", // .y
  blue: "#8b93ff", // .path
  magenta: "#b58cff", // .br
  cyan: "#5bc6e8", // .c
  white: "#c9cdd6",
  brightBlack: "#6b707c", // .d / .ps
  brightRed: "#f78a9c",
  brightGreen: "#7be2b2",
  brightYellow: "#f5c97e",
  brightBlue: "#a9afff",
  brightMagenta: "#c9a9ff",
  brightCyan: "#82d6f0",
  brightWhite: "#e8eaf0", // .cmd
  ...sb("255, 255, 255", [0.1, 0.2, 0.32]),
};

/** 现代暗色亮（推导）：同一组色相加深到白底可读 */
const MODERN_LIGHT: ITheme = {
  foreground: "#2a2e38",
  cursor: "#14161c",
  selectionBackground: "rgba(91, 99, 240, 0.18)",
  black: "#14161c",
  red: "#d93a56",
  green: "#1f9d66",
  yellow: "#b87a10",
  blue: "#5b63f0",
  magenta: "#8b5cf6",
  cyan: "#1a8db5",
  white: "#4f5463",
  brightBlack: "#868b99",
  brightRed: "#e85a72",
  brightGreen: "#2bb87a",
  brightYellow: "#cf931e",
  brightBlue: "#7c83f5",
  brightMagenta: "#a07cf8",
  brightCyan: "#2ba6cc",
  brightWhite: "#6b707c",
  ...sb("20, 22, 28"),
};

export const THEMES: readonly ThemeDef[] = [
  { id: "cream-light", style: "cream", dark: false, xterm: on(WARM_ANSI, "#3a2d24") },
  { id: "cream-dark", style: "cream", dark: true, xterm: on(WARM_ANSI, "#2e231b") },
  { id: "minimal-light", style: "minimal", dark: false, xterm: on(MINIMAL_LIGHT, "#ffffff") },
  { id: "minimal-dark", style: "minimal", dark: true, xterm: on(MINIMAL_DARK, "#0d0e11") },
  { id: "milk-light", style: "milk", dark: false, xterm: on(MILK_LIGHT, "#f7eedf") },
  { id: "milk-dark", style: "milk", dark: true, xterm: on(WARM_ANSI, "#241b15", { cursor: "#d6995a" }) },
  { id: "glass-light", style: "glass", dark: false, glassTerm: true, xterm: on(GLASS_ANSI, "#606574") }, // 设计稿 --smoke 叠在壁纸上的合成色
  { id: "glass-dark", style: "glass", dark: true, glassTerm: true, xterm: on(GLASS_ANSI, "#0e121e") }, // 深色烟晶叠在夜色壁纸上的合成色（截图取样均值）
  { id: "morandi-light", style: "morandi", dark: false, xterm: on(MORANDI_ANSI, "#33393a") },
  { id: "morandi-dark", style: "morandi", dark: true, xterm: on(MORANDI_ANSI, "#1c1e20") },
  { id: "modern-light", style: "modern", dark: false, xterm: on(MODERN_LIGHT, "#ffffff") },
  { id: "modern-dark", style: "modern", dark: true, xterm: on(MODERN_DARK, "#0b0c0f") },
];

export const DEFAULT_THEME = "cream-light";

/** 旧版本存的主题 id → 新 id */
const LEGACY = new Map([
  ["cream", "cream-light"],
  ["cream-night", "cream-dark"],
  ["glass", "glass-light"],
]);

const byId = (id: string | null | undefined) => THEMES.find((t) => t.id === id);

/** 由（风格, 明暗）得到主题；未知风格回退到默认主题 */
export function themeFor(style: string, dark: boolean): ThemeDef {
  return THEMES.find((t) => t.style === style && t.dark === dark) ?? byId(DEFAULT_THEME)!;
}

/** 读取存储值（主题 id，同时记下风格与明暗）：兼容旧值，空值 / 未知值回退到默认主题 */
export function parseTheme(stored: string | null): ThemeDef {
  return byId(stored) ?? byId(LEGACY.get(stored ?? "")) ?? byId(DEFAULT_THEME)!;
}

/** 同一风格内亮 ↔ 暗 */
export function toggledTheme(id: string): ThemeDef {
  const t = parseTheme(id);
  return themeFor(t.style, !t.dark);
}
