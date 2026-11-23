// YAML 语法检查（编辑器的错误标记与保存前确认）；只在编辑 YAML 时由 editor.ts 动态加载
import { parseAllDocuments } from "yaml";

export interface YamlProblem {
  /** 文本中的字符偏移（UTF-16） */
  from: number;
  to: number;
  /** 行号，从 1 开始 */
  line: number;
  message: string;
}

/** 所有文档的语法错误，按位置排序 */
export function yamlProblems(text: string): YamlProblem[] {
  const out: YamlProblem[] = [];
  for (const doc of parseAllDocuments(text, { prettyErrors: false })) {
    for (const e of doc.errors) {
      const from = Math.min(e.pos[0], text.length);
      const to = Math.min(Math.max(e.pos[1], from), text.length);
      out.push({ from, to, line: lineAt(text, from), message: e.message.split("\n")[0] });
    }
  }
  return out.sort((a, b) => a.from - b.from);
}

function lineAt(text: string, pos: number) {
  let n = 1;
  for (let i = text.indexOf("\n"); i !== -1 && i < pos; i = text.indexOf("\n", i + 1)) n++;
  return n;
}
