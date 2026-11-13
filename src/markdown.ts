// Markdown 渲染 + 净化：md 可能来自远程主机或他人，而 webview 能调用 Tauri invoke，输出必须经 DOMPurify 净化后才能插入页面
import { marked } from "marked";
import DOMPurify from "dompurify";

const PURIFY_CONFIG = {
  // 只保留 HTML（不允许 SVG / MathML）
  USE_PROFILES: { html: true },
  // 表单可提交导致 webview 跳转；style 可经 url() 外连；媒体 / 嵌入元素可加载外部资源
  FORBID_TAGS: ["style", "form", "button", "textarea", "select", "area", "video", "audio", "source", "track", "picture"],
  // 去掉所有可触发资源加载的属性（图片不加载：本地 / 远程相对图片避免任意读取，外链图片避免泄露隐私）
  FORBID_ATTR: ["style", "src", "srcset", "poster", "background", "action", "formaction", "target"],
};

DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  // 链接只保留 http(s) 与页内锚点，其余（相对路径、file:、javascript: 等）去掉 href
  if (node.tagName === "A") {
    const href = node.getAttribute("href")?.trim() ?? "";
    if (!/^(https?:|#)/i.test(href)) node.removeAttribute("href");
  }
  // 图片替换为 alt 文本占位（src 已被去掉，不会发起请求）
  if (node.tagName === "IMG") {
    const span = node.ownerDocument.createElement("span");
    span.className = "md-img";
    span.textContent = `[图片${node.getAttribute("alt") ? "：" + node.getAttribute("alt") : ""}]`;
    node.replaceWith(span);
  }
});

/** GFM（表格、任务列表、删除线）渲染为净化后的 HTML 字符串 */
export function renderMarkdown(text: string): string {
  const html = marked.parse(text, { async: false, gfm: true });
  return DOMPurify.sanitize(html, PURIFY_CONFIG);
}
