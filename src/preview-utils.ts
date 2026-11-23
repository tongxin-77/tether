// 文件预览的纯逻辑：可预览 / 编辑类型判定、编辑器语言、纯文本分块、Markdown 顶层块分组（便于单测）

/** 超过此大小的文件只读预览、不提供编辑 */
export const MAX_EDIT = 5 * 1024 * 1024;

/** 按文本打开的文件名（小写，精确匹配） */
const TEXT_NAMES = new Set([
  ".gitignore", ".gitattributes", ".gitmodules", ".dockerignore", ".editorconfig", ".npmrc", ".yarnrc", ".nvmrc",
  ".prettierrc", ".eslintrc", ".env", "dockerfile", "makefile",
]);
const TEXT_EXT = /\.(txt|log|ya?ml|ini|conf|cfg|properties|toml|jsonc?|xml|sh|bash|ps1|bat|cmd|csv)$/;
/** 模板副本后缀：按去掉后缀后的真实类型判定（如 config.yaml.example） */
const TEMPLATE = /\.(example|sample|template)$/;

/** 小写文件名，去掉模板后缀 */
function realName(path: string) {
  const name = path.replace(/[\\/]+$/, "").split(/[\\/]/).pop()!.toLowerCase();
  return { name: name.replace(TEMPLATE, ""), template: TEMPLATE.test(name) };
}

/**
 * 应用内打开的类型：md 渲染预览（可切换编辑），text 直接进编辑器（大文件只读）；其余返回 null（用默认程序打开）。
 * 模板副本（*.example / *.sample / *.template）总是按文本打开
 */
export function previewKind(path: string): "md" | "text" | null {
  const { name, template } = realName(path);
  if (/\.(md|markdown)$/.test(name)) return "md";
  if (template || TEXT_NAMES.has(name) || name.startsWith(".env.") || TEXT_EXT.test(name)) return "text";
  return null;
}

export type EditorLang = "markdown" | "yaml" | "json" | "properties" | "toml" | "shell" | "plain";

/** 编辑器的语法高亮语言（模板副本按去掉后缀后的类型） */
export function editorLang(path: string): EditorLang {
  const { name } = realName(path);
  if (/\.(md|markdown)$/.test(name)) return "markdown";
  if (/\.ya?ml$/.test(name)) return "yaml";
  if (/\.jsonc?$/.test(name)) return "json";
  if (/\.toml$/.test(name)) return "toml";
  if (/\.(sh|bash)$/.test(name)) return "shell";
  if (/\.(ini|conf|cfg|properties)$/.test(name) || /^\.(env(\..*)?|editorconfig|npmrc|yarnrc|gitmodules)$/.test(name) || name === "env") {
    return "properties";
  }
  return "plain";
}

/** 不可编辑的原因（只读预览）；可编辑返回 undefined。encoding 为 null 表示解码时有非法字节被替换 */
export function editBlock(size: number, encoding: string | null): string | undefined {
  if (size > MAX_EDIT) return `文件超过 ${MAX_EDIT / 1024 / 1024} MB，只读预览`;
  if (encoding === null) return "文件含无法识别的字节，为免保存时损坏，只读预览";
  return undefined;
}

export interface TextChunk {
  text: string;
  /** 块内行数（用于估算未渲染时的高度） */
  lines: number;
}

/**
 * 纯文本按行分块：每块最多 maxLines 行或约 maxChars 字符；超长单行切成多块（不拆开代理对）。
 * 兼容 CRLF；末尾换行不产生多余空行
 */
export function chunkLines(text: string, maxLines = 200, maxChars = 32 * 1024): TextChunk[] {
  const out: TextChunk[] = [];
  let buf: string[] = [];
  let size = 0;
  const flush = () => {
    if (buf.length) out.push({ text: buf.join("\n"), lines: buf.length });
    buf = [];
    size = 0;
  };
  const lines = text.split(/\r?\n/);
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  for (let line of lines) {
    while (line.length > maxChars) {
      flush();
      let end = maxChars;
      const c = line.charCodeAt(end - 1);
      if (c >= 0xd800 && c <= 0xdbff) end--;
      out.push({ text: line.slice(0, end), lines: 1 });
      line = line.slice(end);
    }
    buf.push(line);
    size += line.length + 1;
    if (buf.length >= maxLines || size >= maxChars) flush();
  }
  flush();
  return out;
}

const VOID_TAG = /^(area|base|br|col|embed|hr|img|input|link|meta|source|track|wbr)$/i;

/** HTML 片段中未闭合标签数的粗略估计（开标签 +1、闭标签 -1，忽略空元素与自闭合） */
export function htmlDepth(raw: string): number {
  let d = 0;
  for (const m of raw.matchAll(/<(\/?)([a-z][\w-]*)\b[^>]*?(\/?)>/gi)) {
    if (m[1]) d--;
    else if (!m[3] && !VOID_TAG.test(m[2])) d++;
  }
  return d;
}

/**
 * Markdown 顶层 token 按原文长度分组（每组约 maxChars）；块级 HTML 未闭合时（如 <details> 跨多个块）不切开，
 * 避免各组单独净化时标签被拆散
 */
export function groupBlocks<T extends { type: string; raw: string }>(tokens: T[], maxChars = 16 * 1024): T[][] {
  const out: T[][] = [];
  let cur: T[] = [];
  let size = 0;
  let depth = 0;
  for (const t of tokens) {
    cur.push(t);
    size += t.raw.length;
    if (t.type === "html") depth = Math.max(0, depth + htmlDepth(t.raw));
    if (size >= maxChars && depth === 0) {
      out.push(cur);
      cur = [];
      size = 0;
    }
  }
  if (cur.length) out.push(cur);
  return out;
}
