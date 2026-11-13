# Tether 后台服务（daemon）需求文档

> 组件：`tetherd.exe`（`daemon/`）+ 共享协议 crate（`proto/`）
> 相关文档：[前端 GUI](frontend-requirements.md) · [管理服务器 hub](hub-requirements.md) · [索引](README.md)

## 1. 背景与目标

`tetherd` 是按用户常驻的无窗口后台进程，持有该用户全部终端会话（本地 ConPTY 与 SSH channel）及其状态、vt100 屏幕。它**不是 Windows 服务**：登录自启（HKCU Run），以当前用户身份运行。GUI 只是查看器，关闭 GUI 不结束会话；需要时 daemon 唤起 GUI（如点击通知）。

目标：

- 会话生命周期与 GUI 解耦，GUI 随时接入 / 断开都能恢复画面。
- 空闲近乎零开销（内存约 1 MB 级、CPU 0%）。
- 集中承担所有敏感操作：配置、SSH 认证与凭据、known_hosts、文件系统、Claude Code settings 修改、系统通知。
- 第二阶段作为 hub 的客户端，上报会话并支持远程接管。

## 2. 范围 / 非目标

**范围**：管道服务、本地 / 远程会话、屏幕快照、通知检测与 toast、配置与最近关闭持久化、SSH（认证 / 指纹 / 连接复用 / SFTP）、文件操作与传输、Claude Code hook 集成、安装注册（自启 + AUMID）。

**非目标**：

- 不做 Windows Service / 多用户共享；每个用户一个 daemon。
- 不接受网络入站连接（只监听本机命名管道；hub 连接为出站）。
- 不写 `~/.ssh/config`；不在未经用户点击的情况下修改 `~/.claude/settings.json`。
- 不自动启动 `claude`。
- 不保证跨版本协议兼容（GUI 与 daemon 同包发布）。

## 3. 功能需求

### 3.1 进程、管道与安装

| 编号 | 需求 | 验收标准 | 状态 |
|------|------|----------|------|
| D-1.1 | 监听 `\\.\pipe\tetherd-<用户名>`，以 first-instance 方式保证同用户只有一个 daemon，重复启动即退出 | 连续启动两次，第二个进程记录日志后退出 | 已实现 |
| D-1.2 | 同时只服务一个 GUI：新 GUI 连接顶替旧连接；首条消息为 `ClaudeEvent` 的连接视为 hook 一次性连接，处理后断开，不影响 GUI | hook 触发期间 GUI 不掉线 | 已实现 |
| D-1.3 | dev 隔离：debug 构建管道 `tetherd-dev-<用户名>`，数据目录 `%APPDATA%\tether-dev`、日志 `%LOCALAPPDATA%\tether-dev` | dev 与安装版 daemon 并存 | 已实现 |
| D-1.4 | `--install` 写 HKCU Run 自启并注册 AUMID `com.tether.app`（DisplayName “Tether”）；`--uninstall` 删除；结果写日志 | 安装后重新登录 daemon 自动运行；toast 来源显示 “Tether” | 已实现 |
| D-1.5 | 未注册 AUMID 时 toast 回退 PowerShell AUMID | 未 install 时仍能弹通知（来源显示 Windows PowerShell） | 已实现 |
| D-1.6 | 点击通知时 GUI 未运行则拉起同目录 `tether.exe --session <id>` | GUI 关闭状态下点击 toast，窗口打开并定位会话 | 已实现 |
| D-1.7 | 日志追加到 `%LOCALAPPDATA%\tether\daemon.log` | 启动、连接、错误均有记录 | 已实现 |
| D-1.8 | 打包：`scripts/tauri.mjs` 在 build 时编译 release daemon 并作为 sidecar 打入单个 NSIS 安装包（当前用户安装）；安装 / 升级 / 卸载前 `taskkill` 两个进程，安装后 `--install`，卸载前 `--uninstall` | 安装包约 3 MB，安装后 `tether.exe` 与 `tetherd.exe` 同目录 | 已实现 |

### 3.2 会话

| 编号 | 需求 | 验收标准 | 状态 |
|------|------|----------|------|
| D-2.1 | 本地会话经 ConPTY 启动配置的 shell；环境变量 `TERM=xterm-256color`、`COLORTERM=truecolor`、`TETHER_SESSION`、`TETHER_PIPE` | 会话中 `echo $env:COLORTERM` 为 truecolor | 已实现 |
| D-2.2 | shell 配置：默认 `auto`——每次新建会话时现场探测，PowerShell 7 优先（PATH 中 `pwsh.exe` 或 `%ProgramFiles%\PowerShell\7\pwsh.exe`），否则 `powershell.exe -NoLogo`；显式配置原样尊重；命令行按空白拆分、支持双引号包裹 | 装好 pwsh 后不重启 daemon，新会话即用 pwsh | 已实现 |
| D-2.3 | 可选 shell 现场探测：自动 / Windows PowerShell / PowerShell 7 / cmd / Git Bash / WSL，存在才列出 | 未装 Git 时列表无 Git Bash | 已实现 |
| D-2.4 | `Create` 可携带一次性 `command`（仅本地），用于 winget 安装 PowerShell 7 | 安装会话运行 winget 且不改配置 | 已实现 |
| D-2.5 | 每会话维护 vt100 屏幕；GUI `Attach` 时先回快照再持续推送输出 | 关闭 GUI 期间的输出在重开后可见（当前屏） | 已实现 |
| D-2.6 | 无 GUI attach 时由 daemon 代答 `ESC[6n`（CPR），有 GUI 时交给 xterm 回答，避免重复 | 无 GUI 时启动 PowerShell 不卡住 | 已实现 |
| D-2.7 | 子进程 / channel 结束标记 exited，保留到用户关闭；会话元信息含 name、cwd、command、status、startedAt、lastActivityAt、host、claude | 退出的会话仍在列表中显示 exited | 已实现 |
| D-2.8 | 最近关闭：用户关闭会话时记录目标（本地目录 / 远程 hostId + 绝对路径），最多 10 条、按目标去重、最新在前，持久化 `recent.json`，可删除单条 | 重启 daemon 后最近关闭仍在 | 已实现 |

### 3.3 通知

| 编号 | 需求 | 验收标准 | 状态 |
|------|------|----------|------|
| D-3.1 | 检测 OSC 9（忽略 `9;4` 任务栏进度）与独立 BEL（区分 OSC 终止符），200 ms 内合并、通知后 1 秒静默 | Claude Code 设 `iterm2_with_bell` 时一次完成只弹一条 | 已实现 |
| D-3.2 | GUI 未运行、窗口失焦或会话不在前台时弹 Windows toast，并把会话状态置 waiting | 前台激活会话不弹通知 | 已实现 |
| D-3.3 | 点击仍在屏幕上的 toast：GUI 已连接则发 `Activate`，否则拉起 GUI | 见 D-1.6 | 已实现 |
| D-3.4 | 提示音：toast 一律静音（未打包程序不能用自定义 toast 音频），toast 成功弹出且 `config.sound`（默认 true）开启时用 `PlaySoundW(SND_MEMORY \| SND_ASYNC \| SND_NODEFAULT)` 播放内嵌 WAV，1 秒内只响一次；`PlaySound` 消息（试听）不受开关影响；结果写日志“播放提示音” | 被抑制的通知不响；关闭后通知无声；旧配置默认开启 | 已实现 |

### 3.4 Claude Code 集成

| 编号 | 需求 | 验收标准 | 状态 |
|------|------|----------|------|
| D-4.1 | 仅在 GUI 发 `SetClaudeHooks` 时修改 settings.json（`CLAUDE_CONFIG_DIR` 或 `%USERPROFILE%\.claude`）：写前备份 `.tether-bak-<ms>`；合并到 7 个事件（SessionStart / UserPromptSubmit / Notification / Stop / SubagentStart / SubagentStop / SessionEnd）；只含部分事件时状态报 `outdated`，重新启用补齐；保留其它键与顺序；幂等；非法 JSON 拒绝修改 | 重复启用无重复条目；移除后只删 Tether 条目及因此变空的结构 | 已实现 |
| D-4.2 | `tetherd hook` CLI：从 stdin 读 hook JSON，按 `TETHER_SESSION` / `TETHER_PIPE` 一次性发给 daemon；无输出、退出码恒 0；daemon 不在时约 1 秒放弃；非 Tether 会话立即退出 | 在普通终端运行 claude 不受影响、不报错 | 已实现 |
| D-4.3 | 状态机：SessionStart→Idle、UserPromptSubmit→Thinking、Notification→Waiting（带消息）、Stop→Done，SessionEnd / 会话退出清除；Waiting 时用户按键回到 Thinking | GUI 显示思考中 / 等待确认 / 已完成 | 已实现 |
| D-4.4 | Notification 与 Stop 按 D-3.2 条件弹 toast（标题 `<会话名> · Claude`），`idle_prompt` 不重复通知 | 一次任务完成只弹一条“任务已完成” | 已实现 |
| D-4.5 | 去重：会话有 Claude 状态或 5 秒内收到过 hook 事件时，抑制其 OSC 9 / BEL 通知；远程会话（hook 回不到本机）仍以 OSC 9 / BEL 为准 | 启用集成后本地会话不出现双通知 | 已实现 |
| D-4.6 | 后台子代理：Stop 时 `background_tasks` 中仍有 subagent → Background（带计数，不通知），SubagentStop 计数减一，减到 0 转 Thinking，下一次无子代理的 Stop 才 Done + 通知；忽略 `agent_type` 为空的内部代理；Stop 报 0 个时清空登记，旧版无该字段时按登记计数且 30 分钟过期；SessionStart / SessionEnd / 退出重置 | 后台子代理未结束时不显示已完成、不弹通知；漏掉 SubagentStop 不会永久卡住 | 已实现 |

### 3.5 SSH 远程会话

| 编号 | 需求 | 验收标准 | 状态 |
|------|------|----------|------|
| D-5.1 | russh 实现；同一主机复用一条连接（keepalive 30 s），每会话一个 channel，最后一个会话关闭后断开 | 同主机开 3 个会话只有 1 条 TCP 连接 | 已实现 |
| D-5.2 | 连接时不额外询问（除必要的密码 / 口令 / 指纹）；远程 pty 为 `xterm-256color` 并设 `COLORTERM`；默认进 home，解析为绝对路径 | 点击主机即进入远程 shell | 已实现 |
| D-5.3 | 密钥认证顺序：`keyPath` → Windows OpenSSH ssh-agent → 未配置私钥时尝试 `id_ed25519` / `id_ecdsa` / `id_rsa`；私钥口令弹窗输入不保存 | 仅 agent 中有密钥时可登录 | 已实现未实测（ssh-agent 路径） |
| D-5.4 | 密钥全部失败后回退密码认证（与 OpenSSH 一致，便于只读导入的 ssh config 主机） | 密钥不匹配的主机弹出密码框 | 已实现 |
| D-5.5 | 密码：优先用凭据管理器中已保存的（服务名 `tether`，账户 `user@host:port`），失败删除并重新询问；勾选保存才写入；**绝不明文落盘** | config.json / 日志中无密码 | 已实现 |
| D-5.5a | 设置中保存密码：`SetHostPassword{hostId, password: Secret?}` 写入 / 清除凭据管理器（dev 服务名 `tether-dev`）；Config 回复带 `passwordSaved`（有密码的主机 id）；SetConfig 按 id 比对新旧合并主机列表，账户变化迁移（目标已有不覆盖）、主机删除则删密码，旧账户仍被其它主机使用时保留 | 改端口后凭据随之迁移；删除主机后凭据消失；config.json / 日志中无密码 | 已实现 |
| D-5.6 | 服务器只开 keyboard-interactive 时用同一密码回应单个密码提示 | 仅开 KI 的服务器可登录 | 已实现未实测 |
| D-5.7 | 主机来源：config.json `hosts` + 只读解析 `~/.ssh/config`（跳过通配模式，识别 HostName / User / Port / IdentityFile，id `ssh:<alias>`）；覆盖项整条替换，删除即恢复默认；未写 User 时默认 `root`；**永不写 `~/.ssh/config`** | 编辑 / 恢复默认后 `~/.ssh/config` 修改时间不变 | 已实现 |
| D-5.8 | 按 `~/.ssh/known_hosts` 校验（非 22 端口 `[host]:port`）：未知主机经 GUI 确认后追加；不一致**直接拒绝**，无“仍然继续” | 篡改记录后连接失败并报错 | 已实现 |
| D-5.9 | 需要交互但无 GUI 时连接直接失败；提示 2 分钟无响应视为取消 | 无 GUI 时连接密码主机立即报错 | 已实现 |

### 3.6 文件系统

| 编号 | 需求 | 验收标准 | 状态 |
|------|------|----------|------|
| D-6.1 | 以 sessionId 定位后端：本地磁盘或该会话连接上懒建、复用的 SFTP；同 reqId 回复 | 远程文件树不新增 SSH 连接 | 已实现 |
| D-6.2 | 列目录（目录在前、名称排序；本地空路径列盘符）、新建、重命名、递归删除（符号链接只删链接） | 各操作返回成功 / 错误信息 | 已实现 |
| D-6.3 | 读文本（UTF-8 lossy，>2 MB 拒绝）供 Markdown 预览 | 超限文件返回错误 | 已实现 |
| D-6.4 | 打开：本地默认程序 / 默认浏览器（https UserChoice，失败回退）；远程先下载到 `%TEMP%\tether\<user@host_port>\<路径>` | 远程文件下载后打开 | 已实现 |
| D-6.5 | 上传 / 下载：递归、同名覆盖、文件夹合并，推送累计字节进度 | 进度事件单调递增至 total | 已实现 |
| D-6.6 | `OpenUrl` 仅接受 http(s)，其它协议忽略 | `file:` / `javascript:` 链接不被打开 | 已实现 |
| D-6.7 | `FsCopy`：源为本地或 hostId（经连接池复用 / 重连，传完释放），目标为会话后端；远程 → 远程按块流式中转、不落临时文件；`keepBoth` 项在目标已有同名项时改名 `name (n).ext`（隐藏文件 / 文件夹不拆扩展名，同批次互不冲突），其余同名覆盖、文件夹合并 | 四种方向递归复制正确，进度推送至 total | 已实现 |

### 3.7 hub 客户端（第二阶段）

| 编号 | 需求 | 验收标准 | 状态 |
|------|------|----------|------|
| D-7.1 | 主动出站 WSS 连接 hub，token 来自设置、存凭据管理器；断线自动重连（退避） | hub 重启后客户端自动恢复；客户端无入站端口 | 计划中 |
| D-7.2 | 上报会话列表与元信息（复用 `SessionInfo`：name、cwd、host、status、claude、lastActivityAt），默认不传终端内容 | hub 侧只见元信息；网络流量随会话变化而非输出量 | 计划中 |
| D-7.3 | 接管：hub 请求接管时先发 vt100 快照，再流式推送输出，并接受 hub 的输入 / resize；本地 GUI 可收回控制 | 接管期间本地 GUI 显示被接管标识；收回后 hub 只读或断开 | 计划中 |
| D-7.4 | 本地开关可禁止接管（按机器；按会话为待定项） | 开关关闭时 hub 接管请求被拒绝 | 计划中 |

协议与安全细节见 [hub-requirements.md](hub-requirements.md)。

## 4. 接口：GUI ↔ daemon 管道协议

- 传输：本机命名管道 `\\.\pipe\tetherd-<用户名>`（dev：`tetherd-dev-<用户名>`），每行一条 JSON，`type` 字段区分消息，终端数据 base64。类型定义在 `proto/src/lib.rs`，GUI（Rust 侧透传 + TS 类型）与 daemon 共用。
- **GUI → daemon（ClientMsg）**：会话（List / Create / Attach / Input / Resize / Rename / Close / Focus）、配置（GetConfig / SetConfig / SetHostPassword）、交互回复（PromptReply）、文件（FsList / FsMkdir / FsCreateFile / FsRename / FsDelete / FsOpen / FsRead / Upload / Download / FsCopy）、OpenUrl、RemoveRecent、Claude 集成（GetClaudeHooks / SetClaudeHooks）；`ClaudeEvent` 仅由 `tetherd hook` 一次性连接发送。
- **daemon → GUI（ServerMsg）**：Sessions（全量）、Snapshot、Output、Notify、Activate、Error、Config（含探测到的 shell、合并后主机、ssh config 原值、有已保存密码的主机 id）、Prompt、FsEntries / FsContent / FsResult、Transfer、Recent、ClaudeHooks。
- 文件类请求以 `reqId` 关联回复；Prompt 以 `reqId` 关联 PromptReply，`value=null` 表示取消。
- 版本策略：同包发布，不做协议版本协商（见待定项）。

## 5. 非功能需求

### 5.1 性能与资源预算（release 实测）

| 指标 | 预算 / 实测 |
|------|-------------|
| 空闲内存 | 约 1.3 MB 私有 / 11 MB 工作集 |
| 空闲 CPU | 0% |
| 每会话增量 | ConPTY 约 +1–2 MB；shell 本身占大头（Windows PowerShell 约 60 MB、cmd 约 10 MB） |
| 运行时 | 单线程 tokio；不得引入常驻轮询 |

### 5.2 安全

- 只监听当前用户命名管道，无网络入站端口。
- 密码只存 Windows 凭据管理器；私钥口令不保存；日志不记录密码 / 口令。
- 不写 `~/.ssh/config`；known_hosts 只追加经用户确认的新主机，不一致直接拒绝。
- `~/.claude/settings.json` 仅在用户显式启用 / 移除时修改，且先备份。
- `OpenUrl` 仅 http(s)。

### 5.3 兼容性

- Windows 10/11 x64（ConPTY、OpenSSH agent 命名管道）。
- russh 使用 ring 后端（避免 aws-lc-rs 在 Windows 上需 cmake / nasm）。
- config.json 新增字段一律带默认值，旧配置可直接读取；解析失败使用默认配置。
- `appearance` 只含 `opacity`（窗口透明度，默认 100）；旧版背景图配置中的 `background` / `blur` 忽略，且此时旧 `opacity`（面板不透明度）不沿用、恢复默认。

## 6. 待定项

1. 管道 ACL：当前依赖默认安全描述符，是否需要显式限定当前用户 SID。
2. vt100 无回滚缓冲（scrollback 0），GUI 重连只恢复当前屏；是否增加有限回滚（内存预算待定）。
3. 协议无版本号；引入 hub 后若 daemon 与 hub 独立升级，需要版本协商。
4. 安装 / 升级会 `taskkill` 结束所有会话，是否需要升级前提示或会话迁移。
5. daemon 崩溃即丢失全部会话，暂无持久化 / 恢复方案。
6. 标为“已实现未实测”的条目需补测。
