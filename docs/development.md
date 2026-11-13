# 开发与构建

> 从根目录 [README](../README.md) 迁移而来。使用说明见 [usage.md](usage.md)，版本变更见 [CHANGELOG.md](../CHANGELOG.md)。

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
