import { describe, expect, it } from "vitest";
import { applyLayout, PRESETS, type Slot } from "./layout";
import type { Component } from "./component";

const slot = () => {
  const s = { hidden: false, items: [] as unknown[], append: (...n: unknown[]) => s.items.push(...n) };
  return s;
};
const slots = () => ({ titlebar: slot(), left: slot(), main: slot(), bottom: slot(), right: slot(), overlay: slot() }) satisfies Record<Slot, unknown>;

function part(name: string, log: string[]) {
  const c: Component = {
    el: name as unknown as HTMLElement,
    mount: () => log.push(`mount ${name}`),
    onVisible: (v) => log.push(`${name} ${v}`),
  };
  return c;
}

describe("applyLayout", () => {
  it("按预设顺序放入槽位，空槽位隐藏，overlay 不隐藏", () => {
    const log: string[] = [];
    const s = slots();
    applyLayout({ main: ["a", "b"], right: ["c"] }, { a: part("a", log), b: part("b", log), c: part("c", log) }, s);
    expect(s.main.items).toEqual(["a", "b"]);
    expect(s.right.items).toEqual(["c"]);
    expect([s.main.hidden, s.right.hidden, s.left.hidden, s.bottom.hidden, s.overlay.hidden]).toEqual([false, false, true, true, false]);
    expect(log).toEqual(["mount a", "mount b", "mount c", "a true", "b true", "c true"]);
  });

  it("不在预设中的组件不放入、告知不可见", () => {
    const log: string[] = [];
    const s = slots();
    applyLayout({ main: ["a"] }, { a: part("a", log), x: part("x", log) }, s);
    expect(log).toContain("x false");
    expect(log).not.toContain("mount x");
  });

  it("预设引用未知组件时报错", () => {
    expect(() => applyLayout({ main: ["nope"] }, {}, slots())).toThrow(/nope/);
  });

  it("独立窗口预设：没有侧栏、文件栏与主机信息", () => {
    const d: Record<string, string[] | undefined> = PRESETS.detached;
    expect(d.left).toBeUndefined();
    expect(d.right).toBeUndefined();
    expect(d.main).not.toContain("hostInfo");
    expect(d.titlebar).not.toContain("brand");
    expect(PRESETS.main.right).toContain("hostInfo");
  });
});
