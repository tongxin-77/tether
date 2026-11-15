<p align="center">
  <img src="src-tauri/icons/128x128@2x.png" width="112" alt="Tether">
</p>

<h1 align="center">Tether</h1>

<p align="center">
  轻量多会话终端 —— 会话由后台进程持有，关闭窗口不中断；可在会话中运行 Claude Code 并接收完成通知。
</p>

<p align="center">
  <a href="https://github.com/tongxin-77/tether/releases"><img src="https://img.shields.io/github/v/release/tongxin-77/tether?label=release" alt="release"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6?logo=windows" alt="platform">
  <img src="https://img.shields.io/badge/built%20with-Tauri%202-24C8DB?logo=tauri" alt="Tauri 2">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="license"></a>
</p>

<p align="center">
  <a href="https://github.com/tongxin-77/tether/releases">下载</a> ·
  <a href="docs/usage.md">使用说明</a> ·
  <a href="docs/development.md">开发构建</a> ·
  <a href="CHANGELOG.md">更新日志</a>
</p>

---

## ✨ 亮点

| 特性 | 说明 |
|---|---|
| **会话常驻** | 全部终端会话由无窗口后台进程 `tetherd.exe` 持有，关闭窗口不中断。 |
| **Claude Code 集成** | 一键写入 hooks，侧栏实时显示思考中 / 等待确认 / 后台运行中 / 已完成，完成时弹系统通知并播放提示音。 |
| **为 Claude Code 优化的输入** | `claude` 命令参数联想补全；剪贴板是图片时 Ctrl+V 直接把图片交给 Claude Code。 |
| **SSH 远程会话** | 纯 Rust 实现，同一主机复用一条连接；支持密钥、ssh-agent、密码（存入 Windows 凭据管理器）与 `~/.ssh/config`。 |
| **文件树** | 浏览当前会话目录（远程走 SFTP），上传 / 下载、跨会话复制粘贴、Markdown 应用内预览。 |
| **多窗口与外观** | 会话可拖出为独立窗口；窗口背景透明度可调；默认 shell 可选，PowerShell 7 优先。 |
| **Windows Terminal 式复制粘贴** | 选择即复制、Ctrl+C 有选区才复制、右键粘贴，还能只选中表格的某一列单元格。 |

## 📦 安装

从 [Releases](https://github.com/tongxin-77/tether/releases) 下载 `tether_<版本>_x64-setup.exe` 安装即可（Windows 10/11，需 WebView2）。

- 默认按当前用户安装到 `%LOCALAPPDATA%\tether`，安装时自动注册登录自启与系统通知。
- 之后的版本会自动检查更新，更新时会话不中断。
- 想在会话中使用 Claude Code：直接运行 `claude`，并在 **设置 → Claude Code 集成** 点 **启用**。

## ⌨️ 常用操作

| 操作 | 效果 |
|---|---|
| 鼠标拖选 / 双击 / 三击 | 选中即复制（双击时路径、URL 整体选中） |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | 有选区时复制，无选区时照常发送中断 |
| <kbd>Ctrl</kbd>+<kbd>V</kbd> / 右键 | 粘贴；剪贴板为图片时把图片粘贴给 Claude Code |
| 在表格单元格内拖选 | 只选中该列，复制时自动拼回单元格内的折行 |
| 输入 `claude --re` 等 | 弹出参数候选，<kbd>↑</kbd> <kbd>↓</kbd> 选择，<kbd>Tab</kbd> 补全，<kbd>Esc</kbd> 关闭 |
| 拖动侧栏会话到窗口外 | 在松开处打开独立窗口；关闭后会话回到主窗口 |
| 右键会话 → 从此目录新建 | 在该会话的启动目录（远程为同主机同目录）新建会话 |
| **＋ 新建会话** | 选择本地目录，或点主机直接建立 SSH 会话 |
| 文件树中 <kbd>Ctrl</kbd>+<kbd>C</kbd> / <kbd>Ctrl</kbd>+<kbd>V</kbd> | 跨会话 / 跨主机复制文件，也可粘贴资源管理器中复制的文件 |

完整说明见 **[使用说明](docs/usage.md)**。

## 🛠️ 从源码构建

需要 Node.js + pnpm、Rust stable 与 MSVC 构建工具（见 [Tauri 前置依赖](https://tauri.app/start/prerequisites/)）。

```sh
pnpm install
cargo build -p tetherd   # 先编译 daemon
pnpm tauri dev           # 开发运行
pnpm tauri build         # 打包 NSIS 安装包
```

dev 构建与安装版的管道、配置、日志完全隔离，可同时运行。架构、打包细节与日志路径见 **[开发与构建](docs/development.md)**。

## 📚 文档

- [使用说明](docs/usage.md) —— 设置、外观、复制粘贴、通知、Claude Code 集成、SSH、文件树等
- [开发与构建](docs/development.md) —— 架构、开发运行、dev / 安装版隔离、打包、登录自启
- [需求文档](docs/README.md) —— 前端 / 后台服务 / 管理服务器 hub
- 第三方字体：界面字体 Nunito 与 JetBrains Mono 以 SIL Open Font License 1.1 授权（经 @fontsource 打包），许可全文见 [docs/licenses](docs/licenses/)
- [更新日志](CHANGELOG.md)

## 📄 许可证

[Apache-2.0](LICENSE)
