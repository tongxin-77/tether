# 更新日志

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

## [0.1.0] - 2026-11-13

首个发布版本。

### 新增

- 多会话终端：Tauri 2 + xterm.js + ConPTY，会话由后台进程 `tetherd` 持有，关闭窗口不中断；登录自启（`tetherd --install`）。
- 单实例窗口，点击通知可拉起并切到对应会话；debug 构建与安装版隔离，可同时运行。
- 设置：新建会话的默认 shell（自动 / Windows PowerShell / PowerShell 7 / 命令提示符 / Git Bash / WSL），未安装 PowerShell 7 时提示安装；外观（透明度等）。
- 复制粘贴：选择即复制、有选区时 Ctrl+C 复制、Ctrl+V / 右键粘贴（bracketed paste）。
- 剪贴板为图片时 Ctrl+V 触发 Claude Code 的图片粘贴（本地会话发送 Alt+V）。
- 表格单元格内拖选：选区限制在框线表格的单元格所在列，复制时把折行拼回一段。
- 完成通知：检测 OSC 9 / BEL 发系统通知。
- Claude Code 集成：一键写入 / 移除 hooks，侧栏显示运行状态、后台子代理数，等待确认时通知；`claude` 命令补全。
- 最近关闭的会话目录，可一键重开。
- SSH 远程会话：读取 `~/.ssh/config`，连接复用，`known_hosts` 指纹校验。
- 文件树：会话目录浏览，新建 / 重命名 / 删除 / 跨会话复制粘贴，上传下载，Markdown 应用内预览，HTML 用浏览器打开；终端输出中的文件路径可单击打开。
- 会话右键“从此目录新建”。
- 拖动侧栏会话到主窗口外，成为只含该会话的独立窗口；关闭后回到主窗口。
- 应用图标。

[未发布]: https://github.com/tongxin-77/tether/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tongxin-77/tether/releases/tag/v0.1.0
