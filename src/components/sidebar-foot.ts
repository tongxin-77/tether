// 侧栏底部：更新标签（updater.ts 控制）、设置按钮、亮色 / 暗色切换
import { onLocaleChange, t } from "../i18n";
import { icon } from "../icons";
import { currentTheme, toggleDark } from "../themes";
import { type Component, type Ctx, html } from "../ui/component";

export function createSidebarFoot(ctx: Ctx): Component {
  const el = html(`<div class="side-foot">
      <!-- 自动检查发现新版本时显示，点击打开更新对话框 -->
      <button id="update-badge" hidden></button>
      <div class="foot-row">
        <button id="settings-btn"><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 7h-9"/><path d="M14 17H5"/><circle cx="17" cy="17" r="3"/><circle cx="7" cy="7" r="3"/></svg><span data-i18n="sidebar.settings"></span></button>
        <!-- 同一风格内切换亮色 / 暗色（图标与提示按当前模式填写），风格在设置面板选择 -->
        <button id="theme-btn" class="icon-btn"></button>
      </div>
    </div>`);
  const themeBtn = el.querySelector<HTMLButtonElement>("#theme-btn")!;
  // 图标表示当前模式，提示表示点击后的模式
  const sync = () => {
    const dark = currentTheme().dark;
    themeBtn.innerHTML = icon(dark ? "moon" : "sun");
    themeBtn.title = dark ? t("theme.toLight") : t("theme.toDark");
  };
  themeBtn.addEventListener("click", () => {
    toggleDark();
    sync();
  });
  el.querySelector("#settings-btn")!.addEventListener("click", () => ctx.actions.openSettings());
  sync();
  onLocaleChange(sync);
  return { el };
}
