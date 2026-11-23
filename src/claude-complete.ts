// `claude` 命令补全：光标前的当前行以 `claude <部分参数>` 结尾时，在光标附近弹出候选（子命令 / 参数 / 部分参数值）。
// 纯前端，本地 / 远程会话通用。只读光标所在的这一物理行（命令折行后不识别）。
import type { Terminal } from "@xterm/xterm";
import { type MessageKey, t } from "./i18n";

interface Item {
  text: string;
  /** 说明；i18n 为 true 时是词条键（内置候选），显示时按当前语言取文字 */
  desc: string;
  i18n?: boolean;
}

/** 弹出的候选：key 为使用计数键，partial 为接受时要替换掉的已输入文本 */
interface Candidate extends Item {
  key: string;
  partial: string;
}

// 候选取自 Claude Code 2.1.322 的 `claude --help`（精选常用项）
const SUBCOMMANDS: Item[] = [
  ["agents", "complete.sub.agents"],
  ["attach", "complete.sub.attach"],
  ["auth", "complete.sub.auth"],
  ["auto-mode", "complete.sub.autoMode"],
  ["doctor", "complete.sub.doctor"],
  ["import", "complete.sub.import"],
  ["install", "complete.sub.install"],
  ["logs", "complete.sub.logs"],
  ["mcp", "complete.sub.mcp"],
  ["plugin", "complete.sub.plugin"],
  ["purge", "complete.sub.purge"],
  ["respawn", "complete.sub.respawn"],
  ["rm", "complete.sub.rm"],
  ["setup-token", "complete.sub.setupToken"],
  ["stop", "complete.sub.stop"],
  ["ultrareview", "complete.sub.ultrareview"],
  ["update", "complete.sub.update"],
].map(([text, desc]) => ({ text, desc, i18n: true }));

const FLAGS: Item[] = [
  ["--continue", "complete.flag.continue"],
  ["-c", "complete.flag.shortC"],
  ["--resume", "complete.flag.resume"],
  ["-r", "complete.flag.shortR"],
  ["--print", "complete.flag.print"],
  ["-p", "complete.flag.shortP"],
  ["--model", "complete.flag.model"],
  ["--permission-mode", "complete.flag.permissionMode"],
  ["--effort", "complete.flag.effort"],
  ["--add-dir", "complete.flag.addDir"],
  ["--dangerously-skip-permissions", "complete.flag.dangerouslySkipPermissions"],
  ["--allow-dangerously-skip-permissions", "complete.flag.allowDangerouslySkipPermissions"],
  ["--name", "complete.flag.name"],
  ["-n", "complete.flag.shortN"],
  ["--worktree", "complete.flag.worktree"],
  ["-w", "complete.flag.shortW"],
  ["--fork-session", "complete.flag.forkSession"],
  ["--session-id", "complete.flag.sessionId"],
  ["--agent", "complete.flag.agent"],
  ["--allowedTools", "complete.flag.allowedTools"],
  ["--disallowedTools", "complete.flag.disallowedTools"],
  ["--tools", "complete.flag.tools"],
  ["--append-system-prompt", "complete.flag.appendSystemPrompt"],
  ["--system-prompt", "complete.flag.systemPrompt"],
  ["--mcp-config", "complete.flag.mcpConfig"],
  ["--settings", "complete.flag.settings"],
  ["--output-format", "complete.flag.outputFormat"],
  ["--fallback-model", "complete.flag.fallbackModel"],
  ["--background", "complete.flag.background"],
  ["--bare", "complete.flag.bare"],
  ["--safe-mode", "complete.flag.safeMode"],
  ["--verbose", "complete.flag.verbose"],
  ["--debug", "complete.flag.debug"],
  ["--ide", "complete.flag.ide"],
  ["--chrome", "complete.flag.chrome"],
  ["--version", "complete.flag.version"],
  ["-v", "complete.flag.shortV"],
  ["--help", "complete.flag.help"],
  ["-h", "complete.flag.shortH"],
].map(([text, desc]) => ({ text, desc, i18n: true }));

/** 取值固定的参数：上一个词为该参数时补全取值 */
const VALUES: Record<string, Item[]> = {
  "--model": ["fable", "opus", "sonnet"].map((text) => ({ text, desc: "complete.value.model", i18n: true })),
  "--permission-mode": ["acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan"].map((text) => ({ text, desc: "complete.value.permissionMode", i18n: true })),
  "--effort": ["low", "medium", "high", "xhigh", "max"].map((text) => ({ text, desc: "complete.value.effort", i18n: true })),
  "--output-format": ["text", "json", "stream-json"].map((text) => ({ text, desc: "complete.value.outputFormat", i18n: true })),
};

/** 会话 shell 类型：决定内置常用命令列表 */
export type ShellKind = "powershell" | "cmd" | "posix";

const toItems = (list: string[][]): Item[] => list.map(([text, desc]) => ({ text, desc, i18n: true }));

/** 各 shell 内置常用命令（按完整命令前缀匹配，同自定义命令） */
const COMMANDS: Record<ShellKind | "shared", Item[]> = {
  powershell: toItems([
    ["Get-ChildItem", "complete.powershell.getChildItem"],
    ["Set-Location", "complete.powershell.setLocation"],
    ["Get-Location", "complete.powershell.getLocation"],
    ["Get-Content", "complete.powershell.getContent"],
    ["Get-Content -Wait -Tail 50", "complete.powershell.getContentWaitTail50"],
    ["Set-Content", "complete.powershell.setContent"],
    ["Select-String", "complete.powershell.selectString"],
    ["Remove-Item", "complete.powershell.removeItem"],
    ["Remove-Item -Recurse -Force", "complete.powershell.removeItemRecurseForce"],
    ["Copy-Item", "complete.powershell.copyItem"],
    ["Move-Item", "complete.powershell.moveItem"],
    ["New-Item -ItemType Directory", "complete.powershell.newItemItemTypeDirectory"],
    ["New-Item -ItemType File", "complete.powershell.newItemItemTypeFile"],
    ["Rename-Item", "complete.powershell.renameItem"],
    ["Test-Path", "complete.powershell.testPath"],
    ["Get-Process", "complete.powershell.getProcess"],
    ["Stop-Process -Name", "complete.powershell.stopProcessName"],
    ["Get-Service", "complete.powershell.getService"],
    ["Get-Command", "complete.powershell.getCommand"],
    ["Get-Help", "complete.powershell.getHelp"],
    ["Invoke-WebRequest", "complete.powershell.invokeWebRequest"],
    ["Get-NetTCPConnection", "complete.powershell.getNetTCPConnection"],
    ["$env:PATH", "complete.powershell.envPATH"],
    ["ls", "complete.powershell.ls"],
    ["cd ..", "complete.powershell.cdUp"],
    ["cat", "complete.powershell.cat"],
    ["rm", "complete.powershell.rm"],
    ["cp", "complete.powershell.cp"],
    ["mv", "complete.powershell.mv"],
    ["mkdir", "complete.powershell.mkdir"],
    ["pwd", "complete.powershell.pwd"],
    ["cls", "complete.powershell.cls"],
    ["where.exe", "complete.powershell.whereExe"],
  ]),
  cmd: toItems([
    ["dir", "complete.cmd.dir"],
    ["dir /s /b", "complete.cmd.dirSB"],
    ["cd", "complete.cmd.cd"],
    ["cd ..", "complete.cmd.cdUp"],
    ["cd /d", "complete.cmd.cdD"],
    ["copy", "complete.cmd.copy"],
    ["xcopy /e /i", "complete.cmd.xcopyEI"],
    ["robocopy", "complete.cmd.robocopy"],
    ["del", "complete.cmd.del"],
    ["move", "complete.cmd.move"],
    ["ren", "complete.cmd.ren"],
    ["mkdir", "complete.cmd.mkdir"],
    ["rmdir /s /q", "complete.cmd.rmdirSQ"],
    ["type", "complete.cmd.type"],
    ["findstr", "complete.cmd.findstr"],
    ["set", "complete.cmd.set"],
    ["where", "complete.cmd.where"],
    ["tasklist", "complete.cmd.tasklist"],
    ["taskkill /f /im", "complete.cmd.taskkillFIm"],
    ["netstat -ano", "complete.cmd.netstatAno"],
    ["ipconfig", "complete.cmd.ipconfig"],
    ["echo", "complete.cmd.echo"],
    ["cls", "complete.cmd.cls"],
    ["exit", "complete.cmd.exit"],
  ]),
  posix: toItems([
    ["ls -la", "complete.posix.lsLa"],
    ["cd ..", "complete.posix.cdUp"],
    ["pwd", "complete.posix.pwd"],
    ["cat", "complete.posix.cat"],
    ["less", "complete.posix.less"],
    ["tail -f", "complete.posix.tailF"],
    ["head", "complete.posix.head"],
    ["grep -rn", "complete.posix.grepRn"],
    ["find . -name", "complete.posix.findDotName"],
    ["cp -r", "complete.posix.cpR"],
    ["mv", "complete.posix.mv"],
    ["rm -rf", "complete.posix.rmRf"],
    ["mkdir -p", "complete.posix.mkdirP"],
    ["touch", "complete.posix.touch"],
    ["chmod +x", "complete.posix.chmodX"],
    ["chown -R", "complete.posix.chownR"],
    ["ps aux", "complete.posix.psAux"],
    ["kill -9", "complete.posix.kill9"],
    ["top", "complete.posix.top"],
    ["df -h", "complete.posix.dfH"],
    ["du -sh", "complete.posix.duSh"],
    ["free -h", "complete.posix.freeH"],
    ["systemctl status", "complete.posix.systemctlStatus"],
    ["systemctl restart", "complete.posix.systemctlRestart"],
    ["journalctl -u", "complete.posix.journalctlU"],
    ["journalctl -f", "complete.posix.journalctlF"],
    ["ss -tlnp", "complete.posix.ssTlnp"],
    ["tar -xzf", "complete.posix.tarXzf"],
    ["curl -I", "complete.posix.curlI"],
    ["sudo", "complete.posix.sudo"],
    ["exit", "complete.posix.exit"],
    // docker 扩展（基础项在 shared）
    ["docker ps -a", "complete.posix.dockerPsA"],
    ["docker images", "complete.posix.dockerImages"],
    ["docker exec -it", "complete.posix.dockerExecIt"],
    ["docker restart", "complete.posix.dockerRestart"],
    ["docker stop", "complete.posix.dockerStop"],
    ["docker rm", "complete.posix.dockerRm"],
    ["docker rmi", "complete.posix.dockerRmi"],
    ["docker pull", "complete.posix.dockerPull"],
    ["docker build -t", "complete.posix.dockerBuildT"],
    ["docker system prune", "complete.posix.dockerSystemPrune"],
    ["docker compose ps", "complete.posix.dockerComposePs"],
    ["docker compose logs -f", "complete.posix.dockerComposeLogsF"],
    ["docker compose restart", "complete.posix.dockerComposeRestart"],
    ["docker compose pull", "complete.posix.dockerComposePull"],
    ["docker compose build", "complete.posix.dockerComposeBuild"],
    ["docker compose exec", "complete.posix.dockerComposeExec"],
    ["docker-compose up -d", "complete.posix.dockerComposeV1UpD"],
    ["docker-compose down", "complete.posix.dockerComposeV1Down"],
    ["docker-compose ps", "complete.posix.dockerComposeV1Ps"],
    ["docker-compose logs -f", "complete.posix.dockerComposeV1LogsF"],
    ["docker-compose restart", "complete.posix.dockerComposeV1Restart"],
    ["docker-compose pull", "complete.posix.dockerComposeV1Pull"],
    ["docker-compose build", "complete.posix.dockerComposeV1Build"],
    ["docker-compose exec", "complete.posix.dockerComposeV1Exec"],
  ]),
  shared: toItems([
    ["git status", "complete.shared.gitStatus"],
    ["git pull", "complete.shared.gitPull"],
    ["git push", "complete.shared.gitPush"],
    ["git add .", "complete.shared.gitAddDot"],
    ["git commit -m", "complete.shared.gitCommitM"],
    ["git checkout", "complete.shared.gitCheckout"],
    ["git switch -c", "complete.shared.gitSwitchC"],
    ["git log --oneline", "complete.shared.gitLogOneline"],
    ["git diff", "complete.shared.gitDiff"],
    ["git stash", "complete.shared.gitStash"],
    ["git clone", "complete.shared.gitClone"],
    ["npm install", "complete.shared.npmInstall"],
    ["npm run dev", "complete.shared.npmRunDev"],
    ["npm run build", "complete.shared.npmRunBuild"],
    ["pnpm install", "complete.shared.pnpmInstall"],
    ["pnpm dev", "complete.shared.pnpmDev"],
    ["pnpm build", "complete.shared.pnpmBuild"],
    ["cargo build", "complete.shared.cargoBuild"],
    ["cargo run", "complete.shared.cargoRun"],
    ["cargo test", "complete.shared.cargoTest"],
    ["docker ps", "complete.shared.dockerPs"],
    ["docker compose up -d", "complete.shared.dockerComposeUpD"],
    ["docker compose down", "complete.shared.dockerComposeDown"],
    ["docker logs -f", "complete.shared.dockerLogsF"],
    ["python", "Python"],
    ["node", "Node.js"],
    ["code .", "complete.shared.codeDot"],
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
      row.appendChild(document.createElement("span")).textContent = it.i18n ? t(it.desc as MessageKey) : it.desc;
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
