import type { IBufferLine, ILink, Terminal } from "@xterm/xterm";

/** 终端输出中的文件路径：Windows 绝对路径、./ ../ 或 / 开头、或含分隔符的相对路径，末段须带扩展名；可带 :行[:列] 后缀。
 *  前一字符须为行首或分隔符（排除 URL 中的 //host/a.js），后一字符须为行尾、空白或常见标点 */
const PATH_RE =
  /(?<![^\s"'`(\[{<|,;=])((?:[A-Za-z]:[\\/]|\.{1,2}[\\/]|[\\/])?(?:[^\s\\/:*?"<>|()[\]{}'`,;]+[\\/])*[^\s\\/:*?"<>|()[\]{}'`,;]*\.[A-Za-z0-9]+)(?::\d+(?::\d+)?)?(?=$|[\s"'`)\]}>|,;:.!?])/g;

/** 一行的文本及每个字符所在的列（宽字符占两列，跳过其后的占位单元格） */
function lineText(line: IBufferLine, cols: number) {
  let text = "";
  const colOf: number[] = [];
  for (let x = 0; x < cols; x++) {
    const cell = line.getCell(x);
    if (!cell || cell.getWidth() === 0) continue;
    const chars = cell.getChars() || " ";
    for (let i = 0; i < chars.length; i++) colOf.push(x);
    text += chars;
  }
  return { text, colOf };
}

/** 为终端注册文件路径链接：悬停下划线，左键单击时以会话目录解析相对路径并交给 open（文件是否存在由打开时判断） */
export function attachFileLinks(term: Terminal, cwd: () => { dir: string; remote: boolean }, open: (path: string) => void) {
  term.registerLinkProvider({
    provideLinks(y, callback) {
      const line = term.buffer.active.getLine(y - 1);
      if (!line) return callback(undefined);
      const { text, colOf } = lineText(line, term.cols);
      const links: ILink[] = [];
      for (const m of text.matchAll(PATH_RE)) {
        const path = m[1];
        if (!/[\\/]/.test(path)) continue;
        const start = m.index;
        const end = start + m[0].length - 1;
        links.push({
          text: m[0],
          range: { start: { x: colOf[start] + 1, y }, end: { x: colOf[end] + 1, y } },
          activate: (ev) => {
            // 拖选结束在路径上时不打开，保证路径可选中复制
            if (ev.button === 0 && !term.hasSelection()) open(resolve(path, cwd()));
          },
        });
      }
      callback(links.length ? links : undefined);
    },
  });
}

function resolve(path: string, { dir, remote }: { dir: string; remote: boolean }) {
  if (/^[A-Za-z]:[\\/]/.test(path) || /^[\\/]/.test(path)) return path;
  const sep = remote ? "/" : "\\";
  const rel = path.replace(/^\.[\\/]/, "");
  const base = dir.replace(/[\\/]+$/, "");
  return base + sep + (remote ? rel : rel.replace(/\//g, "\\"));
}
