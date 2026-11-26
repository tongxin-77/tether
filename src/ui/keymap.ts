// 全局按键：Esc 栈——打开的菜单 / 面板登记一层，Esc 只关最上面一层；以及组合键（如 Ctrl+Shift+K）。
// 有模态对话框打开时都不处理（Esc 交给对话框）；已被处理（defaultPrevented，如编辑器关闭查找框）的不再处理。
// 终端先问 handles：要由这里处理的键不再发给程序（没有登记的组合键如 Ctrl+K 照常透传）

type Key = Pick<KeyboardEvent, "key" | "defaultPrevented"> & Partial<Pick<KeyboardEvent, "ctrlKey" | "shiftKey" | "altKey" | "metaKey" | "preventDefault">>;

export interface Keymap {
  /** 一层 Esc：返回开关，打开时调用 (true) 压到栈顶，关闭时 (false) 移出 */
  esc(close: () => void): (open: boolean) => void;
  /** 登记组合键，写法如 "Ctrl+Shift+K"（修饰键顺序 Ctrl、Shift、Alt）；返回取消登记 */
  bind(combo: string, fn: () => void): () => void;
  /** 该键会由 keymap 处理 */
  handles(ev: Key): boolean;
  /** 处理按键，返回是否已处理（组合键同时阻止默认行为） */
  handle(ev: Key): boolean;
}

/** 按键的组合键写法；单字符键取大写 */
export const comboOf = (ev: Key) =>
  `${ev.ctrlKey || ev.metaKey ? "Ctrl+" : ""}${ev.shiftKey ? "Shift+" : ""}${ev.altKey ? "Alt+" : ""}${ev.key.length === 1 ? ev.key.toUpperCase() : ev.key}`;

/** blocked：有模态对话框打开等情况，此时不处理 */
export function createKeymap(blocked: () => boolean = () => false): Keymap {
  const stack: (() => void)[] = [];
  const binds = new Map<string, () => void>();
  const isEsc = (ev: Key) => ev.key === "Escape" && stack.length > 0;
  const handles = (ev: Key) => !ev.defaultPrevented && (isEsc(ev) || binds.has(comboOf(ev))) && !blocked();
  return {
    esc(close) {
      return (open) => {
        const i = stack.indexOf(close);
        if (i >= 0) stack.splice(i, 1);
        if (open) stack.push(close);
      };
    },
    bind(combo, fn) {
      binds.set(combo, fn);
      return () => binds.get(combo) === fn && binds.delete(combo);
    },
    handles,
    handle(ev) {
      if (!handles(ev)) return false;
      if (isEsc(ev)) stack[stack.length - 1]();
      else {
        ev.preventDefault?.();
        binds.get(comboOf(ev))!();
      }
      return true;
    },
  };
}

/** 应用唯一的 keymap，挂在 document 上 */
export const keymap = createKeymap(() => !!document.querySelector("dialog[open]"));
if (typeof document !== "undefined") document.addEventListener("keydown", (ev) => keymap.handle(ev));
