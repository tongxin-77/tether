// 文件栏地址栏输入的路径解析（纯函数，无副作用）

/**
 * 把地址栏输入解析为规范的绝对路径：支持 `~`、绝对路径、相对 base 的相对路径与 `.` / `..`；
 * 本地（Windows）正反斜杠都认，结果统一为 `\`、盘符大写，盘符根为 `C:\`，UNC 保留 `\\server\share` 前缀；
 * 远程（POSIX）只认 `/`。首尾的一对双引号（资源管理器“复制为路径”）会去掉。
 * 无法解析（空输入、`~` 但 home 未知、base 为盘符列表时的相对路径）返回 null。
 */
export function resolveInput(input: string, base: string, home: string | null, remote: boolean): string | null {
  let p = input.trim().replace(/^"(.*)"$/, "$1").trim();
  if (!p) return null;
  const sep = remote ? "/" : "\\";
  if (!remote) p = p.replace(/\//g, "\\");
  if (p === "~" || p.startsWith("~" + sep)) {
    if (home === null) return null;
    p = home + sep + p.slice(1);
  }
  return remote ? resolvePosix(p, base) : resolveWindows(p, base);
}

function resolvePosix(p: string, base: string): string {
  const full = p.startsWith("/") ? p : base.replace(/\/+$/, "") + "/" + p;
  return "/" + normalize(full.split("/")).join("/");
}

function resolveWindows(p: string, base: string): string | null {
  let prefix: string;
  let rest: string;
  const unc = /^\\\\([^\\]+\\[^\\]+)(.*)$/.exec(p);
  const drive = /^([A-Za-z]):(.*)$/.exec(p);
  if (unc) [prefix, rest] = ["\\\\" + unc[1], unc[2]];
  else if (drive) [prefix, rest] = [drive[1].toUpperCase() + ":", drive[2]];
  else {
    // 相对路径或 `\foo`（当前盘符根）：取 base 的前缀
    const b = base === "" ? null : resolveWindows(base, "");
    if (b === null) return null;
    const bp = /^(\\\\[^\\]+\\[^\\]+|[A-Z]:)(.*)$/.exec(b)!;
    [prefix, rest] = p.startsWith("\\") ? [bp[1], p] : [bp[1], bp[2] + "\\" + p];
  }
  const segs = normalize(rest.split("\\"));
  return segs.length ? prefix + "\\" + segs.join("\\") : prefix + "\\";
}

/** 去掉空段与 `.`，`..` 回退一级（到根为止） */
function normalize(parts: string[]): string[] {
  const out: string[] = [];
  for (const s of parts) {
    if (s === "" || s === ".") continue;
    if (s === "..") out.pop();
    else out.push(s);
  }
  return out;
}
