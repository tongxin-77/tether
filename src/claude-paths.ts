// 把文件交给 Claude Code：@ 引用与拖入终端的路径文字（纯函数，无副作用）

/** 本地路径统一为正斜杠（Claude Code 在 Windows 上按 Node path 解析，两种分隔符都认；正斜杠不会与输入框的 \ 续行混淆） */
const slash = (p: string, remote: boolean) => (remote ? p : p.replace(/\\/g, "/"));

/** path 严格位于 dir 之内时返回相对路径（正斜杠），否则 null；本地不区分大小写 */
export function relativePath(path: string, dir: string, remote: boolean): string | null {
  const p = slash(path, remote);
  const prefix = slash(dir, remote).replace(/\/+$/, "") + "/";
  const fold = (s: string) => (remote ? s : s.toLowerCase());
  if (p.length <= prefix.length || !fold(p).startsWith(fold(prefix))) return null;
  return p.slice(prefix.length);
}

/** Claude Code 的 @ 引用：会话启动目录内用相对路径，否则绝对路径；含空白时 @"…" */
export function claudeMention(path: string, cwd: string, remote: boolean): string {
  const p = relativePath(path, cwd, remote) ?? slash(path, remote);
  return /\s/.test(p) ? `@"${p}"` : `@${p}`;
}

/** 多个 @ 引用：空格分隔，末尾一个空格（不回车） */
export function claudeMentions(paths: string[], cwd: string, remote: boolean): string {
  return paths.map((p) => claudeMention(p, cwd, remote) + " ").join("");
}

/** 拖入终端的文字：Claude 运行中为本地 @ 引用，否则为双引号包裹的原路径；末尾一个空格 */
export function dropText(paths: string[], cwd: string, claude: boolean): string {
  return claude ? claudeMentions(paths, cwd, false) : paths.map((p) => `"${p}" `).join("");
}
