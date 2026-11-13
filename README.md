# Tether

轻量多会话终端（Tauri 2 + TypeScript + xterm.js + ConPTY），会话由后台进程持有，关闭窗口不中断；可在会话中运行 Claude Code 并接收完成通知。

需求文档见 [docs/](docs/README.md)（前端 / 后台服务 / 管理服务器 hub）。版本变更见 [CHANGELOG.md](CHANGELOG.md)。

## 架构

- `daemon/`（`tetherd.exe`）：无窗口后台进程，持有全部终端会话（PTY、状态、vt100 屏幕），检测 OSC 9/BEL 并发系统通知。
- `src-tauri/` + `src/`（`tether.exe`）：GUI 查看器，通过命名管道 `\\.\pipe\tetherd-<用户名>` 接入 daemon；关闭窗口不影响会话。
- `proto/`：两者共享的管道协议类型（换行分隔 JSON，终端数据 base64）。
- GUI 启动时连不上 daemon 会自动拉起同目录下的 `tetherd.exe`；点击通知时若 GUI 未运行，daemon 会拉起同目录下的 `tether.exe`。

## 前置依赖

- Windows 10/11，WebView2
- Node.js + pnpm
- Rust 工具链（stable）与 MSVC 构建工具，见 https://tauri.app/start/prerequisites/
- （可选）Claude Code，在会话中运行 `claude` 即可

## 开发运行

Cargo workspace 产物统一在根目录 `target/`，daemon 与 GUI 同在 `target/debug/`：

```sh
pnpm install
cargo build -p tetherd   # 先编译 daemon
pnpm tauri dev                # 启动 GUI，连不上时自动拉起 target/debug/tetherd.exe
```

也可手动先运行 `target/debug/tetherd.exe` 再起 GUI。daemon 运行中 exe 被占用，重新编译 daemon 前需先结束它（`taskkill /im tetherd.exe /f`，会结束所有会话，含安装版的 daemon，建议按路径只结束 `target\debug\tetherd.exe`）。日志位于 `%LOCALAPPDATA%\tether\daemon.log`（dev 见下）。

**dev 与安装版隔离**：debug 构建（`cfg!(debug_assertions)`，由 `proto::pipe_name()` / `proto::data_dir_name()` 统一判断）使用管道 `\\.\pipe\tetherd-dev-<用户名>`、配置与最近关闭 `%APPDATA%\tether-dev\`、日志 `%LOCALAPPDATA%\tether-dev\daemon.log`。GUI 在 debug 下运行时把 Tauri identifier 改为 `com.tether.app.dev`、窗口标题改为 “Tether (dev)”（`src-tauri/src/lib.rs`，`cargo build` 与 `pnpm tauri dev` 一致），因此应用数据目录 `%APPDATA%\com.tether.app.dev`、WebView2 数据 / localStorage `%LOCALAPPDATA%\com.tether.app.dev` 与单实例键均与安装版分开，两者可同时运行。通知 AUMID 只读沿用安装版注册；dev 下不要执行 `tetherd --install`（会把登录自启改指向 debug 版）。

**单实例**：GUI 用 `tauri-plugin-single-instance` 保证只有一个进程；再次启动 `tether.exe` 会立即退出，已有主窗口还原并前置，若带 `--session <id>`（点击通知拉起）则切到该会话（会话已拖出为独立窗口时前置该窗口）。

打包：`pnpm tauri build` 一条命令产出 NSIS 安装包 `target/release/bundle/nsis/tether_<版本>_x64-setup.exe`。`package.json` 的 `tauri` 脚本经 `scripts/tauri.mjs` 包装：build 时先 `cargo build --release -p tetherd` 并复制为 `src-tauri/binaries/tetherd-x86_64-pc-windows-msvc.exe`，再以 sidecar（`bundle.externalBin`，仅 build 时通过 `--config` 注入，dev 不受影响）打包，安装后与 `tether.exe` 同目录、名为 `tetherd.exe`。安装包默认按当前用户安装到 `%LOCALAPPDATA%\tether`，钩子（`src-tauri/windows/hooks.nsh`）在安装 / 升级前结束运行中的 `tether.exe` 与 `tetherd.exe`（会结束所有会话），安装后执行 `tetherd --install`；卸载前同样结束两者并执行 `tetherd --uninstall`。

## 登录自启

```sh
tetherd --install     # 写入 HKCU\...\CurrentVersion\Run，并注册通知用的 AppUserModelID
tetherd --uninstall   # 删除上述注册表项
```

结果写入 daemon.log。未执行 `--install` 时通知来源会显示为 Windows PowerShell。

## 设置

点击侧边栏底部的 **设置**，在下拉框中选择新建会话启动的默认 shell，默认 **自动（PowerShell 7 优先）**，只影响之后新建的会话。可选项由 daemon 在每次打开设置时现场探测，存在才列出：自动、Windows PowerShell、PowerShell 7（PATH 中有 `pwsh.exe`，或存在 `%ProgramFiles%\PowerShell\7\pwsh.exe`，后者用完整路径启动）、命令提示符、Git Bash（由 PATH 中 `git.exe` 推导，或 `%ProgramFiles%\Git\bin\bash.exe`）、WSL（`%SystemRoot%\System32\wsl.exe`）。配置由 daemon 保存在 `%APPDATA%\tether\config.json`：

```json
{ "shell": "auto" }
```

- **自动**（`"auto"`，config.json 无 `shell` 字段时同此）：每次新建本地会话时现场探测，有 PowerShell 7 用它，否则用 `powershell.exe -NoLogo`；因为同时检查默认安装位置（daemon 常驻，PATH 可能是安装前的旧值），装好 PowerShell 7 后新会话即生效，无需重启 daemon。已写明的具体 shell 值保持不变。
- **安装引导**：未检测到 PowerShell 7 时，终端区顶部显示提示条（**安装** / **不再提示**，后者记在 localStorage），设置中 shell 下拉框下方也有一行提示。**安装** 在用户主目录新建一个本地会话运行 `winget install --id Microsoft.PowerShell -e --source winget`，可在其中查看进度、响应 UAC / 协议确认；该会话结束（或下次打开设置）时重新探测，装好后提示条消失。

仍可直接手改该文件指定任意命令行，不在探测列表中的值会在下拉框中单独列出并保持选中。命令行按空白拆分为程序 + 参数；路径含空格时用双引号包裹，如 `"C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo`（不支持引号转义）。文件不存在或解析失败时使用默认值。

## 外观

**设置 → 外观 → 窗口透明度**（30–100%，默认 100% 即完全不透明）调节窗口背景的不透明度，调低后可透过 Tether 看到桌面和其它窗口（类似 Windows Terminal 的 opacity）。只有背景变半透明：窗口底色、侧栏 / 文件树区域、终端卡片按设置的透明度绘制，终端文字、图标、按钮始终不透明、清晰；设置 / 确认对话框与菜单的不透明度至少 88% 以保证可读。实现上 Tauri 窗口为透明窗口（`transparent: true`，保留系统标题栏），各层底色用带 alpha 的颜色，不使用 `backdrop-filter`（WebView2 内存开销大）。

- 拖动滑块实时预览，点 **保存** 才生效，**取消**（或 Esc）恢复原样。
- 设置保存在 `config.json` 的 `appearance` 中（无此字段时按默认值处理）：

```json
{ "appearance": { "opacity": 85 } }
```

- 旧版的背景图功能已移除：旧配置中的 `background` / `blur` 字段被忽略（此时旧的 `opacity` 是背景图上的面板不透明度，不沿用，窗口恢复为不透明），GUI 启动时删除应用数据目录（`%APPDATA%\com.tether.app\`）下残留的 `background-*` 文件。

透明度为 100% 时外观与默认深色主题完全一致。

## 复制粘贴

行为参照 Windows Terminal，本地 / 远程会话一致；剪贴板经 `tauri-plugin-clipboard-manager` 读写（WebView2 中 `navigator.clipboard.readText` 会弹权限询问）。

- **选择即复制**：鼠标拖选、双击选词、三击选整行，松开鼠标时自动复制，终端右上角短暂提示“已复制”。双击选词以空白、引号、括号、逗号等为分隔，Windows 路径（`C:\a\b.txt`）与 URL 整体选中。
- **Ctrl+C**：有选区时复制并清除选区，不发送中断；无选区时照常发送 Ctrl+C（中断）。**Ctrl+Shift+C** 只复制。
- **表格单元格内拖选**：在 Claude Code 输出的框线表格（`│`）某个单元格内按住左键拖动，选区只覆盖该单元格所在列，不会选中同行其它列；复制时单元格内的折行拼回一段（两侧都是 ASCII 时补一个空格），跨过行分隔线处换行。单击、双击及程序开启鼠标上报时仍走 xterm 原生行为。
- **Ctrl+V** / **Ctrl+Shift+V** / **右键**：粘贴（右键不弹菜单）。剪贴板中没有文本（为空或是图片）时，**Ctrl+V** 改为向会话发送 Claude Code 的图片粘贴键：本地会话发 Alt+V（`\x1bv`，Windows 版 Claude Code 只绑定 Alt+V，`^V` 无效），远程会话发 `^V`（`\x16`）；Claude Code 收到后自行读取剪贴板图片。经 xterm 的 `paste` 发送，程序开启 bracketed paste 时（如 PowerShell 7 自带的 PSReadLine 2.2+、Claude Code）多行内容整体粘贴而不逐行执行；Windows PowerShell 5.1 自带的 PSReadLine 2.0 不支持，多行粘贴会逐行执行。

## 完成通知

daemon 检测会话输出中的 OSC 9 通知与响铃（BEL）。本地会话推荐启用下文的 **Claude Code 集成**（hooks，状态更精确）；远程会话或未启用集成时，如需 Claude Code 完成通知，在 `~/.claude/settings.json` 中设置：

```json
{
  "preferredNotifChannel": "iterm2_with_bell"
}
```

GUI 未运行、窗口失去焦点或会话不在前台时，收到通知会弹出系统通知，侧边栏对应会话出现提示圈；点击仍在屏幕上的通知会打开 GUI 并跳转到该会话（已进入操作中心的通知点击不会回调）。

**提示音**：弹出系统通知时播放内置的鸟鸣提示音（`daemon/assets/notify.wav`，0.8 秒，编译进 `tetherd.exe`，约 70 KB）。未打包的 Win32 程序不能给 toast 指定自定义音频，所以 toast 本身静音，由 daemon 用 winmm `PlaySoundW` 异步播放；1 秒内连续弹出多条通知只响一次，被抑制的通知（会话在前台、去重）不响。**设置 → 通知 → 提示音** 可关闭（关闭后通知完全静音，不回退系统默认音），“试听”按钮不受开关影响。提示音由 `scripts/gen-chirp.js` 合成（变体 d）：`node scripts/gen-chirp.js d` 在当前目录生成 `chirp-d.wav`，复制为 `daemon/assets/notify.wav` 即可。

## Claude Code 集成

**设置 → Claude Code 集成** 显示状态（未启用 / 已启用 / 需要更新）与 settings.json 路径，点 **启用** / **移除** 并确认后由 daemon 修改 Claude Code 的用户设置文件（`CLAUDE_CONFIG_DIR` 环境变量指定的目录下的 `settings.json`，未设置时为 `%USERPROFILE%\.claude\settings.json`）。

- **写入内容**：在 `hooks` 下为 `SessionStart`、`UserPromptSubmit`、`Notification`、`Stop`、`SubagentStart`、`SubagentStop`、`SessionEnd` 各追加一个匹配组（不写 matcher，即匹配全部），命令为当前运行的 `tetherd.exe` 绝对路径（正斜杠；路径无空格时不加引号，Git Bash 与 PowerShell 均可执行，含空格时加双引号，适用于 Claude Code 默认的 Git Bash）加 ` hook`：

  ```json
  { "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "C:/Users/me/AppData/Local/tether/tetherd.exe hook", "timeout": 5 } ] } ] } }
  ```

- 写入前备份为同目录 `settings.json.tether-bak-<毫秒时间戳>`；文件不存在时新建，不是合法 JSON 时拒绝修改并报错。其它键与用户自己的 hooks 保持不动（键顺序保留，以 2 空格缩进写回）；重复启用不会产生重复条目（命令含 `tetherd` 与 ` hook` 的条目视为 Tether 的，先删后加，换了安装位置也会更新路径），已是目标状态时不写文件。旧版只写了前 5 个事件，设置中显示 **需要更新**，点 **更新**（即重新启用）补齐子代理事件。
- **移除**：只删除 Tether 的条目，以及因此变空的匹配组 / 事件数组 / `hooks` 对象。
- **工作方式**：daemon 给每个本地会话设置环境变量 `TETHER_SESSION=<会话 id>`、`TETHER_PIPE=<管道名>`（不含 `\\.\pipe\` 前缀，dev / 安装版各指向自己的 daemon）。Claude Code 触发 hook 时运行 `tetherd hook`：从 stdin 读 hook JSON，经命名管道发一条一次性消息给 daemon 后退出（不占用 GUI 连接），不输出任何内容、退出码恒为 0，daemon 未运行时约 1 秒内放弃；不在 Tether 会话中（无上述环境变量）运行时立即退出。
- **状态**：侧栏会话第二行与卡片标题行显示 `✳` 标记：`Claude`（已启动）、`思考中`（UserPromptSubmit）、`等待确认`（Notification，橙色，悬停显示消息；在终端中按键作答后回到思考中）、`子代理运行中 ×N`（主代理 Stop 时仍有后台子代理在运行）、`已完成`（Stop 且没有后台子代理）；SessionEnd 或会话退出时清除。
- **后台子代理**：主代理 Stop 时按 hook JSON 的 `background_tasks`（`type` 为 `subagent` 的条目，后台 shell / monitor 不计）判断是否仍有子代理在运行：有则显示 `子代理运行中 ×N` 且不通知，每个 `SubagentStop` 计数减一；最后一个结束后 Claude 会被唤起处理结果（不经 UserPromptSubmit），状态转为 `思考中`，直到下一次没有后台子代理的 Stop 才显示 `已完成` 并通知。Claude Code 内部代理（`agent_type` 为空，如提示建议）不计。防卡住：Stop 报告没有子代理时清空登记；不带 `background_tasks` 的旧版 Claude Code 按 SubagentStart / SubagentStop 登记计数，超过 30 分钟未结束的登记作废；新提示、SessionStart、SessionEnd、会话退出也会重置。
- **通知**：Notification（标题 `<会话名> · Claude`，内容为 Claude 的消息）与 Stop（内容“任务已完成”，有后台子代理时推迟到全部完成）按与 OSC 9 相同的条件弹系统通知；回复完成后的空闲提醒（`idle_prompt`）不重复通知。会话有 Claude 状态或 5 秒内收到过 hook 事件时，不再对其 OSC 9 / BEL 弹通知，避免重复；远程会话（hook 无法回到本机 daemon）与未启用集成时仍以 OSC 9 / BEL 为准。

## claude 命令补全

在终端中输入 `claude ` 加部分参数（如 `claude --re`、`claude mc`、`claude -`）时，光标下方弹出候选子命令 / 参数及简短说明；`--model`、`--permission-mode`、`--effort`、`--output-format` 后补全其取值。候选是按 Claude Code 2.1.322 的 `claude --help` 整理的静态列表（`src/claude-complete.ts`），纯前端实现，本地 / 远程会话通用。

- 只在刚打过字时（1.5 秒内）弹出，程序输出恰好匹配时不会出现；**↑ / ↓** 选择、**Tab** 补全（发送剩余字符 + 空格）、**Esc** 关闭（该行内容变化前不再弹出），也可点击候选。弹出层未显示时 ↑ / ↓ / Tab / Esc 照常交给 shell（历史记录等），**Enter** 从不拦截。
- 限制：只读取光标所在的这一物理行，命令折行到下一行后不再识别。

## 最近关闭

没有打开的会话时，终端区显示最近关闭的会话目录（本地 / 远程，最多 10 条，同一目录去重），点击即在该目录新建会话，悬停点 × 删除该条；由 daemon 在用户关闭会话时记录，保存在 `%APPDATA%\tether\recent.json`。

## SSH 远程会话

**会话列表操作**：

- **右键会话 → 从此目录新建**：在该会话的启动目录新建会话（daemon 不跟踪 `cd`，取的是启动目录）；远程会话用同一主机、同一目录。
- **拖出为独立窗口**：按住侧栏会话项拖到主窗口外松开，在松开处为该会话打开独立窗口，只显示终端与标题行（会话名、shell、Claude 状态），复制粘贴 / 图片粘贴 / 表格单元格选择 / 文件链接 / 联想补全与主窗口一致；该会话从主窗口列表移除。关闭独立窗口不结束会话，会话回到主窗口列表；会话被关闭或进程退出时独立窗口自动关闭。已退出的会话不能拖出。关闭主窗口即退出 GUI（连同独立窗口），不跨重启保持：下次打开所有会话都在主窗口。
  - 实现：所有窗口共用主窗口建立的一条 daemon 连接（daemon 只接受一个 GUI 连接），daemon 消息广播给所有窗口，各窗口按会话 id 过滤；交互提示、错误提示、设置由主窗口处理。daemon 只记一份“激活会话 + 窗口焦点”，由获得焦点的窗口上报，失焦延迟上报并在另一个窗口获得焦点时取消，避免互相覆盖。

点击侧栏 **＋ 新建会话** 弹出菜单：`本地` 先选目录再新建（也可用文件树的“在此目录新建会话”）；点击某台主机直接连接，不再询问任何信息，daemon 在后台连接并在远程 home 目录打开 shell。用户名、端口等连接参数统一在 **设置 → 主机** 中维护（`~/.ssh/config` 中未写 `User` 的主机和设置中新增主机均默认 `root`）。远程会话与本地会话走同一套模型（状态、屏幕快照、OSC 9/BEL 通知、关闭窗口后会话保留），侧边栏名称前显示主机标签。

- **连接复用**：daemon 内用纯 Rust 的 russh 实现，同一主机只建一条 SSH 连接（开启 keepalive），每个终端会话是其上的一个 channel；该主机最后一个会话关闭后断开连接。
- **主机来源**：
  - Tether 管理的主机：在 **设置 → 主机** 中新增 / 编辑 / 删除，保存在 `%APPDATA%\tether\config.json` 的 `hosts` 数组：`{ "id", "name", "host", "port", "user", "auth": "key" | "password", "keyPath"? }`。
  - `~/.ssh/config`：读取其中的 `Host` 条目（跳过含 `*`/`?`/`!` 的模式，只识别 `HostName`/`User`/`Port`/`IdentityFile`），合并到列表并标记 `ssh config`，认证方式默认密钥。可在设置中点 **编辑** 修改名称、主机地址、端口、用户名、认证方式、私钥路径：保存为 `config.json` 中同 id（`ssh:<alias>`）、带 `"source": "sshConfig"` 的覆盖项，整条替换解析值；点 **恢复默认** 删除覆盖项回到 ssh config 原值。Tether 不会修改 `~/.ssh/config`；该 alias 从 ssh config 删除后，对应覆盖项不再显示，下次保存设置时清除。
- **认证**：
  - `key`：依次尝试主机配置的私钥（`keyPath`）→ Windows OpenSSH ssh-agent（`\.\pipe\openssh-ssh-agent`）→ 未配置私钥时尝试 `~/.ssh/id_ed25519`、`id_ecdsa`、`id_rsa`（存在才试）。私钥有口令时弹窗输入，不保存。
  - `password`：弹窗输入，可勾选“保存密码”，保存到 **Windows 凭据管理器**（普通凭据，服务名 `tether`，账户 `user@host:port`；dev 构建服务名为 `tether-dev`，与安装版隔离）。下次优先使用已保存的密码，失败则删除该保存项并重新弹窗。服务器只开放 keyboard-interactive 时，用同一密码回应其单个密码提示。
  - **在设置中保存密码**：主机编辑框认证方式选“密码”时可直接输入密码（`type=password`、关闭自动填充），并显示“密码：已保存 / 未保存”（由 daemon 查询凭据管理器得出，只下发主机 id，不下发密码）；留空不修改已保存的密码，**清除已保存密码** 删除之。与其它主机改动一样，点设置面板的 **保存** 才生效，取消则丢弃。密码经独立消息 `SetHostPassword` 发给 daemon 只存凭据管理器，不进 `config.json`、不写日志（协议中为 `Secret` 类型，Debug 输出不含内容）。保存设置时 daemon 按主机 id 比对新旧主机列表：用户名 / 主机 / 端口变化则把密码迁到新账户（新账户已有密码不覆盖），删除主机则删除其密码；旧账户仍被其它主机使用时保留。认证方式改为密钥不删除密码（密钥失败回退密码时仍会用到），需要时点清除。ssh config 主机编辑覆盖项同样适用；**恢复默认** 本身不动密码，只有账户随之变化时才迁移到恢复后的账户。
- **主机指纹**：按 `~/.ssh/known_hosts` 校验（非 22 端口条目格式为 `[host]:port`）。未知主机弹窗显示 SHA256 指纹，确认信任后追加到 known_hosts；与记录不一致时直接拒绝连接并报错，需自行核实后手动删除 known_hosts 中的旧条目。
- 需要输入密码 / 口令 / 确认指纹但没有打开的 Tether 窗口时，连接直接失败；弹窗 2 分钟未响应视为取消。

## 文件树

终端右侧的文件树显示**当前激活会话**的目录：本地会话读本地磁盘，远程会话走同一条 SSH 连接上的 SFTP 子系统（按连接懒创建、缓存复用，不额外建连接；连接仍随该主机最后一个终端会话关闭而断开）。右上角 `›` / `‹` 收起 / 展开，状态记在 localStorage。

- **根目录**：固定为会话启动时的目录（不跟随 shell 里的 `cd`）；远程会话启动时用 SFTP 把 home（或指定目录）解析为绝对路径作为根。`↑` 返回上一级，双击文件夹进入；本地到达盘符根再上一级显示盘符列表。每个会话各自记住浏览位置。目录展开时才读取（懒加载），`⟳` 刷新。
- **右键菜单**：新建文件夹 / 新建文件、复制 / 粘贴、重命名、删除（文件夹递归删除，会先确认）、打开、下载到…、复制路径、刷新；文件夹上还有“在此目录新建会话”（本地会话 → 本地会话；远程会话 → 同主机远程会话）。
- **打开**：本地文件用系统默认程序打开；远程文件先下载到 `%TEMP%\tether\<user@host_port>\<远程路径>` 再打开（每次打开都重新下载覆盖，编辑后不会自动传回）。
- **Markdown 预览**：双击 `.md` / `.markdown` 在应用内预览（覆盖在终端区域上方，`×` 或 Esc 关闭，切换会话自动关闭）；右键提供“预览”和“用默认程序打开”。超过 2 MB 的文件不预览。渲染支持 GFM（表格、任务列表、删除线），输出经 DOMPurify 净化；http(s) 链接用默认浏览器打开，`#锚点` 在预览内跳转，相对链接不处理；图片（含外链图片）一律不加载，只显示 alt 占位。
- **HTML**：双击 `.html` / `.htm` 用**默认浏览器**打开（取 https 协议的默认程序，而非 .html 的文件关联；取不到时回退为默认程序）；右键提供“在浏览器中打开”和“用默认程序打开”。远程 html 先下载到上述临时目录再打开，其引用的相对资源（css / js / 图片）不会一并下载。
- **上传**：把文件 / 文件夹从资源管理器拖进文件树，落在文件夹上传到该文件夹，落在文件上传到其所在文件夹，落在空白处传到当前浏览目录。
- **下载**：右键“下载到…”选择本地目录，文件夹递归下载。
- 上传 / 下载遇到同名文件**直接覆盖**，同名文件夹合并；本地会话的上传 / 下载即复制。传输进度显示在文件树底部，暂不支持取消。
- **多选**：单击选中（文件夹同时展开 / 收起），**Ctrl+单击** 增减选中项，**Shift+单击** 在同一目录列表内连选。
- **复制粘贴（跨会话）**：选中文件 / 文件夹后按 **Ctrl+C** 或右键“复制”，切到任意会话（本地、任意 SSH 主机或同一会话）的文件树按 **Ctrl+V** 或右键“粘贴（N 项）”。快捷键只在文件树获得焦点时生效（单击文件树即获得焦点），终端里的 Ctrl+C / Ctrl+V 行为不变；复制不改动系统剪贴板。粘贴目标同拖入：文件夹上 → 该文件夹；文件上 → 其所在文件夹；未选中 / 空白处 → 当前浏览目录。文件夹递归复制，进度显示在底部传输列表，完成后刷新目标目录。
  - 本地 → 本地为复制，本地 ↔ 远程为上传 / 下载；远程 → 远程（含同一主机，SFTP 没有服务端复制）由 daemon 按块读出再写入，流式中转，不落临时文件。
  - 剪贴板按“位置（本地 / 主机 id）+ 绝对路径”记录，不绑定会话：源会话关闭后仍可粘贴，daemon 经连接池复用该主机连接，已断开则重新连接（已保存密码 / 密钥直接登录，否则照常弹认证提示），传完即释放。
  - **从资源管理器粘贴**：在资源管理器中复制文件 / 文件夹后，在文件树按 Ctrl+V 即上传到目标目录。判定规则：文件树复制时记下系统剪贴板序号（`GetClipboardSequenceNumber`），粘贴时若系统剪贴板中有文件（CF_HDROP）且内部剪贴板为空或序号已变化，粘贴资源管理器的文件，否则粘贴文件树内部剪贴板。
  - **同名冲突**：粘贴前检查目标目录，每个同名项弹窗选择 **覆盖 / 跳过 / 都保留**，可勾选“对其余同名项应用相同操作”，Esc 取消整个粘贴。都保留改名为 `name (1).ext`、`name (2).ext`…（以点开头的隐藏文件如 `.bashrc` → `.bashrc (1)`，文件夹 → `name (1)`）。文件夹选覆盖即**合并**，其中的同名文件直接覆盖，不再逐个询问。粘贴回源所在目录（同一位置、同一目录）时不询问，自动改名为 `name (1)`。
- 远程符号链接以斜体显示，展开时按目录尝试读取，失败则当作文件；删除符号链接只删链接本身。
