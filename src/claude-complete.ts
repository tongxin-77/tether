// `claude` 命令补全：光标前的当前行以 `claude <部分参数>` 结尾时，在光标附近弹出候选（子命令 / 参数 / 部分参数值）。
// 纯前端，本地 / 远程会话通用。只读光标所在的这一物理行（命令折行后不识别）。
import type { Terminal } from "@xterm/xterm";

interface Item {
  text: string;
  desc: string;
}

/** 弹出的候选：key 为使用计数键，partial 为接受时要替换掉的已输入文本 */
interface Candidate extends Item {
  key: string;
  partial: string;
}

// 候选取自 Claude Code 2.1.322 的 `claude --help`（精选常用项）
const SUBCOMMANDS: Item[] = [
  ["agents", "管理后台会话"],
  ["attach", "在此终端打开后台会话"],
  ["auth", "管理登录认证"],
  ["auto-mode", "查看 / 重置 auto 模式配置"],
  ["doctor", "检查安装健康状况"],
  ["import", "从其它 AI 编程工具导入配置"],
  ["install", "安装原生版本"],
  ["logs", "打印后台会话的近期输出"],
  ["mcp", "配置和管理 MCP 服务器"],
  ["plugin", "管理插件"],
  ["purge", "删除项目的本地 Claude Code 数据"],
  ["respawn", "以当前版本重启后台会话"],
  ["rm", "删除后台会话"],
  ["setup-token", "设置长期认证令牌"],
  ["stop", "停止后台会话"],
  ["ultrareview", "云端多智能体代码评审"],
  ["update", "检查并安装更新"],
].map(([text, desc]) => ({ text, desc }));

const FLAGS: Item[] = [
  ["--continue", "继续当前目录最近的对话"],
  ["-c", "--continue：继续最近的对话"],
  ["--resume", "按会话 ID / 名称恢复，或打开选择器"],
  ["-r", "--resume：恢复对话"],
  ["--print", "输出回复后退出（非交互）"],
  ["-p", "--print：非交互输出"],
  ["--model", "本会话使用的模型"],
  ["--permission-mode", "权限模式"],
  ["--effort", "推理强度"],
  ["--add-dir", "额外允许访问的目录"],
  ["--dangerously-skip-permissions", "跳过所有权限检查（仅限沙箱）"],
  ["--allow-dangerously-skip-permissions", "允许切换到跳过权限检查"],
  ["--name", "会话显示名称"],
  ["-n", "--name：会话显示名称"],
  ["--worktree", "为本会话新建 git worktree"],
  ["-w", "--worktree：新建 git worktree"],
  ["--fork-session", "恢复时创建新的会话 ID"],
  ["--session-id", "指定会话 ID（UUID）"],
  ["--agent", "本会话使用的 agent"],
  ["--allowedTools", "允许的工具列表"],
  ["--disallowedTools", "禁止的工具列表"],
  ["--tools", "可用的内置工具"],
  ["--append-system-prompt", "追加系统提示"],
  ["--system-prompt", "替换系统提示"],
  ["--mcp-config", "从 JSON 文件 / 字符串加载 MCP 服务器"],
  ["--settings", "额外的设置文件或 JSON"],
  ["--output-format", "输出格式（配合 --print）"],
  ["--fallback-model", "过载时的备用模型"],
  ["--background", "在后台启动会话"],
  ["--bare", "最小模式（跳过 hooks、插件等）"],
  ["--safe-mode", "禁用全部自定义配置启动"],
  ["--verbose", "详细输出"],
  ["--debug", "调试模式"],
  ["--ide", "启动时自动连接 IDE"],
  ["--chrome", "启用 Claude in Chrome"],
  ["--version", "显示版本号"],
  ["-v", "--version：显示版本号"],
  ["--help", "显示帮助"],
  ["-h", "--help：显示帮助"],
].map(([text, desc]) => ({ text, desc }));

/** 取值固定的参数：上一个词为该参数时补全取值 */
const VALUES: Record<string, Item[]> = {
  "--model": ["fable", "opus", "sonnet"].map((text) => ({ text, desc: "模型别名" })),
  "--permission-mode": ["acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan"].map((text) => ({ text, desc: "权限模式" })),
  "--effort": ["low", "medium", "high", "xhigh", "max"].map((text) => ({ text, desc: "推理强度" })),
  "--output-format": ["text", "json", "stream-json"].map((text) => ({ text, desc: "输出格式" })),
};

/** 会话 shell 类型：决定内置常用命令列表 */
export type ShellKind = "powershell" | "cmd" | "posix";

const toItems = (list: string[][]): Item[] => list.map(([text, desc]) => ({ text, desc }));

/** 各 shell 内置常用命令（按完整命令前缀匹配，同自定义命令） */
const COMMANDS: Record<ShellKind | "shared", Item[]> = {
  powershell: toItems([
    ["Get-ChildItem", "列出目录内容"],
    ["Set-Location", "切换目录"],
    ["Get-Location", "显示当前目录"],
    ["Get-Content", "读取文件内容"],
    ["Get-Content -Wait -Tail 50", "持续查看文件末尾"],
    ["Set-Content", "写入文件内容"],
    ["Select-String", "按模式搜索文本"],
    ["Remove-Item", "删除文件 / 目录"],
    ["Remove-Item -Recurse -Force", "强制递归删除"],
    ["Copy-Item", "复制文件 / 目录"],
    ["Move-Item", "移动 / 重命名"],
    ["New-Item -ItemType Directory", "新建目录"],
    ["New-Item -ItemType File", "新建文件"],
    ["Rename-Item", "重命名"],
    ["Test-Path", "检查路径是否存在"],
    ["Get-Process", "列出进程"],
    ["Stop-Process -Name", "按名称结束进程"],
    ["Get-Service", "列出服务"],
    ["Get-Command", "查找命令"],
    ["Get-Help", "查看帮助"],
    ["Invoke-WebRequest", "发送 HTTP 请求"],
    ["Get-NetTCPConnection", "查看 TCP 连接"],
    ["$env:PATH", "环境变量 PATH"],
    ["ls", "Get-ChildItem 别名"],
    ["cd ..", "返回上级目录"],
    ["cat", "Get-Content 别名"],
    ["rm", "Remove-Item 别名"],
    ["cp", "Copy-Item 别名"],
    ["mv", "Move-Item 别名"],
    ["mkdir", "新建目录"],
    ["pwd", "显示当前目录"],
    ["cls", "清屏"],
    ["where.exe", "查找可执行文件位置"],
  ]),
  cmd: toItems([
    ["dir", "列出目录内容"],
    ["dir /s /b", "递归列出完整路径"],
    ["cd", "切换 / 显示目录"],
    ["cd ..", "返回上级目录"],
    ["cd /d", "切换驱动器与目录"],
    ["copy", "复制文件"],
    ["xcopy /e /i", "递归复制目录"],
    ["robocopy", "可靠复制目录"],
    ["del", "删除文件"],
    ["move", "移动 / 重命名"],
    ["ren", "重命名"],
    ["mkdir", "新建目录"],
    ["rmdir /s /q", "删除目录及内容"],
    ["type", "显示文件内容"],
    ["findstr", "搜索文本"],
    ["set", "查看 / 设置环境变量"],
    ["where", "查找可执行文件位置"],
    ["tasklist", "列出进程"],
    ["taskkill /f /im", "按映像名结束进程"],
    ["netstat -ano", "查看端口占用"],
    ["ipconfig", "查看网络配置"],
    ["echo", "输出文本"],
    ["cls", "清屏"],
    ["exit", "退出"],
  ]),
  posix: toItems([
    ["ls -la", "列出目录（含隐藏、详细）"],
    ["cd ..", "返回上级目录"],
    ["pwd", "显示当前目录"],
    ["cat", "显示文件内容"],
    ["less", "分页查看文件"],
    ["tail -f", "持续查看文件末尾"],
    ["head", "显示文件开头"],
    ["grep -rn", "递归搜索文本"],
    ["find . -name", "按名称查找文件"],
    ["cp -r", "递归复制"],
    ["mv", "移动 / 重命名"],
    ["rm -rf", "强制递归删除"],
    ["mkdir -p", "新建目录（含父目录）"],
    ["touch", "新建空文件"],
    ["chmod +x", "添加执行权限"],
    ["chown -R", "递归修改属主"],
    ["ps aux", "列出进程"],
    ["kill -9", "强制结束进程"],
    ["top", "进程监视"],
    ["df -h", "磁盘使用情况"],
    ["du -sh", "目录大小"],
    ["free -h", "内存使用情况"],
    ["systemctl status", "查看服务状态"],
    ["systemctl restart", "重启服务"],
    ["journalctl -u", "查看服务日志"],
    ["journalctl -f", "持续查看系统日志"],
    ["ss -tlnp", "查看监听端口"],
    ["tar -xzf", "解压 .tar.gz"],
    ["curl -I", "查看 HTTP 响应头"],
    ["sudo", "以 root 运行"],
    ["exit", "退出"],
    // docker 扩展（基础项在 shared）
    ["docker ps -a", "列出全部容器"],
    ["docker images", "列出镜像"],
    ["docker exec -it", "进入容器执行命令"],
    ["docker restart", "重启容器"],
    ["docker stop", "停止容器"],
    ["docker rm", "删除容器"],
    ["docker rmi", "删除镜像"],
    ["docker pull", "拉取镜像"],
    ["docker build -t", "构建并标记镜像"],
    ["docker system prune", "清理未使用的资源"],
    ["docker compose ps", "列出服务容器"],
    ["docker compose logs -f", "持续查看服务日志"],
    ["docker compose restart", "重启服务"],
    ["docker compose pull", "拉取服务镜像"],
    ["docker compose build", "构建服务镜像"],
    ["docker compose exec", "在服务容器中执行命令"],
    ["docker-compose up -d", "后台启动服务（v1）"],
    ["docker-compose down", "停止并移除服务（v1）"],
    ["docker-compose ps", "列出服务容器（v1）"],
    ["docker-compose logs -f", "持续查看服务日志（v1）"],
    ["docker-compose restart", "重启服务（v1）"],
    ["docker-compose pull", "拉取服务镜像（v1）"],
    ["docker-compose build", "构建服务镜像（v1）"],
    ["docker-compose exec", "在服务容器中执行命令（v1）"],
  ]),
  shared: toItems([
    ["git status", "查看工作区状态"],
    ["git pull", "拉取并合并"],
    ["git push", "推送"],
    ["git add .", "暂存全部改动"],
    ["git commit -m", "提交"],
    ["git checkout", "切换分支 / 恢复文件"],
    ["git switch -c", "新建并切换分支"],
    ["git log --oneline", "简洁提交历史"],
    ["git diff", "查看改动"],
    ["git stash", "暂存改动"],
    ["git clone", "克隆仓库"],
    ["npm install", "安装依赖"],
    ["npm run dev", "启动开发服务"],
    ["npm run build", "构建"],
    ["pnpm install", "安装依赖"],
    ["pnpm dev", "启动开发服务"],
    ["pnpm build", "构建"],
    ["cargo build", "构建"],
    ["cargo run", "运行"],
    ["cargo test", "运行测试"],
    ["docker ps", "列出容器"],
    ["docker compose up -d", "后台启动服务"],
    ["docker compose down", "停止并移除服务"],
    ["docker logs -f", "持续查看容器日志"],
    ["python", "Python"],
    ["node", "Node.js"],
    ["code .", "用 VS Code 打开当前目录"],
  ]),
};

/** 最后一个 `claude` 词（前面是行首 / 空白 / 提示符结尾字符）之后至少一个空白，取其后的参数部分 */
const CLAUDE_RE = /^(?:.*[\s>$#])?claude(?:\.exe)?(\s+.*)$/;
/** 只在用户刚打过字后弹出，避免程序输出恰好匹配时出现 */
const TYPING_WINDOW = 1500;
const MAX_ITEMS = 8;

/** 用户自定义联想命令（设置中配置，任意命令可用） */
let customItems: Item[] = [];
export function setCustomCompletions(list: Item[]) {
  customItems = list;
}

const USAGE_KEY = "tether.completeUsage";
let usage: Record<string, number> = {};
try {
  usage = JSON.parse(localStorage.getItem(USAGE_KEY) ?? "{}") ?? {};
} catch {}
function bumpUsage(key: string) {
  usage[key] = (usage[key] ?? 0) + 1;
  try {
    localStorage.setItem(USAGE_KEY, JSON.stringify(usage));
  } catch {}
}

/** claude 内置候选；不匹配返回空 */
function completeClaude(before: string): Candidate[] {
  const m = CLAUDE_RE.exec(before);
  if (!m) return [];
  const words = m[1].trimStart().split(/\s+/);
  const partial = words[words.length - 1];
  if (!partial) return [];
  const prev = words[words.length - 2];
  let pool: Item[];
  let kind: string;
  if (prev && VALUES[prev]) (pool = VALUES[prev]), (kind = `val:${prev}`);
  else if (partial.startsWith("-")) (pool = FLAGS), (kind = "flag");
  // 子命令只能是第一个参数
  else if (words.length === 1) (pool = SUBCOMMANDS), (kind = "sub");
  else return [];
  return pool
    .filter((i) => i.text.startsWith(partial) && i.text !== partial)
    .map((i) => ({ ...i, key: `${kind}:${i.text}`, partial }));
}

/** 完整命令候选（自定义 / 内置常用命令）：从行首或提示符结尾（> $ #）后的命令文本（去掉前导空白）是 item.text 的真前缀（不区分大小写） */
function completeCommand(before: string, pool: Item[], kind: string): Candidate[] {
  if (!pool.length) return [];
  let typed: string | undefined;
  for (let i = 0; i < before.length; i++) {
    if (/\S/.test(before[i]) && /(^|[>$#])\s*$/.test(before.slice(0, i))) {
      typed = before.slice(i);
      break;
    }
  }
  if (!typed) return [];
  const lower = typed.toLowerCase();
  return pool
    .filter((i) => i.text.toLowerCase().startsWith(lower) && i.text.length > typed!.length)
    .map((i) => ({ ...i, key: `${kind}:${i.text}`, partial: typed! }));
}

/** 根据光标前文本计算候选（claude + 自定义 + 内置常用命令，按使用次数降序、同次数保持原顺序）；不匹配返回 null */
export function complete(before: string, shell: ShellKind): Candidate[] | null {
  const items = [
    ...completeClaude(before),
    ...completeCommand(before, customItems, "custom"),
    ...completeCommand(before, COMMANDS[shell], `cmd:${shell}`),
    ...completeCommand(before, COMMANDS.shared, "cmd:shared"),
  ];
  items.sort((a, b) => (usage[b.key] ?? 0) - (usage[a.key] ?? 0));
  return items.length ? items : null;
}

let popup: HTMLDivElement | undefined;
/** 当前弹出所属终端与状态 */
let owner: Terminal | undefined;
let partial = "";
let items: Candidate[] = [];
let selected = 0;
let accept: ((item: Candidate) => void) | undefined;

function ensurePopup(container: HTMLElement) {
  if (!popup) {
    popup = document.createElement("div");
    popup.id = "cc-popup";
    popup.hidden = true;
    // 点击候选不让终端失焦
    popup.addEventListener("mousedown", (ev) => ev.preventDefault());
    container.appendChild(popup);
  }
  return popup;
}

function hide() {
  if (popup) popup.hidden = true;
  owner = undefined;
}

function render() {
  const p = popup!;
  p.replaceChildren(
    ...items.map((it, i) => {
      const row = document.createElement("div");
      row.className = i === selected ? "cc-item sel" : "cc-item";
      row.appendChild(document.createElement("b")).textContent = it.text;
      row.appendChild(document.createElement("span")).textContent = it.desc;
      row.addEventListener("click", () => accept?.(it));
      return row;
    }),
  );
  p.querySelector(".sel")?.scrollIntoView({ block: "nearest" });
}

/** 为一个终端启用补全：container 为弹出层的定位容器（#terminals），input 发送补全文本。
 *  返回自定义按键处理（返回 false 表示已消费），由调用方与其它按键处理组合 */
/** enabled 为 false 时（如 Claude Code 在前台）不计算、不显示补全 */
export function attachClaudeComplete(
  term: Terminal,
  container: HTMLElement,
  input: (data: string) => void,
  shell: () => ShellKind,
  enabled: () => boolean,
) {
  let lastInput = 0;
  /** Esc 关闭时的行文本：行内容变化前不再弹出 */
  let dismissed: string | undefined;

  const update = () => {
    const el = term.element;
    if (!el || !enabled() || !el.contains(document.activeElement) || Date.now() - lastInput > TYPING_WINDOW) {
      if (owner === term) hide();
      return;
    }
    const buf = term.buffer.active;
    const line = buf.getLine(buf.baseY + buf.cursorY);
    const before = line?.translateToString(false, 0, buf.cursorX) ?? "";
    const result = before === dismissed ? null : complete(before, shell());
    if (!result) {
      if (owner === term) hide();
      return;
    }
    dismissed = undefined;
    const keep = owner === term ? items[selected]?.key : undefined;
    items = result;
    partial = items.reduce((p, i) => (i.partial.length > p.length ? i.partial : p), "");
    selected = Math.max(0, items.findIndex((i) => i.key === keep));
    owner = term;
    accept = (it) => {
      bumpUsage(it.key);
      if (/^(custom|cmd):/.test(it.key)) {
        // 完整命令：大小写可能不同，先退格删掉已输入文本再输入完整命令
        input(it.text.startsWith(it.partial) ? it.text.slice(it.partial.length) : "\x7f".repeat(it.partial.length) + it.text);
      } else input(it.text.slice(it.partial.length) + " ");
      hide();
      term.focus();
    };
    const p = ensurePopup(container);
    p.hidden = false;
    render();
    position(term, container, p);
  };

  term.onData(() => (lastInput = Date.now()));
  term.onWriteParsed(update);
  term.onCursorMove(update);
  term.textarea?.addEventListener("blur", () => owner === term && hide());

  return (ev: KeyboardEvent) => {
    if (owner !== term || popup?.hidden || ev.isComposing || ev.keyCode === 229) return true;
    if (ev.ctrlKey || ev.altKey || ev.metaKey) return true;
    const key = ev.key;
    if (key !== "ArrowUp" && key !== "ArrowDown" && key !== "Tab" && key !== "Escape") return true;
    ev.preventDefault();
    if (ev.type !== "keydown") return false;
    if (key === "Escape") {
      const buf = term.buffer.active;
      dismissed = buf.getLine(buf.baseY + buf.cursorY)?.translateToString(false, 0, buf.cursorX);
      hide();
    } else if (key === "Tab") {
      accept?.(items[selected]);
    } else {
      selected = (selected + (key === "ArrowDown" ? 1 : items.length - 1)) % items.length;
      render();
    }
    return false;
  };
}

/** 弹出层放在光标下一行（下方放不下时放到光标上方），坐标相对 container */
function position(term: Terminal, container: HTMLElement, p: HTMLDivElement) {
  const screen = term.element?.querySelector(".xterm-screen") as HTMLElement | null;
  if (!screen) return;
  const s = screen.getBoundingClientRect();
  const c = container.getBoundingClientRect();
  const cw = s.width / term.cols;
  const ch = s.height / term.rows;
  const buf = term.buffer.active;
  const x = s.left - c.left + Math.max(0, buf.cursorX - partial.length) * cw;
  const top = s.top - c.top + (buf.cursorY + 1) * ch;
  p.style.maxHeight = `${MAX_ITEMS * 26 + 8}px`;
  const h = p.offsetHeight;
  p.style.top = `${top + h > c.height - 4 ? top - ch - h : top}px`;
  p.style.left = `${Math.max(4, Math.min(x, c.width - p.offsetWidth - 4))}px`;
}
