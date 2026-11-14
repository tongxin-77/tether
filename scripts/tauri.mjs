// `pnpm tauri ...` 的薄包装：build 时先编译 release 版 tetherd 并作为 sidecar（bundle.externalBin）打进安装包。
// externalBin 不写进 tauri.conf.json：tauri-build 在 dev 下也会把 sidecar 复制成 target/debug/tetherd.exe，
// 覆盖调试版 daemon，且 daemon 运行中时删除失败导致编译中断。
// build 时若找到更新签名私钥（%USERPROFILE%\.tauri\tether.key），从 Windows 凭据管理器读私钥密码，
// 只经环境变量传给 tauri 子进程（不打印、不落盘），并开启 createUpdaterArtifacts 生成 .sig；找不到私钥则照常构建无签名包。
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const TRIPLE = 'x86_64-pc-windows-msvc';
const KEY_FILE = join(homedir(), '.tauri', 'tether.key');
/** 凭据管理器中保存私钥密码的普通凭据（cmdkey /generic:tether-updater-signing） */
const CRED_TARGET = 'tether-updater-signing';
const args = process.argv.slice(2);

function run(cmd, cmdArgs, env = process.env) {
  const r = spawnSync(cmd, cmdArgs, { stdio: 'inherit', env });
  if (r.status !== 0) process.exit(r.status ?? 1);
}

/** 读普通凭据的密码（CredReadW，无需额外模块）；不存在时返回 undefined */
function readCredential(target) {
  const ps = `
[Console]::OutputEncoding = [Text.Encoding]::UTF8
Add-Type -Namespace W -Name C -MemberDefinition @'
[DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool CredReadW(string target, int type, int flags, out IntPtr cred);
[DllImport("advapi32.dll")] public static extern void CredFree(IntPtr cred);
[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct CREDENTIAL { public int Flags; public int Type; public string TargetName; public string Comment; public long LastWritten;
  public int CredentialBlobSize; public IntPtr CredentialBlob; public int Persist; public int AttributeCount; public IntPtr Attributes;
  public string TargetAlias; public string UserName; }
'@
$p = [IntPtr]::Zero
if (-not [W.C]::CredReadW($env:TETHER_CRED_TARGET, 1, 0, [ref]$p)) { exit 2 }
$c = [Runtime.InteropServices.Marshal]::PtrToStructure($p, [type][W.C+CREDENTIAL])
[Console]::Out.Write([Runtime.InteropServices.Marshal]::PtrToStringUni($c.CredentialBlob, $c.CredentialBlobSize / 2))
[W.C]::CredFree($p)
`;
  // 脚本含引号与换行，用 -EncodedCommand（UTF-16LE base64）避免命令行转义问题
  const encoded = Buffer.from(ps, 'utf16le').toString('base64');
  const r = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', encoded], {
    encoding: 'utf8',
    env: { ...process.env, TETHER_CRED_TARGET: target },
  });
  return r.status === 0 ? r.stdout : undefined;
}

/** 签名用的子进程环境；没有私钥时返回 undefined */
function signingEnv() {
  const env = { ...process.env };
  if (!env.TAURI_SIGNING_PRIVATE_KEY) {
    if (!existsSync(KEY_FILE)) return undefined;
    env.TAURI_SIGNING_PRIVATE_KEY = KEY_FILE;
  }
  if (env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD === undefined) {
    const password = readCredential(CRED_TARGET);
    if (password === undefined) console.warn(`[tauri.mjs] 凭据管理器中没有 ${CRED_TARGET}，按无密码私钥签名`);
    env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = password ?? '';
  }
  return env;
}

let env = process.env;
if (args[0] === 'build') {
  run('cargo', ['build', '--release', '-p', 'tetherd']);
  mkdirSync('src-tauri/binaries', { recursive: true });
  copyFileSync('target/release/tetherd.exe', `src-tauri/binaries/tetherd-${TRIPLE}.exe`);
  const config = { bundle: { externalBin: ['binaries/tetherd'] } };
  const signing = signingEnv();
  if (signing) {
    env = signing;
    config.bundle.createUpdaterArtifacts = true;
  } else {
    console.warn(`[tauri.mjs] 未找到更新签名私钥（${KEY_FILE}），构建不含 .sig 的安装包，不能用于自动更新`);
  }
  args.push('--config', JSON.stringify(config));
}

run(process.execPath, ['node_modules/@tauri-apps/cli/tauri.js', ...args], env);
