// 预览面板的文本编辑器（CodeMirror 6）：由 preview.ts 在进入编辑时动态加载，语言包与 YAML 校验再按需加载
import { EditorState, type Extension, type Text } from "@codemirror/state";
import { EditorView, drawSelection, highlightActiveLine, highlightActiveLineGutter, highlightSpecialChars, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { HighlightStyle, StreamLanguage, bracketMatching, indentOnInput, syntaxHighlighting } from "@codemirror/language";
import { highlightSelectionMatches, search, searchKeymap } from "@codemirror/search";
import { lintGutter, linter } from "@codemirror/lint";
import { tags as t } from "@lezer/highlight";
import type { EditorLang } from "./preview-utils";
import type { YamlProblem } from "./yaml-check";

export interface Editor {
  view: EditorView;
  /** 内容与上次保存（或打开）时不同 */
  dirty(): boolean;
  /** 记下已保存的内容 */
  setSaved(doc: Text): void;
  /** 第一处 YAML 语法错误；非 YAML 或无错误为 undefined */
  yamlError(): YamlProblem | undefined;
}

// 配色全部引用应用主题的 CSS 变量，切换主题 / 亮暗即时生效
const theme = EditorView.theme({
  "&": { height: "100%", color: "var(--text)", backgroundColor: "transparent", fontSize: "calc(13px + var(--font-adj))" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--f-mono)", lineHeight: "1.6" },
  ".cm-content": { caretColor: "var(--accent)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "2px" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection": {
    backgroundColor: "var(--selection)",
  },
  ".cm-activeLine": { backgroundColor: "rgb(var(--surface-rgb) / 0.55)" },
  ".cm-gutters": { color: "var(--text-3)", backgroundColor: "transparent", border: "none" },
  ".cm-activeLineGutter": { color: "var(--text)", backgroundColor: "transparent" },
  ".cm-selectionMatch": { backgroundColor: "var(--accent-soft)" },
  "&.cm-focused .cm-matchingBracket": { color: "inherit", backgroundColor: "var(--accent-soft)", outline: "1px solid var(--accent-muted)" },
  ".cm-searchMatch": { backgroundColor: "var(--warning-soft)", outline: "1px solid var(--warning)" },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "var(--accent-soft)", outline: "1px solid var(--accent)" },
  ".cm-specialChar": { color: "var(--danger)" },
  ".cm-panels": { color: "var(--text)", backgroundColor: "rgb(var(--surface-rgb))" },
  ".cm-panels.cm-panels-bottom": { borderTop: "1px solid var(--border)" },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--border)" },
  ".cm-panel.cm-search": { padding: "6px 10px", fontFamily: "var(--f-ui)" },
  ".cm-panel.cm-search label": { fontSize: "inherit" },
  ".cm-textfield": {
    color: "var(--text)",
    backgroundColor: "rgb(var(--bg-rgb))",
    border: "1px solid var(--border)",
    borderRadius: "8px",
    padding: "2px 6px",
    font: "inherit",
  },
  ".cm-textfield:focus": { outline: "none", borderColor: "var(--accent)" },
  ".cm-button": {
    color: "var(--text)",
    backgroundImage: "none",
    backgroundColor: "rgb(var(--surface-hover-rgb))",
    border: "none",
    borderRadius: "99px",
    padding: "2px 10px",
    font: "inherit",
  },
  ".cm-panel button[name=close]": { color: "var(--text-2)" },
  ".cm-tooltip": { color: "var(--text)", backgroundColor: "var(--popover)", border: "1px solid var(--border)", borderRadius: "8px" },
  ".cm-diagnostic-error": { borderLeftColor: "var(--danger)" },
});

const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.controlKeyword, t.operatorKeyword, t.modifier, t.definitionKeyword], color: "var(--accent-strong)" },
  { tag: [t.propertyName, t.attributeName, t.definition(t.propertyName)], color: "var(--special)" },
  { tag: [t.string, t.special(t.string), t.regexp, t.character], color: "var(--success-text)" },
  { tag: [t.number, t.bool, t.null, t.atom, t.unit], color: "var(--warning-text)" },
  { tag: t.comment, color: "var(--text-3)", fontStyle: "italic" },
  { tag: [t.meta, t.labelName, t.typeName, t.tagName, t.processingInstruction, t.className], color: "var(--accent)" },
  { tag: [t.punctuation, t.separator, t.bracket, t.operator], color: "var(--text-2)" },
  { tag: t.heading, color: "var(--accent-strong)", fontWeight: "bold" },
  { tag: t.strong, fontWeight: "bold" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: [t.link, t.url], color: "var(--accent-strong)", textDecoration: "underline" },
  { tag: t.monospace, color: "var(--special)" },
  { tag: [t.quote, t.contentSeparator], color: "var(--text-2)" },
  { tag: t.invalid, color: "var(--danger)" },
]);

/** 查找替换面板等的界面文字 */
const phrases = EditorState.phrases.of({
  Find: "查找",
  Replace: "替换",
  next: "下一个",
  previous: "上一个",
  all: "全部",
  "match case": "区分大小写",
  "by word": "全字匹配",
  regexp: "正则",
  replace: "替换",
  "replace all": "全部替换",
  close: "关闭",
  "current match": "当前匹配",
  "replaced match on line $": "已替换第 $ 行的匹配",
  "replaced $ matches": "已替换 $ 处",
  "on line": "所在行",
  "Go to line": "跳到行",
  go: "跳转",
  Diagnostics: "问题",
  "No diagnostics": "没有问题",
});

/** 语言包与（YAML 的）校验；按需动态加载，各自打成独立的包 */
async function language(lang: EditorLang): Promise<{ ext: Extension; check?: (text: string) => YamlProblem[] }> {
  switch (lang) {
    case "yaml": {
      const [{ yaml }, { yamlProblems }] = await Promise.all([import("@codemirror/lang-yaml"), import("./yaml-check")]);
      const lint = linter((view) =>
        yamlProblems(view.state.doc.toString()).map((p) => ({ from: p.from, to: p.to, severity: "error" as const, message: p.message })),
      );
      return { ext: [yaml(), lint, lintGutter()], check: yamlProblems };
    }
    case "markdown":
      return { ext: [(await import("@codemirror/lang-markdown")).markdown(), EditorView.lineWrapping] };
    case "json":
      return { ext: (await import("@codemirror/lang-json")).json() };
    case "properties":
      return { ext: StreamLanguage.define((await import("@codemirror/legacy-modes/mode/properties")).properties) };
    case "toml":
      return { ext: StreamLanguage.define((await import("@codemirror/legacy-modes/mode/toml")).toml) };
    case "shell":
      return { ext: StreamLanguage.define((await import("@codemirror/legacy-modes/mode/shell")).shell) };
    default:
      // 纯文本（日志、txt 等）长行折行显示
      return { ext: EditorView.lineWrapping };
  }
}

/** 在 parent 中创建编辑器；change 在内容变化后调用，save 由 Ctrl+S 触发 */
export async function createEditor(parent: HTMLElement, text: string, lang: EditorLang, on: { change: () => void; save: () => void }): Promise<Editor> {
  const { ext, check } = await language(lang);
  let saved: Text | undefined;
  let isDirty = false;
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc: text,
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        highlightSpecialChars(),
        history(),
        drawSelection(),
        EditorState.allowMultipleSelections.of(true),
        indentOnInput(),
        bracketMatching(),
        highlightActiveLine(),
        highlightSelectionMatches(),
        search(),
        keymap.of([{ key: "Mod-s", run: () => (on.save(), true), preventDefault: true }, ...defaultKeymap, ...searchKeymap, ...historyKeymap, indentWithTab]),
        syntaxHighlighting(highlight),
        theme,
        phrases,
        ext,
        EditorView.updateListener.of((u) => {
          if (!u.docChanged) return;
          isDirty = !u.state.doc.eq(saved!);
          on.change();
        }),
      ],
    }),
  });
  saved = view.state.doc;
  return {
    view,
    dirty: () => isDirty,
    setSaved(doc) {
      saved = doc;
      isDirty = !view.state.doc.eq(doc);
    },
    yamlError: () => check?.(view.state.doc.toString())[0],
  };
}
