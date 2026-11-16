// 主题的运行时部分：设置 data-theme、记住选择、刷新终端配色、多窗口同步。
// 注册表（风格 × 明暗）与纯逻辑在 theme-utils.ts；界面颜色来自 styles.css 中按 data-theme 定义的语义变量。
import type { ITheme, Terminal } from "@xterm/xterm";
import { parseTheme, STYLES, themeFor, toggledTheme, xtermThemeFor, type StyleDef, type ThemeDef } from "./theme-utils";

/** 存主题 id（"<风格>-light|dark"，即风格与明暗两项）；旧值由 parseTheme 迁移。index.html 的防闪烁脚本读同一个键 */
const KEY = "tether.theme";
/** 已创建的终端：切换主题时整体替换其 theme */
const terms = new Set<Terminal>();
let current = load();
/** 窗口半透明时终端背景透明（由终端卡片提供半透明底色） */
let transparent = false;

export function getStyles(): readonly StyleDef[] {
  return STYLES;
}

export function currentTheme(): ThemeDef {
  return current;
}

/** 切换主题：设置 data-theme、更新全部终端、记住选择 */
function applyTheme(t: ThemeDef) {
  current = t;
  document.documentElement.dataset.theme = t.id;
  try {
    localStorage.setItem(KEY, t.id);
  } catch {}
  refreshTerminals();
}

/** 选择风格（设置面板），保持当前明暗 */
export function setStyle(style: string) {
  applyTheme(themeFor(style, current.dark));
}

/** 同一风格内切换亮色 / 暗色（侧栏按钮） */
export function toggleDark() {
  applyTheme(toggledTheme(current.id));
}

/** 当前主题下的 xterm 配色 */
export function xtermTheme(): ITheme {
  return xtermThemeFor(current, transparent);
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
  return parseTheme(id);
}

// 模块加载即应用（index.html 头部的内联脚本已提前设置过，避免首帧闪烁；这里兜底校验）
document.documentElement.dataset.theme = current.id;
// 其它窗口（拖出的独立窗口）切换主题时同步
window.addEventListener("storage", (ev) => {
  if (ev.key === KEY) {
    current = load();
    document.documentElement.dataset.theme = current.id;
    refreshTerminals();
  }
});
