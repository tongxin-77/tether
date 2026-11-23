import { describe, expect, it } from "vitest";
import { notesForLocale, notesSince } from "./update-notes";

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

describe("notesForLocale", () => {
  const both = `<!-- lang:zh-CN -->
## [0.6.0] - 2026-12-01

- 中文说明

<!-- lang:en -->
## [0.6.0] - 2026-12-01

- English notes`;
  it("zh-CN / zh-TW 取中文段，en / ja 取英文段", () => {
    expect(notesForLocale(both, "zh-CN")).toBe("## [0.6.0] - 2026-12-01\n\n- 中文说明");
    expect(notesForLocale(both, "zh-TW")).toBe("## [0.6.0] - 2026-12-01\n\n- 中文说明");
    expect(notesForLocale(both, "en")).toBe("## [0.6.0] - 2026-12-01\n\n- English notes");
    expect(notesForLocale(both, "ja")).toBe("## [0.6.0] - 2026-12-01\n\n- English notes");
  });
  it("没有标记的旧格式整段返回", () => {
    expect(notesForLocale(body, "en")).toBe(body);
  });
  it("所需语言段缺失时取另一种", () => {
    expect(notesForLocale("<!-- lang:zh-CN -->\n- 只有中文", "ja")).toBe("- 只有中文");
    expect(notesForLocale("<!-- lang:en -->\n- only en", "zh-TW")).toBe("- only en");
  });
  it("兼容 CRLF 与标记前后空白；选段后再按版本截取", () => {
    const crlf = "<!-- lang:zh-CN -->\r\n## [0.6.0]\r\n\r\n- 新\r\n\r\n## [0.5.0]\r\n\r\n- 旧\r\n  <!--  lang:en  -->  \r\n## [0.6.0]\r\n\r\n- new\r\n\r\n## [0.5.0]\r\n\r\n- old";
    expect(notesSince(notesForLocale(crlf, "en"), "0.5.0")).toBe("## [0.6.0]\n\n- new");
    expect(notesSince(notesForLocale(crlf, "zh-CN"), "0.5.0")).toBe("## [0.6.0]\n\n- 新");
  });
});
