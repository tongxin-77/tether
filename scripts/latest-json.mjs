// 生成自动更新清单 latest.json（随安装包一起上传到 GitHub Release，客户端从 releases/latest/download/latest.json 读取）。
// 用法：node scripts/latest-json.mjs <版本号> <输出路径>
//       node scripts/latest-json.mjs --release-notes <版本号> <输出路径>   （GitHub Release 说明：该版本段的中英双语）
// signature 必须是 .sig 文件的内容（不是路径或 URL）；notes 取 CHANGELOG.md / CHANGELOG.en.md 中该版本及之前各版本的段落
// （含 `## [x.y.z]` 标题），以 `<!-- lang:zh-CN -->`、`<!-- lang:en -->` 标记分段拼接，客户端按界面语言选段、只显示比自身新的版本；
// CHANGELOG.en.md 缺该版本段时只输出中文（不加标记，与旧格式相同）。
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

/** CHANGELOG 中从 `## [version]` 起到文末链接定义之前的内容；没有该版本段返回 null */
export function notesFrom(changelog, version) {
  const lines = changelog.split(/\r?\n/);
  const start = lines.findIndex((l) => l.startsWith(`## [${version}]`));
  if (start < 0) return null;
  const rest = lines.slice(start);
  const end = rest.findIndex((l) => /^\[[^\]]+\]:/.test(l));
  return (end < 0 ? rest : rest.slice(0, end)).join('\n').trim();
}

/** 该版本段的正文（不含 `## [version]` 标题，到下一个版本段或链接定义为止）；没有该版本段返回 null */
export function versionSection(changelog, version) {
  const lines = changelog.split(/\r?\n/);
  const start = lines.findIndex((l) => l.startsWith(`## [${version}]`));
  if (start < 0) return null;
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((l) => /^## \[|^\[[^\]]+\]:/.test(l));
  return (end < 0 ? rest : rest.slice(0, end)).join('\n').trim();
}

/** latest.json 的 notes：中英两段带语言标记拼接；英文缺失时只有中文（无标记） */
export function bilingualNotes(zh, en) {
  if (!en) return zh;
  return `<!-- lang:zh-CN -->\n${zh}\n\n<!-- lang:en -->\n${en}`;
}

/** GitHub Release 说明：该版本段的中文在前、英文在后，以分隔线隔开；英文缺失时只有中文 */
export function releaseNotes(zh, en) {
  return en ? `${zh}\n\n---\n\n${en}\n` : `${zh}\n`;
}

function readChangelog(file) {
  return existsSync(file) ? readFileSync(file, 'utf8') : '';
}

function main(args) {
  const releaseMode = args[0] === '--release-notes';
  const [version, out] = releaseMode ? args.slice(1) : args;
  if (!version || !out) {
    console.error('用法：node scripts/latest-json.mjs [--release-notes] <版本号> <输出路径>');
    process.exit(1);
  }
  const zhLog = readChangelog('CHANGELOG.md');
  const enLog = readChangelog('CHANGELOG.en.md');
  const pick = releaseMode ? versionSection : notesFrom;
  const zh = pick(zhLog, version);
  if (zh === null) {
    console.error(`CHANGELOG.md 中没有 ${version} 段`);
    process.exit(1);
  }
  const en = pick(enLog, version);
  if (en === null) console.warn(`CHANGELOG.en.md 中没有 ${version} 段，只输出中文说明`);

  if (releaseMode) {
    writeFileSync(out, releaseNotes(zh, en));
    console.log(`已生成 ${out}`);
    return;
  }

  const name = `tether_${version}_x64-setup.exe`;
  const signature = readFileSync(`target/release/bundle/nsis/${name}.sig`, 'utf8').trim();
  const manifest = {
    version,
    notes: bilingualNotes(zh, en),
    pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, 'Z'),
    platforms: {
      'windows-x86_64': {
        signature,
        url: `https://github.com/tongxin-77/tether/releases/download/v${version}/${name}`,
      },
    },
  };
  writeFileSync(out, JSON.stringify(manifest, null, 2) + '\n');
  console.log(`已生成 ${out}`);
}

// 作为脚本运行时执行；被测试导入时只导出纯函数
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2));
}
