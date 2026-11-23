// 文件预览 / 编辑面板：覆盖在终端区域上方，关闭后回到原终端。Markdown 默认渲染预览、可切到源码编辑；
// 其余文本类文件直接进编辑器（CodeMirror，首次编辑时才加载），过大或编码无法识别时只读预览
// 大文件分段：每段一个 .pv-sec（content-visibility: auto），屏外段落跳过布局与绘制；DOM 仍完整，原生查找与复制不受影响
import type { Editor } from "./editor";
import { renderMarkdownSections } from "./markdown";
import { chunkLines, type EditorLang } from "./preview-utils";

interface Deps {
  /** 用默认浏览器打开 http(s) 链接（走 daemon） */
  openUrl: (url: string) => void;
  /** 预览关闭后（重新 fit 并聚焦当前终端） */
  onClose: () => void;
  /** 确认框：确定返回 ""，取消返回 null（files.ts 的 ask） */
  ask: (message: string, ok: string) => Promise<string | null>;
}

/** 面板中打开的文件；读写细节（会话、编码、mtime）由 files.ts 的闭包持有 */
export interface OpenFile {
  path: string;
  text: string;
  /** md：渲染预览，可切换编辑；text：直接编辑 */
  kind: "md" | "text";
  lang: EditorLang;
  /** 不可编辑的原因（只读预览） */
  readonly?: string;
  openDefault: () => void;
  /** 写回；force 时不检测外部修改 */
  save: (text: string, force: boolean) => Promise<{ conflict?: boolean; error?: string }>;
  /** 重新读取并打开（edit 为 true 时直接进编辑） */
  reload: (edit: boolean) => void;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const panel = $("preview");
const nameEl = $("pv-name");
const noteEl = $("pv-note");
const editBtn = $<HTMLButtonElement>("pv-edit");
const saveBtn = $<HTMLButtonElement>("pv-save");
const cancelBtn = $<HTMLButtonElement>("pv-cancel");
const bodyEl = $("pv-body");
const docEl = $("pv-doc");
const editorEl = $("pv-editor");
const conflictDlg = $<HTMLDialogElement>("pv-conflict");

let deps: Deps;
let file: OpenFile | undefined;
let editor: Editor | undefined;
let saving = false;

export function initPreview(d: Deps) {
  deps = d;
  $("pv-close").addEventListener("click", () => void requestClosePreview());
  $("pv-open").addEventListener("click", () => file?.openDefault());
  editBtn.addEventListener("click", () => void startEdit());
  saveBtn.addEventListener("click", () => void save());
  cancelBtn.addEventListener("click", () => void cancelEdit());
  document.addEventListener("keydown", (ev) => {
    // 有对话框打开时 Esc 交给对话框；编辑器已处理的 Esc（关闭查找框、取消多选等）不再关闭面板
    if (ev.key === "Escape" && !panel.hidden && !ev.defaultPrevented && !document.querySelector("dialog[open]")) void requestClosePreview();
  });
  // 链接一律不在 webview 内导航：锚点在预览内滚动，http(s) 交给默认浏览器，其它（已被净化去掉 href）忽略
  docEl.addEventListener("click", (ev) => {
    const a = (ev.target as Element).closest("a");
    if (!a) return;
    ev.preventDefault();
    const href = a.getAttribute("href");
    if (!href) return;
    if (href.startsWith("#")) scrollToAnchor(href.slice(1));
    else deps.openUrl(a.href);
  });
  // 中键 / Ctrl+点击也不能让 webview 打开新窗口
  docEl.addEventListener("auxclick", (ev) => {
    if ((ev.target as Element).closest("a")) ev.preventDefault();
  });
}

/** 打开文件：text 类默认直接编辑；当前有未保存的修改时先确认 */
export async function showPreview(f: OpenFile, edit = f.kind === "text") {
  if (!(await confirmDiscard())) return;
  dropEditor();
  docEl.replaceChildren();
  file = f;
  nameEl.title = f.path;
  noteEl.hidden = !f.readonly;
  noteEl.textContent = f.readonly ?? "";
  panel.hidden = false;
  setMode(false);
  bodyEl.focus();
  if (edit && !f.readonly) await startEdit();
  else renderView();
}

/** 有未保存的修改 */
export function previewDirty() {
  return !panel.hidden && !!editor?.dirty();
}

/** 关闭面板（有未保存的修改时先确认）；返回是否已关闭 */
export async function requestClosePreview() {
  if (!(await confirmDiscard())) return false;
  closePreview();
  return true;
}

/** 直接关闭（丢弃未保存的修改，如会话已关闭） */
export function closePreview() {
  if (panel.hidden) return;
  panel.hidden = true;
  dropEditor();
  docEl.replaceChildren();
  file = undefined;
  deps.onClose();
}

async function confirmDiscard() {
  if (!previewDirty()) return true;
  return (await deps.ask(`“${baseName(file!.path)}”有未保存的修改，确定放弃？`, "放弃修改")) !== null;
}

/** 渲染预览（Markdown）或只读纯文本 */
function renderView() {
  const f = file!;
  const markdown = f.kind === "md";
  docEl.classList.toggle("plain", !markdown);
  // 段数可能上万，逐个 append 到 fragment（避免展开参数过多）
  const frag = document.createDocumentFragment();
  if (markdown) {
    for (const { html, lines } of renderMarkdownSections(f.text)) section(frag, lines).innerHTML = html;
  } else {
    // 块末为空行时补一个换行，否则该空行在块边界处不占高度
    for (const c of chunkLines(f.text)) section(frag, c.lines).textContent = c.text.endsWith("\n") || !c.text ? c.text + "\n" : c.text;
  }
  docEl.replaceChildren(frag);
  // 标题锚点（GitHub 风格 slug）记在 data 属性上，不写 id，避免 DOM clobbering
  const used = new Map<string, number>();
  for (const h of docEl.querySelectorAll<HTMLElement>("h1, h2, h3, h4, h5, h6")) {
    const base = slug(h.textContent ?? "");
    const n = used.get(base) ?? 0;
    used.set(base, n + 1);
    h.dataset.anchor = n ? `${base}-${n}` : base;
  }
  setMode(false);
  bodyEl.scrollTop = 0;
  // 焦点移出终端，避免按键继续输入到被遮住的终端
  bodyEl.focus();
}

/** 进入编辑（首次加载 CodeMirror 与语言包） */
async function startEdit() {
  const f = file!;
  if (editor || f.readonly) return;
  const { createEditor } = await import("./editor");
  const ed = await createEditor(editorEl, f.text, f.lang, { change: updateTitle, save: () => void save() });
  // 加载期间已换了文件或重复点击：丢弃这个
  if (file !== f || editor) return ed.view.destroy();
  editor = ed;
  docEl.replaceChildren();
  setMode(true);
  editor.view.focus();
}

/** Markdown 编辑中“取消”：放弃修改回到预览 */
async function cancelEdit() {
  if (!(await confirmDiscard())) return;
  dropEditor();
  renderView();
}

async function save(force = false) {
  const f = file;
  const ed = editor;
  if (!f || !ed || saving) return;
  if (!force) {
    const e = ed.yamlError();
    if (e && (await deps.ask(`YAML 有语法错误（第 ${e.line} 行：${e.message}），仍要保存？`, "仍要保存")) === null) return;
  }
  const doc = ed.view.state.doc;
  const text = doc.toString();
  saving = true;
  saveBtn.disabled = true;
  const r = await f.save(text, force).finally(() => (saving = false));
  if (file !== f || editor !== ed) return;
  updateTitle();
  if (r.error) return alert(`保存失败：${r.error}`);
  if (r.conflict) {
    const choice = await askConflict(baseName(f.path));
    if (choice === "overwrite") return save(true);
    if (choice === "reload") {
      ed.setSaved(ed.view.state.doc);
      f.reload(true);
    }
    return;
  }
  f.text = text;
  ed.setSaved(doc);
  updateTitle();
  // Markdown 保存后回到预览（按新内容渲染）；保存期间又有输入则留在编辑
  if (f.kind === "md" && !ed.dirty()) {
    dropEditor();
    renderView();
  }
}

/** 文件已被外部修改：覆盖 / 重新加载 / 取消 */
function askConflict(name: string): Promise<"overwrite" | "reload" | ""> {
  $("pv-c-msg").textContent = `“${name}”已在别处被修改（或已删除）。`;
  conflictDlg.returnValue = "";
  conflictDlg.showModal();
  return new Promise((resolve) =>
    conflictDlg.addEventListener("close", () => resolve(conflictDlg.returnValue as "overwrite" | "reload" | ""), { once: true }),
  );
}

function setMode(editing: boolean) {
  bodyEl.hidden = editing;
  editorEl.hidden = !editing;
  editBtn.hidden = editing || file?.kind !== "md" || !!file.readonly;
  saveBtn.hidden = !editing;
  cancelBtn.hidden = !editing || file?.kind !== "md";
  updateTitle();
}

/** 标题：未保存时名字前加 •，保存按钮仅在有修改时可用 */
function updateTitle() {
  const dirty = !!editor?.dirty();
  nameEl.textContent = (dirty ? "• " : "") + (file ? baseName(file.path) : "");
  saveBtn.disabled = !dirty || saving;
}

function dropEditor() {
  editor?.view.destroy();
  editor = undefined;
  editorEl.replaceChildren();
}

function baseName(path: string) {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

/** 追加一个分段；未渲染时按行数估算高度（渲染过后浏览器记住实际高度） */
function section(parent: DocumentFragment, lines: number) {
  const el = document.createElement("div");
  el.className = "pv-sec";
  el.style.containIntrinsicSize = `auto ${(lines * 1.7).toFixed(1)}em`;
  parent.append(el);
  return el;
}

function slug(s: string) {
  return s
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s/g, "-");
}

function scrollToAnchor(raw: string) {
  let id = raw;
  try {
    id = decodeURIComponent(raw);
  } catch {}
  const q = CSS.escape(id);
  docEl.querySelector(`[data-anchor="${q}"], [id="${q}"], [name="${q}"]`)?.scrollIntoView();
}
