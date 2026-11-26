// 品牌区：标志、名称、版本号（updater.ts 写入 #brand-ver）与新建按钮；新建菜单列出本地、每台主机与恢复 Claude 对话
import { t } from "../i18n";
import { icon } from "../icons";
import { type Component, type Ctx, html } from "../ui/component";
import { activeSession } from "../ui/store";

export function createBrand(ctx: Ctx): Component {
  const { store, actions } = ctx;
  const el = html(`<div class="c-brand" id="brand">
      <span class="brand-mark"><svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 17H7A5 5 0 0 1 7 7h2"/><path d="M15 7h2a5 5 0 1 1 0 10h-2"/><line x1="8" x2="16" y1="12" y2="12"/></svg></span>
      <span class="brand-name">Tether</span>
      <span class="brand-ver" id="brand-ver"></span>
      <div class="new-wrap">
        <button id="new-btn" data-i18n-title="sidebar.newTitle">${icon("plus")}</button>
        <div id="new-menu" data-tauri-drag-region="false" hidden></div>
      </div>
    </div>`);
  const newMenu = el.querySelector<HTMLElement>("#new-menu")!;

  el.querySelector("#new-btn")!.addEventListener("click", (ev) => {
    ev.stopPropagation();
    if (!newMenu.hidden) {
      newMenu.hidden = true;
      return;
    }
    const item = (ico: "folder-open" | "server" | "history", text: string, sub: string, action: () => void) => {
      const b = document.createElement("button");
      b.innerHTML = `<span class="mi-ico">${icon(ico)}</span><span class="mi-text"></span>`;
      const tx = b.querySelector(".mi-text")!;
      tx.append(text);
      if (sub) tx.appendChild(document.createElement("small")).textContent = sub;
      b.addEventListener("click", () => {
        newMenu.hidden = true;
        action();
      });
      return b;
    };
    newMenu.replaceChildren(
      item("folder-open", t("newMenu.local"), t("newMenu.localSub"), () => actions.createLocal()),
      ...store.get().hosts.map((h) => item("server", h.name, `${h.user}@${h.host}`, () => actions.createAt({ kind: "remote", hostId: h.id }))),
      // 当前为本地会话时列出其启动目录的对话，否则先选目录
      item("history", t("newMenu.resume"), t("newMenu.resumeSub"), () => {
        const s = activeSession(store.get());
        actions.openClaudeHistory(s && !s.host ? s.cwd : undefined);
      }),
    );
    newMenu.hidden = false;
  });
  document.addEventListener("click", () => (newMenu.hidden = true));
  return { el };
}
