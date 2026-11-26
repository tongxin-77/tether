import { describe, expect, it } from "vitest";
import { createStore } from "./store";

describe("createStore", () => {
  it("set 浅合并，get 返回新状态", () => {
    const s = createStore({ a: 1, b: "x" });
    s.set({ a: 2 });
    expect(s.get()).toEqual({ a: 2, b: "x" });
  });

  it("select 只在所选值变化时通知，带上旧值", () => {
    const s = createStore({ a: 1, b: "x" });
    const calls: [number, number][] = [];
    s.select((st) => st.a, (v, prev) => calls.push([v, prev]));
    s.set({ b: "y" });
    s.set({ a: 1 });
    s.set({ a: 3 });
    expect(calls).toEqual([[3, 1]]);
  });

  it("可自定义比较；取消订阅后不再通知", () => {
    const s = createStore({ list: [1, 2] });
    let n = 0;
    const off = s.select((st) => st.list, () => n++, (x, y) => x.join() === y.join());
    s.set({ list: [1, 2] });
    s.set({ list: [1, 2, 3] });
    off();
    s.set({ list: [4] });
    expect(n).toBe(1);
  });

  it("订阅者按订阅顺序收到通知，回调中读到的是新状态", () => {
    const s = createStore({ a: 0 });
    const seen: string[] = [];
    s.select((st) => st.a, () => seen.push(`1:${s.get().a}`));
    s.select((st) => st.a, () => seen.push(`2:${s.get().a}`));
    s.set({ a: 5 });
    expect(seen).toEqual(["1:5", "2:5"]);
  });
});
