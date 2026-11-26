// 通用的确认 / 输入对话框与右键菜单（元素首次使用时建到 overlay 槽位）
import { t } from "../i18n";
import { html } from "./component";
import { keymap } from "./keymap";
import { overlay } from "./layout";

/** [文字, 动作, 是否禁用]；null 为分隔线 */
export type MenuItem = [string, () => void, boolean?] | null;

let dlg: HTMLDialogElement | undefined;
let menu: HTMLElement | undefined;
let menuEsc: (open: boolean) => void;

function dialog() {
  if (dlg) return dlg;
  dlg = html<HTMLDialogElement>(`<dialog id="fs-dialog">
      <form method="dialog">
        <p id="fs-d-msg"></p>
        <input id="fs-d-input" autocomplete="off" spellcheck="false" />
        <div class="actions">
          <button value="ok" id="fs-d-ok"></button>
          <button value="cancel" data-i18n="common.cancel"></button>
        </div>
      </form>
    </dialog>`);
  overlay().append(dlg);
  return dlg;
}

/** input 为 undefined 时是确认框；确定返回输入值（确认框为 ""），取消返回 null */
export function ask(message: string, ok: string, input?: string): Promise<string | null> {
  const d = dialog();
  const dlgInput = d.querySelector<HTMLInputElement>("#fs-d-input")!;
  const dlgOk = d.querySelector<HTMLButtonElement>("#fs-d-ok")!;
  d.querySelector("#fs-d-msg")!.textContent = message;
  dlgOk.textContent = ok;
  dlgOk.classList.toggle("danger", ok === t("common.delete"));
  dlgInput.hidden = input === undefined;
  dlgInput.value = input ?? "";
  d.returnValue = "";
  d.showModal();
  if (input !== undefined) {
    dlgInput.focus();
    // 默认选中不含扩展名的部分
    const dot = input.lastIndexOf(".");
    dlgInput.setSelectionRange(0, dot > 0 ? dot : input.length);
  }
  return new Promise((resolve) => d.addEventListener("close", () => resolve(d.returnValue === "ok" ? dlgInput.value.trim() : null), { once: true }));
}

function hideMenu() {
  menu!.hidden = true;
  menuEsc(false);
}

export function showMenu(x: number, y: number, items: MenuItem[]) {
  if (!menu) {
    menu = html(`<div id="fs-menu" hidden></div>`);
    overlay().append(menu);
    menuEsc = keymap.esc(hideMenu);
    document.addEventListener("click", hideMenu);
    window.addEventListener("blur", hideMenu);
  }
  const m = menu;
  m.replaceChildren(
    ...items.map((it) => {
      if (!it) return document.createElement("hr");
      const b = document.createElement("button");
      b.textContent = it[0];
      b.disabled = !!it[2];
      if (it[0] === t("common.delete")) b.className = "danger";
      b.addEventListener("click", () => {
        hideMenu();
        it[1]();
      });
      return b;
    }),
  );
  m.hidden = false;
  menuEsc(true);
  // 贴边时向内收
  const r = m.getBoundingClientRect();
  m.style.left = `${Math.min(x, innerWidth - r.width - 4)}px`;
  m.style.top = `${Math.min(y, innerHeight - r.height - 4)}px`;
}
