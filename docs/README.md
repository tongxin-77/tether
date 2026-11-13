# Tether 文档

## 使用与开发

| 文档 | 说明 |
|------|------|
| [usage.md](usage.md) | 使用说明：设置、外观、复制粘贴、通知、Claude Code 集成、补全、最近关闭、SSH、文件树、独立窗口 |
| [development.md](development.md) | 开发与构建：架构、前置依赖、开发运行、dev / 安装版隔离、单实例、打包、登录自启、日志路径 |

## 需求文档

| 文档 | 组件 | 说明 |
|------|------|------|
| [frontend-requirements.md](frontend-requirements.md) | `tether.exe`（GUI 查看器） | Tauri 2 + TypeScript + xterm.js，`src/` + `src-tauri/` |
| [daemon-requirements.md](daemon-requirements.md) | `tetherd.exe`（常驻后台进程） | 持有全部会话，`daemon/` + `proto/` |
| [hub-requirements.md](hub-requirements.md) | `tether-hub`（管理服务器） | 第二阶段，尚未开始 |

状态标记：**已实现**（代码已有且经使用验证）/ **已实现未实测**（代码已有，缺少真实环境验证）/ **计划中**（未开始）。

组件间接口：GUI ↔ daemon 走本机命名管道协议（见 [daemon §接口](daemon-requirements.md#4-接口gui--daemon-管道协议)）；daemon ↔ hub 走出站 WSS（见 [hub §接口](hub-requirements.md#4-接口daemon--hub)）。
