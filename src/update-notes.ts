// 更新说明：latest.json 的 notes 含多个版本段（`## [x.y.z] - 日期`，新版在前），只显示比当前版本新的段落。
// 新格式按语言分段：`<!-- lang:zh-CN -->` 与 `<!-- lang:en -->` 标记各自开始一段，客户端按界面语言选段

const HEAD = /^## \[(\d+(?:\.\d+)*)\]/;
const LANG_MARK = /^\s*<!--\s*lang:([\w-]+)\s*-->\s*$/;

/** 版本号按数字逐段比较：a > b 时为正 */
function compare(a: string, b: string) {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d) return d;
  }
  return 0;
}

/** 按界面语言选段：zh-CN / zh-TW → 中文段，en / ja 等 → 英文段；所需段缺失时取另一种，没有标记（旧格式）时整段返回 */
export function notesForLocale(body: string, locale: string): string {
  const sections = new Map<string, string[]>();
  let cur: string[] | undefined;
  for (const line of body.split(/\r?\n/)) {
    const m = LANG_MARK.exec(line);
    if (m) {
      cur = [];
      sections.set(m[1], cur);
    } else cur?.push(line);
  }
  if (!sections.size) return body;
  const want = locale.startsWith("zh") ? ["zh-CN", "en"] : ["en", "zh-CN"];
  const key = want.find((k) => sections.has(k)) ?? [...sections.keys()][0];
  return sections.get(key)!.join("\n").trim();
}

/** 截取比 current 新的版本段；没有版本标题（旧格式）或没有更新的段落时原样返回 */
export function notesSince(body: string, current: string): string {
  const lines = body.split(/\r?\n/);
  const kept: string[] = [];
  let keep = false;
  let sections = 0;
  for (const line of lines) {
    const m = HEAD.exec(line);
    if (m) {
      keep = compare(m[1], current) > 0;
      if (keep) sections++;
    }
    if (keep) kept.push(line);
  }
  return sections ? kept.join("\n").trim() : body;
}
