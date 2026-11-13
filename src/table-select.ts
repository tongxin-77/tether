import type { IDecoration, IMarker, Terminal } from "@xterm/xterm";

const BAR = "│";
/** 表格内部行分隔线（├─┼─┤）在单元格左边界处的字符 */
const SEP = "├┼";

interface CellSelection {
  text: string;
  marks: (IMarker | IDecoration)[];
}

const selections = new WeakMap<Terminal, CellSelection>();

/** 当前单元格选区的文本（折行已拼回一段），没有时为 undefined */
export function cellSelectionText(term: Terminal) {
  return selections.get(term)?.text;
}

export function clearCellSelection(term: Terminal) {
  selections.get(term)?.marks.forEach((m) => m.dispose());
  selections.delete(term);
}

/** Claude Code 框线表格的单元格内拖选：选区只覆盖该单元格所在列（不选中同行其它列），复制时把单元格内折行拼回一段，
 *  跨过行分隔线处换行。单击（未拖动）原样转交 xterm，保留链接点击；双击等仍走 xterm 原生选择 */
export function attachTableSelect(term: Terminal, box: HTMLElement) {
  let replaying = false;

  /** 一行每个单元格的字符：宽字符的占位单元格为空串，空白单元格为空格 */
  const chars = (y: number) => {
    const line = term.buffer.active.getLine(y);
    const out: string[] = [];
    for (let x = 0; x < term.cols; x++) {
      const cell = line?.getCell(x);
      out.push(!cell || cell.getWidth() === 0 ? "" : cell.getChars() || " ");
    }
    return out;
  };

  /** 鼠标位置对应的缓冲区单元格；clamp 时把屏幕外的位置夹到边缘（拖出终端区域时） */
  const cellAt = (ev: MouseEvent, clamp = false) => {
    const screen = term.element?.querySelector(".xterm-screen");
    if (!screen) return;
    const rect = screen.getBoundingClientRect();
    let x = Math.floor(((ev.clientX - rect.left) / rect.width) * term.cols);
    let y = Math.floor(((ev.clientY - rect.top) / rect.height) * term.rows);
    if (clamp) {
      x = Math.min(Math.max(x, 0), term.cols - 1);
      y = Math.min(Math.max(y, 0), term.rows - 1);
    } else if (x < 0 || y < 0 || x >= term.cols || y >= term.rows) return;
    return { x, y: y + term.buffer.active.viewportY };
  };

  const update = (l: number, r: number, a: { x: number; y: number }, b: { x: number; y: number }) => {
    clearCellSelection(term);
    const [s, e] = a.y < b.y || (a.y === b.y && a.x <= b.x) ? [a, b] : [b, a];
    const inCell = (x: number) => Math.min(Math.max(x, l + 1), r - 1);
    const buf = term.buffer.active;
    const marks: (IMarker | IDecoration)[] = [];
    const parts: string[] = [];
    for (let y = s.y; y <= e.y; y++) {
      const c = chars(y);
      if (SEP.includes(c[l])) {
        parts.push("\n");
        continue;
      }
      if (c[l] !== BAR || c[r] !== BAR) break; // 离开表格，选区止于上一行
      const from = y === s.y ? inCell(s.x) : l + 1;
      const to = y === e.y ? inCell(e.x) + 1 : r;
      if (to <= from) continue;
      parts.push(c.slice(from, to).join("").trim());
      const marker = term.registerMarker(y - (buf.baseY + buf.cursorY));
      if (!marker) continue;
      marks.push(marker);
      const deco = term.registerDecoration({ marker, x: from, width: to - from });
      if (!deco) continue;
      marks.push(deco);
      deco.onRender((el) => el.classList.add("cell-sel"));
    }
    // 折行处两侧都是 ASCII 时原文是空格断行，补回空格；中文折行直接拼接
    let text = "";
    for (const p of parts) {
      if (p === "\n") {
        if (text && !text.endsWith("\n")) text += "\n";
      } else if (p) {
        if (/[\x21-\x7e]$/.test(text) && /^[\x21-\x7e]/.test(p)) text += " ";
        text += p;
      }
    }
    selections.set(term, { text: text.trimEnd(), marks });
  };

  box.addEventListener(
    "mousedown",
    (ev) => {
      if (replaying) return;
      clearCellSelection(term);
      if (ev.button !== 0 || ev.detail > 1 || ev.shiftKey || ev.altKey || ev.ctrlKey || ev.metaKey) return;
      if (term.modes.mouseTrackingMode !== "none") return; // 程序在接收鼠标事件
      const p = cellAt(ev);
      if (!p) return;
      const c = chars(p.y);
      let l = p.x;
      while (l >= 0 && c[l] !== BAR) l--;
      let r = p.x;
      while (r < c.length && c[r] !== BAR) r++;
      if (l < 0 || r >= c.length || l === p.x || r === p.x) return; // 不在单元格内

      ev.preventDefault();
      ev.stopPropagation();
      term.clearSelection();
      term.focus();
      let moved = false;
      const move = (e: MouseEvent) => {
        const q = cellAt(e, true);
        if (!q) return;
        moved ||= q.x !== p.x || q.y !== p.y;
        if (moved) update(l, r, p, q);
      };
      const up = (e: MouseEvent) => {
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
        if (moved) return;
        // 单击：重放给 xterm（链接点击等）
        replaying = true;
        ev.target?.dispatchEvent(new MouseEvent("mousedown", ev));
        replaying = false;
        e.target?.dispatchEvent(new MouseEvent("mouseup", e));
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    },
    true,
  );

  // 开始原生选择时清掉单元格选区，两者不共存
  term.onSelectionChange(() => {
    if (term.hasSelection()) clearCellSelection(term);
  });
}
