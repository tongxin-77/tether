import { describe, expect, it } from "vitest";
import { createKeymap } from "./keymap";

const esc = (defaultPrevented = false) => ({ key: "Escape", defaultPrevented });

describe("Esc 栈", () => {
  it("只关最上面一层，关闭后下一层接手", () => {
    const km = createKeymap();
    const closed: string[] = [];
    const a = km.esc(() => {
      closed.push("a");
      a(false);
    });
    const b = km.esc(() => {
      closed.push("b");
      b(false);
    });
    a(true);
    b(true);
    expect(km.handle(esc())).toBe(true);
    expect(km.handle(esc())).toBe(true);
    expect(km.handle(esc())).toBe(false);
    expect(closed).toEqual(["b", "a"]);
  });

  it("重新打开的层移到栈顶", () => {
    const km = createKeymap();
    const closed: string[] = [];
    const a = km.esc(() => closed.push("a"));
    const b = km.esc(() => closed.push("b"));
    a(true);
    b(true);
    a(true);
    km.handle(esc());
    expect(closed).toEqual(["a"]);
  });

  it("空栈、非 Esc、已被处理或被阻止（对话框打开）时不处理", () => {
    let blocked = false;
    const km = createKeymap(() => blocked);
    expect(km.handles(esc())).toBe(false);
    km.esc(() => {})(true);
    expect(km.handles(esc())).toBe(true);
    expect(km.handles({ key: "Enter", defaultPrevented: false })).toBe(false);
    expect(km.handles(esc(true))).toBe(false);
    blocked = true;
    expect(km.handles(esc())).toBe(false);
    expect(km.handle(esc())).toBe(false);
  });
});

describe("组合键", () => {
  const key = (key: string, mods: { ctrlKey?: boolean; shiftKey?: boolean; altKey?: boolean } = {}) => ({ key, defaultPrevented: false, ...mods });

  it("Ctrl+Shift+K 由 keymap 处理并阻止默认行为；Ctrl+K 等未登记的键透传", () => {
    const km = createKeymap();
    let n = 0;
    km.bind("Ctrl+Shift+K", () => n++);
    let prevented = false;
    expect(km.handles(key("k", { ctrlKey: true }))).toBe(false);
    expect(km.handles(key("K", { shiftKey: true }))).toBe(false);
    expect(km.handle({ ...key("K", { ctrlKey: true, shiftKey: true }), preventDefault: () => (prevented = true) })).toBe(true);
    expect(n).toBe(1);
    expect(prevented).toBe(true);
    // 小写 key（如 CapsLock）同样匹配
    expect(km.handles(key("k", { ctrlKey: true, shiftKey: true }))).toBe(true);
  });

  it("对话框打开时不处理；取消登记后不再处理", () => {
    let blocked = true;
    const km = createKeymap(() => blocked);
    const off = km.bind("Ctrl+Shift+K", () => {});
    expect(km.handles(key("K", { ctrlKey: true, shiftKey: true }))).toBe(false);
    blocked = false;
    expect(km.handles(key("K", { ctrlKey: true, shiftKey: true }))).toBe(true);
    off();
    expect(km.handles(key("K", { ctrlKey: true, shiftKey: true }))).toBe(false);
  });
});
