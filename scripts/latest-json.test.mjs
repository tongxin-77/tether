import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { bilingualNotes, notesFrom, releaseNotes, versionSection } from './latest-json.mjs';

const zhLog = `# 更新日志

## [未发布]

## [0.2.0] - 2026-01-02

### ✨ 新增

- **新功能**

## [0.1.0] - 2026-01-01

首个版本。

[未发布]: https://example.com/compare/v0.2.0...HEAD
[0.2.0]: https://example.com/v0.2.0
`;

const enLog = zhLog
  .replace('# 更新日志', '# Changelog')
  .replace('## [未发布]', '## [Unreleased]')
  .replace('### ✨ 新增', '### ✨ Added')
  .replace('**新功能**', '**New feature**')
  .replace('首个版本。', 'First release.');

describe('notesFrom', () => {
  it('从该版本截取到更早各版本，不含链接定义', () => {
    expect(notesFrom(zhLog, '0.2.0')).toBe(
      '## [0.2.0] - 2026-01-02\n\n### ✨ 新增\n\n- **新功能**\n\n## [0.1.0] - 2026-01-01\n\n首个版本。',
    );
    expect(notesFrom(zhLog, '0.1.0')).toBe('## [0.1.0] - 2026-01-01\n\n首个版本。');
  });

  it('兼容 CRLF，缺版本段返回 null', () => {
    expect(notesFrom(zhLog.replace(/\n/g, '\r\n'), '0.1.0')).toBe('## [0.1.0] - 2026-01-01\n\n首个版本。');
    expect(notesFrom(zhLog, '9.9.9')).toBeNull();
    expect(notesFrom('', '0.1.0')).toBeNull();
  });
});

describe('versionSection', () => {
  it('只取该版本段正文，不含标题', () => {
    expect(versionSection(zhLog, '0.2.0')).toBe('### ✨ 新增\n\n- **新功能**');
    expect(versionSection(zhLog, '0.1.0')).toBe('首个版本。');
    expect(versionSection(zhLog, '9.9.9')).toBeNull();
  });
});

describe('bilingualNotes', () => {
  it('中英两段带语言标记拼接', () => {
    const notes = bilingualNotes(notesFrom(zhLog, '0.2.0'), notesFrom(enLog, '0.2.0'));
    expect(notes.startsWith('<!-- lang:zh-CN -->\n## [0.2.0] - 2026-01-02\n')).toBe(true);
    expect(notes).toContain('首个版本。\n\n<!-- lang:en -->\n## [0.2.0] - 2026-01-02\n\n### ✨ Added');
    expect(notes.endsWith('First release.')).toBe(true);
  });

  it('英文缺失时只输出中文段（无标记）', () => {
    expect(bilingualNotes('## [0.1.0]\n\n中文', null)).toBe('## [0.1.0]\n\n中文');
  });
});

describe('releaseNotes', () => {
  it('中文在前、英文在后，分隔线隔开；英文缺失时只有中文', () => {
    expect(releaseNotes('中文', 'English')).toBe('中文\n\n---\n\nEnglish\n');
    expect(releaseNotes('中文', null)).toBe('中文\n');
  });
});

describe('CHANGELOG.en.md', () => {
  it('与 CHANGELOG.md 的版本段一一对应', () => {
    const versions = (text) => [...text.matchAll(/^## \[(\d[^\]]*)\]/gm)].map((m) => m[1]);
    const zh = versions(readFileSync('CHANGELOG.md', 'utf8'));
    expect(zh.length).toBeGreaterThan(0);
    expect(versions(readFileSync('CHANGELOG.en.md', 'utf8'))).toEqual(zh);
  });
});
