// 生成自动更新清单 latest.json（随安装包一起上传到 GitHub Release，客户端从 releases/latest/download/latest.json 读取）。
// 用法：node scripts/latest-json.mjs <版本号> <输出路径>
// signature 必须是 .sig 文件的内容（不是路径或 URL）；notes 取 CHANGELOG.md 中该版本及之前各版本的段落（含 `## [x.y.z]` 标题），客户端只显示比自身新的版本。
import { readFileSync, writeFileSync } from 'node:fs';

const [version, out] = process.argv.slice(2);
if (!version || !out) {
  console.error('用法：node scripts/latest-json.mjs <版本号> <输出路径>');
  process.exit(1);
}

const name = `tether_${version}_x64-setup.exe`;
const signature = readFileSync(`target/release/bundle/nsis/${name}.sig`, 'utf8').trim();

const lines = readFileSync('CHANGELOG.md', 'utf8').split(/\r?\n/);
const start = lines.findIndex((l) => l.startsWith(`## [${version}]`));
if (start < 0) {
  console.error(`CHANGELOG.md 中没有 ${version} 段`);
  process.exit(1);
}
const rest = lines.slice(start);
const end = rest.findIndex((l) => /^\[[^\]]+\]:/.test(l));
const notes = (end < 0 ? rest : rest.slice(0, end)).join('\n').trim();

const manifest = {
  version,
  notes,
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
