// 布局：index.html 只有槽位容器（data-slot），预设决定哪些组件按什么顺序放进哪个槽位；
// 没有组件的槽位隐藏。主窗口与拖出的独立窗口各用一套预设
import type { Component } from "./component";

export type Slot = "titlebar" | "left" | "main" | "bottom" | "right" | "overlay";
export type Preset = Partial<Record<Slot, string[]>>;

/** 主窗口：顶部单条横栏 = 品牌与新建 + 卡片头 + 窗口按钮；侧栏 = 会话列表 + 底栏；中间列 = 终端 + 覆盖层（预览 / 历史）；中间列下方 = 变更面板；右侧 = 文件栏 + 主机信息 */
export const PRESETS = {
  main: {
    titlebar: ["brand", "cardHead", "titleBar"],
    left: ["sessions", "sidebarFoot"],
    main: ["terminal", "preview", "history"],
    bottom: ["changes"],
    right: ["files", "hostInfo"],
    overlay: ["settings"],
  },
  /** 独立窗口：横栏只有卡片头与窗口按钮，只有中间列（无会话列表、文件栏、主机信息） */
  detached: {
    titlebar: ["cardHead", "titleBar"],
    main: ["terminal", "preview", "history"],
    overlay: ["settings"],
  },
} satisfies Record<string, Preset>;

export type LayoutName = keyof typeof PRESETS;

interface SlotEl {
  hidden: HTMLElement["hidden"];
  append(...nodes: HTMLElement[]): void;
}

/** 按预设把组件放进槽位：先放根元素再 mount，最后告知各组件是否显示 */
export function applyLayout(preset: Preset, parts: Record<string, Component>, slots: Record<Slot, SlotEl>) {
  const shown = new Set<string>();
  for (const [slot, names] of Object.entries(preset) as [Slot, string[]][]) {
    for (const name of names) {
      const c = parts[name];
      if (!c) throw new Error(`layout: unknown component ${name}`);
      slots[slot].append(c.el);
      c.mount?.();
      shown.add(name);
    }
  }
  for (const slot of Object.keys(slots) as Slot[]) if (slot !== "overlay") slots[slot].hidden = !preset[slot]?.length;
  for (const [name, c] of Object.entries(parts)) c.onVisible?.(shown.has(name));
}

/** 页面中的槽位容器 */
export function pageSlots(): Record<Slot, HTMLElement> {
  // 槽位 class（.slot-left 等）供组件按所在槽位调整样式（如变更面板在侧栏时的窄版）
  const get = (s: Slot) => {
    const e = document.querySelector<HTMLElement>(`[data-slot="${s}"]`)!;
    e.classList.add(`slot-${s}`);
    return e;
  };
  return { titlebar: get("titlebar"), left: get("left"), main: get("main"), bottom: get("bottom"), right: get("right"), overlay: get("overlay") };
}

/** 菜单、对话框所在的槽位 */
export function overlay(): HTMLElement {
  return document.querySelector<HTMLElement>('[data-slot="overlay"]') ?? document.body;
}
