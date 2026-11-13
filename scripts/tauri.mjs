// `pnpm tauri ...` 的薄包装：build 时先编译 release 版 tetherd 并作为 sidecar（bundle.externalBin）打进安装包。
// externalBin 不写进 tauri.conf.json：tauri-build 在 dev 下也会把 sidecar 复制成 target/debug/tetherd.exe，
// 覆盖调试版 daemon，且 daemon 运行中时删除失败导致编译中断。
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';

const TRIPLE = 'x86_64-pc-windows-msvc';
const args = process.argv.slice(2);

function run(cmd, cmdArgs) {
  const r = spawnSync(cmd, cmdArgs, { stdio: 'inherit' });
  if (r.status !== 0) process.exit(r.status ?? 1);
}

if (args[0] === 'build') {
  run('cargo', ['build', '--release', '-p', 'tetherd']);
  mkdirSync('src-tauri/binaries', { recursive: true });
  copyFileSync('target/release/tetherd.exe', `src-tauri/binaries/tetherd-${TRIPLE}.exe`);
  args.push('--config', JSON.stringify({ bundle: { externalBin: ['binaries/tetherd'] } }));
}

run(process.execPath, ['node_modules/@tauri-apps/cli/tauri.js', ...args]);
