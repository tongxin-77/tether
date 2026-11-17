// 更新说明：latest.json 的 notes 含多个版本段（`## [x.y.z] - 日期`，新版在前），只显示比当前版本新的段落

const HEAD = /^## \[(\d+(?:\.\d+)*)\]/;

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
