import { describe, expect, it } from "vitest";
import { notesSince } from "./update-notes";

const body = `## [0.5.6] - 2026-11-18

### ✨ 新增

- **C**

## [0.5.5] - 2026-11-17

- **B**

## [0.5.4] - 2026-11-16

- **A**`;

describe("notesSince", () => {
  it("只保留比当前版本新的段落", () => {
    expect(notesSince(body, "0.5.4")).toBe(`## [0.5.6] - 2026-11-18

### ✨ 新增

- **C**

## [0.5.5] - 2026-11-17

- **B**`);
  });
  it("按数字比较版本号", () => {
    expect(notesSince("## [0.10.0]\n\n- x\n\n## [0.9.0]\n\n- y", "0.9.0")).toBe("## [0.10.0]\n\n- x");
  });
  it("没有版本标题的旧格式原样返回", () => {
    expect(notesSince("### 新增\n\n- x", "0.5.4")).toBe("### 新增\n\n- x");
  });
  it("没有比当前新的段落时原样返回", () => {
    expect(notesSince(body, "0.6.0")).toBe(body);
  });
});
