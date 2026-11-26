// 组件模型（无框架）：组件 = 根元素 + 可选生命周期，由工厂 (ctx) => Component 创建。
// 约束：组件不读别的组件的 DOM、不互相 import；跨组件只经 ctx.store / ctx.actions，编排在 app.ts
import type { ClaudeResume } from "../claude-history";
import type { OpenFile } from "../preview";
import type { ClientMsg, SessionInfo, Target } from "../protocol";
import { applyDom } from "../i18n";
import type { MenuItem } from "./dialogs";
import type { Keymap } from "./keymap";
import type { AppState, Store } from "./store";

export interface Component {
  el: HTMLElement;
  /** 根元素放进槽位之后 */
  mount?(): void;
  unmount?(): void;
  /** 布局装配后告知是否显示（不在当前布局中为 false） */
  onVisible?(visible: boolean): void;
}

export interface Actions {
  activate(id: number): void;
  /** 聚焦当前终端；fit 时先按容器重新计算行列 */
  focusTerminal(fit?: boolean): void;
  /** command / resume 仅本地会话有效 */
  createAt(target: Target, command?: string, resume?: ClaudeResume): void;
  /** 先选目录再新建本地会话 */
  createLocal(): void;
  /** 把会话拖成独立窗口（屏幕坐标） */
  detach(id: number, x: number, y: number): void;
  openSettings(): void;
  openClaudeHistory(cwd: string | undefined): void;
  /** 提交历史面板 */
  openHistory(dir: string): void;
  /** 分支切换后：历史面板正显示 dir 时重新读取 */
  reloadHistory(dir: string): void;
  openPreview(f: OpenFile, edit?: boolean): Promise<void>;
  /** 关闭预览（有未保存修改时先确认），返回是否已关闭 */
  requestClosePreview(): Promise<boolean>;
  /** 向当前终端粘贴文字（不回车） */
  paste(text: string): void;
  /** 复制到系统剪贴板并提示 */
  copy(text: string): void;
  /** 从资源管理器拖到文件树以外（CSS 像素坐标） */
  dropOnTerminal(x: number, y: number, paths: string[]): void;
  /** 打开终端输出中的路径（失败静默） */
  openPath(s: SessionInfo, path: string): void;
  /** 在应用内预览文本文件（不是当前会话时先切过去） */
  previewFile(s: SessionInfo, path: string): void;
  /** 远程会话终端变为 root：文件栏提示同步提权 */
  suggestRoot(sessionId: number): void;
  setTermFont(size: number | null): void;
  setGpu(on: boolean): void;
  ask(message: string, ok: string, input?: string): Promise<string | null>;
  showMenu(x: number, y: number, items: MenuItem[]): void;
}

export interface Ctx {
  store: Store<AppState>;
  send: (msg: ClientMsg) => void;
  actions: Actions;
  keymap: Keymap;
}

export type Factory<C extends Component = Component> = (ctx: Ctx) => C;

/** 由 HTML 片段建元素（单个根），并套用 data-i18n 词条 */
export function html<T extends HTMLElement = HTMLElement>(markup: string): T {
  const tpl = document.createElement("template");
  tpl.innerHTML = markup.trim();
  applyDom(tpl.content);
  return tpl.content.firstElementChild as T;
}
