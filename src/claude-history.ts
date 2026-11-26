// 「恢复 Claude 对话」弹窗：列出某目录的 Claude Code 历史对话（src-tauri 只读 ~/.claude/projects），
// 点击在该目录新建本地会话并 `claude --resume`（daemon 带上原权限模式）；已在会话中打开的直接切换过去
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { onLocaleChange, t, tn } from "./i18n";

/** 与 src-tauri claude_history 命令的返回一致 */
interface Conversation {
  id: string;
  title: string;
  /** 文件修改时间，Unix 毫秒 */
  mtime: number;
  messages: number;
  permissionMode: string | null;
  bypass: boolean;
}

/** 与 proto ClaudeResume 一致 */
export interface ClaudeResume {
  id: string;
  permissionMode: string | null;
  bypass: boolean;
}

interface Deps {
  /** 已打开该对话（未退出）的会话 id */
  openedIn: (claudeId: string) => number | undefined;
  activate: (id: number) => void;
  /** 在 cwd 新建本地会话并恢复对话 */
  resume: (cwd: string, r: ClaudeResume) => void;
  relativeTime: (ms: number) => string;
  /** 文件栏当前浏览目录（仅本地会话） */
  filesDir: () => string | undefined;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
let deps: Deps;
let dir = "";
let items: Conversation[] = [];
/** 加载中，或读取失败的错误信息 */
let loadState: "loading" | { error: string } | undefined;
/** 只认最近一次请求的结果 */
let seq = 0;

export function initClaudeHistory(d: Deps) {
  deps = d;
  $("cch-filter").addEventListener("input", render);
  // 输入框里回车不提交表单（会关闭弹窗）
  $("cch-filter").addEventListener("keydown", (ev) => ev.key === "Enter" && ev.preventDefault());
  $("cch-files").addEventListener("click", () => {
    const d = deps.filesDir();
    if (d) load(d);
  });
  onLocaleChange(() => $<HTMLDialogElement>("cc-history").open && render());
}

/** 打开弹窗；没有目录（当前不是本地会话）时先选择文件夹 */
export async function openClaudeHistory(cwd: string | undefined) {
  if (!cwd) {
    const d = await open({ directory: true, title: t("history.pickDir") });
    if (typeof d !== "string") return;
    cwd = d;
  }
  $<HTMLInputElement>("cch-filter").value = "";
  $<HTMLDialogElement>("cc-history").showModal();
  $("cch-filter").focus();
  load(cwd);
}

async function load(cwd: string) {
  dir = cwd;
  const my = ++seq;
  items = [];
  loadState = "loading";
  render();
  const r = await invoke<Conversation[]>("claude_history", { dir: cwd }).then(
    (list) => ({ list }),
    (e) => ({ error: String(e) }),
  );
  if (my !== seq) return;
  if ("list" in r) [items, loadState] = [r.list, undefined];
  else loadState = r;
  render();
}

function render() {
  $("cch-dir").textContent = dir;
  $("cch-dir").title = dir;
  const fd = deps.filesDir();
  $("cch-files").hidden = !fd || fd.toLowerCase() === dir.toLowerCase();
  const q = $<HTMLInputElement>("cch-filter").value.trim().toLowerCase();
  const shown = loadState ? [] : items.filter((c) => c.title.toLowerCase().includes(q));
  const empty = $("cch-empty");
  empty.hidden = shown.length > 0;
  empty.textContent =
    loadState === "loading"
      ? t("common.loading")
      : loadState
        ? t("history.readFailed", { error: loadState.error })
        : items.length
          ? t("history.noMatch")
          : t("history.none");
  $("cch-list").replaceChildren(...shown.map(row));
}

function row(c: Conversation) {
  const li = document.createElement("li");
  const opened = deps.openedIn(c.id);
  li.innerHTML = `<span class="cch-title"></span><span class="cch-meta"><span></span></span>`;
  li.querySelector(".cch-title")!.textContent = c.title || t("history.untitled");
  li.title = c.title;
  li.querySelector(".cch-meta span")!.textContent = `${deps.relativeTime(c.mtime)} · ${tn("history.messages", c.messages)}`;
  if (opened !== undefined) {
    const tag = li.querySelector(".cch-meta")!.appendChild(document.createElement("span"));
    tag.className = "tag open";
    tag.textContent = t("history.opened");
  }
  li.addEventListener("click", () => {
    $<HTMLDialogElement>("cc-history").close();
    if (opened !== undefined) deps.activate(opened);
    else deps.resume(dir, { id: c.id, permissionMode: c.permissionMode, bypass: c.bypass });
  });
  return li;
}
