// 终端的复制粘贴与 Claude 输入框全选（Ctrl+A）按键处理
import type { Terminal } from "@xterm/xterm";
import { invoke } from "@tauri-apps/api/core";
// WebView2 中 navigator.clipboard.readText 会弹权限询问，统一走剪贴板插件
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import type { ClaudeState, SessionInfo } from "./protocol";
import { cellSelectionText, clearCellSelection } from "./table-select";
import { claudeInputRange, pastedPath } from "./term-utils";

/** 复制选区（优先表格单元格选区）到系统剪贴板，成功后 onCopied；无选区返回 false */
export function copySelection(term: Terminal, onCopied: () => void) {
  const text = cellSelectionText(term) || term.getSelection();
  if (!text) return false;
  writeText(text).then(onCopied, (err) => console.error("Copy failed", err));
  return true;
}

/** 粘贴剪贴板文本：走 term.paste，开启 bracketed paste 的程序（PSReadLine / Claude Code 等）不会把多行逐行执行。
 *  noText：剪贴板没有文本（为空或是图片）时调用，Ctrl+V 据此把图片粘贴键发给会话（Claude Code 收到后自行读取剪贴板图片） */
export async function pasteInto(term: Terminal, noText?: () => void) {
  let text = "";
  try {
    text = await readText();
  } catch {
    // 剪贴板为空或不是文本
  }
  if (text) term.paste(text);
  else noText?.();
  term.focus();
}

/** Ctrl+V 剪贴板无文本时粘贴图片：本地会话中 Claude Code 运行时由 Tether 把剪贴板图片存为文件、粘贴其路径
 *  （Claude Code 识别为 [Image #N]），比它自己经 PowerShell 读剪贴板快数秒；无图片、出错及其它情况发图片粘贴键由程序自己读。
 *  结果写回发起时的终端（term 与 s.id），期间切换会话不影响 */
export async function pasteImage(term: Terminal, s: SessionInfo, sendInput: (data: string) => void) {
  if (s.host === null && s.claude) {
    try {
      const path = await invoke<string | null>("clipboard_image_file");
      if (path) return term.paste(pastedPath(path));
    } catch (e) {
      console.warn("Failed to paste clipboard image, letting Claude Code read it", e);
    }
  }
  // Windows 版 Claude Code 的图片粘贴键是 Alt+V（ESC v），^V 未绑定；远程会话（Linux / macOS）仍为 ^V
  sendInput(s.host !== null ? "\x16" : "\x1bv");
}

/** 复制粘贴快捷键（类 Windows Terminal）：有选区时 Ctrl+C 复制而不发送 ^C；Ctrl+Shift+C 复制；Ctrl+V / Ctrl+Shift+V 粘贴。
 *  返回 false 表示 xterm 不再处理该键；粘贴键在 keydown 上 preventDefault，阻止浏览器原生 paste 事件，避免 xterm 再粘贴一次 */
export function clipboardKey(term: Terminal, ev: KeyboardEvent, sendImagePaste: () => void, onCopied: () => void) {
  if (!ev.ctrlKey || ev.altKey || ev.metaKey) return true;
  if (ev.code === "KeyC") {
    if (!ev.shiftKey && !term.hasSelection() && !cellSelectionText(term)) return true;
    ev.preventDefault();
    if (ev.type === "keydown" && copySelection(term, onCopied)) {
      term.clearSelection();
      clearCellSelection(term);
    }
    return false;
  }
  if (ev.code === "KeyV") {
    ev.preventDefault();
    // 仅 Ctrl+V（不含 Ctrl+Shift+V）在剪贴板无文本时发送图片粘贴键，触发 Claude Code 的图片粘贴
    if (ev.type === "keydown") pasteInto(term, ev.shiftKey ? undefined : sendImagePaste);
    return false;
  }
  return true;
}

/** claude 会话中的 Ctrl+A：不发 ^A，改为高亮屏幕上 Claude Code 输入框的文字（Ctrl+C 走 clipboardKey 复制）；
 *  高亮仍是 Ctrl+A 产生的时按 Backspace / Delete 清空选区，idle / done 状态下再发两次 Esc 清空输入框并吞掉该键
 *  （thinking 时 Esc 会中断任务、waiting 时会拒绝权限请求，故其它状态只取消高亮、按键照常透传）。返回 false 表示 xterm 不再处理该键 */
export function attachClaudeSelectAll(term: Terminal, claude: () => ClaudeState | undefined, sendInput: (data: string) => void) {
  let ours = false;
  term.onSelectionChange(() => (ours = false));
  return (ev: KeyboardEvent) => {
    const state = claude()?.state;
    if (!state) return true;
    const mods = ev.ctrlKey || ev.altKey || ev.metaKey;
    if (ev.code === "KeyA" && ev.ctrlKey && !ev.shiftKey && !ev.altKey && !ev.metaKey) {
      ev.preventDefault();
      if (ev.type === "keydown") ours = selectClaudeInput(term);
      return false;
    }
    if (ev.type !== "keydown" || ["Control", "Shift", "Alt", "Meta"].includes(ev.key)) return true;
    if (ours && !mods && (ev.key === "Backspace" || ev.key === "Delete") && term.hasSelection()) {
      term.clearSelection();
      if (state === "idle" || state === "done") {
        ev.preventDefault();
        sendInput("\x1b");
        setTimeout(() => sendInput("\x1b"), 50);
        return false;
      }
    }
    ours = false;
    return true;
  };
}

/** 高亮可视区内 Claude Code 输入框的文字；找不到、输入为空或只有占位提示（暗色文字）时返回 false */
function selectClaudeInput(term: Terminal) {
  const buf = term.buffer.active;
  const top = buf.viewportY;
  // 每个单元格一个字符：宽字符的占位单元格、多码元字符用非空白字符占位，保证下标即列号
  const lines: string[] = [];
  for (let y = 0; y < term.rows; y++) {
    const line = buf.getLine(top + y);
    let s = "";
    for (let x = 0; x < term.cols; x++) {
      const cell = line?.getCell(x);
      const c = cell?.getChars() ?? "";
      s += cell && cell.getWidth() === 0 ? "_" : c.length === 1 ? c : c ? "_" : " ";
    }
    lines.push(s);
  }
  const r = claudeInputRange(lines);
  // 输入框为空时 Claude Code 显示暗色占位提示（单行），不当作输入
  if (!r || (r.endRow === r.startRow && buf.getLine(top + r.endRow)?.getCell(r.endCol - 1)?.isDim())) return false;
  term.select(r.startCol, top + r.startRow, (r.endRow - r.startRow) * term.cols + r.endCol - r.startCol);
  return true;
}
