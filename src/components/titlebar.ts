// 自定义标题栏的窗口按钮；拖动与双击最大化由槽位上的 data-tauri-drag-region 处理。最大化状态变化（含双击、Win+↑）时切换图标
import { getCurrentWindow } from "@tauri-apps/api/window";
import { onLocaleChange, t } from "../i18n";
import { icon } from "../icons";
import { type Component, html } from "../ui/component";

export function createTitleBar(): Component {
  const el = html(`<div class="c-titlebar">
      <button id="win-min" class="win-btn" data-i18n-title="titlebar.minimize"></button>
      <button id="win-max" class="win-btn"></button>
      <button id="win-close" class="win-btn" data-i18n-title="common.close"></button>
    </div>`);
  const btn = (id: string) => el.querySelector<HTMLButtonElement>(`#${id}`)!;
  const win = getCurrentWindow();
  const maxBtn = btn("win-max");
  btn("win-min").innerHTML = icon("minus");
  btn("win-close").innerHTML = icon("x");
  btn("win-min").addEventListener("click", () => win.minimize());
  maxBtn.addEventListener("click", () => win.toggleMaximize());
  btn("win-close").addEventListener("click", () => win.close());
  const sync = async () => {
    const max = await win.isMaximized();
    maxBtn.innerHTML = icon(max ? "copy" : "square");
    maxBtn.title = max ? t("titlebar.restore") : t("titlebar.maximize");
  };
  sync();
  win.onResized(sync);
  onLocaleChange(sync);
  return { el };
}
