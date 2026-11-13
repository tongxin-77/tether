// Markdown 预览面板：覆盖在终端区域上方，关闭后回到原终端
import { renderMarkdown } from "./markdown";

interface Deps {
  /** 用默认浏览器打开 http(s) 链接（走 daemon） */
  openUrl: (url: string) => void;
  /** 预览关闭后（重新 fit 并聚焦当前终端） */
  onClose: () => void;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const panel = $("preview");
const nameEl = $("pv-name");
const bodyEl = $("pv-body");
const docEl = $("pv-doc");

let deps: Deps;
/** 当前预览文件的“用默认程序打开” */
let openDefault: (() => void) | undefined;

export function initPreview(d: Deps) {
  deps = d;
  $("pv-close").addEventListener("click", closePreview);
  $("pv-open").addEventListener("click", () => openDefault?.());
  document.addEventListener("keydown", (ev) => {
    // 有对话框打开时 Esc 交给对话框
    if (ev.key === "Escape" && !panel.hidden && !document.querySelector("dialog[open]")) closePreview();
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

export function showPreview(path: string, text: string, onOpenDefault: () => void) {
  openDefault = onOpenDefault;
  nameEl.textContent = path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
  nameEl.title = path;
  docEl.innerHTML = renderMarkdown(text);
  // 标题锚点（GitHub 风格 slug）记在 data 属性上，不写 id，避免 DOM clobbering
  const used = new Map<string, number>();
  for (const h of docEl.querySelectorAll<HTMLElement>("h1, h2, h3, h4, h5, h6")) {
    const base = slug(h.textContent ?? "");
    const n = used.get(base) ?? 0;
    used.set(base, n + 1);
    h.dataset.anchor = n ? `${base}-${n}` : base;
  }
  panel.hidden = false;
  bodyEl.scrollTop = 0;
  // 焦点移出终端，避免按键继续输入到被遮住的终端
  bodyEl.focus();
}

export function closePreview() {
  if (panel.hidden) return;
  panel.hidden = true;
  docEl.replaceChildren();
  openDefault = undefined;
  deps.onClose();
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
