import { describe, expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
import { TERMINAL_OPTIONS } from "./term-utils";

// 表格单元格选择（table-select.ts）用 registerDecoration，它是 xterm 的 proposed API，未开启时直接抛错
describe("终端选项", () => {
  it("允许 proposed API，registerDecoration 不抛错", () => {
    const term = new Terminal(TERMINAL_OPTIONS);
    const marker = term.registerMarker(0)!;
    expect(() => term.registerDecoration({ marker, x: 0, width: 1 })).not.toThrow();
  });
});
