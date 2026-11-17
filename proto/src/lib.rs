//! daemon 与 GUI 之间的管道协议：换行分隔的 JSON，每行一条消息，`type` 字段区分消息类型。
//! 二进制终端数据（snapshot / output）以 base64 字符串传输。

use serde::{Deserialize, Serialize};

/// 管道协议版本：GUI 与 daemon 互发 Hello 时携带。0.1.0 的 daemon 不认识 Hello（视为 0），也不支持 Shutdown
pub const PROTO_VERSION: u32 = 1;

/// 本地命名管道名，按用户名区分，同一用户只会有一个 daemon；debug 构建用 `tetherd-dev-`，与安装版隔离
pub fn pipe_name() -> String {
    let prefix = if cfg!(debug_assertions) { "tetherd-dev" } else { "tetherd" };
    format!(r"\\.\pipe\{prefix}-{}", std::env::var("USERNAME").unwrap_or_default())
}

/// daemon 数据目录名（%APPDATA% 下放 config / recent，%LOCALAPPDATA% 下放日志）；debug 构建为 `tether-dev`
pub fn data_dir_name() -> &'static str {
    if cfg!(debug_assertions) { "tether-dev" } else { "tether" }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Waiting,
    Exited,
}

/// 会话元信息，由 daemon 维护（第二版也用于上报服务端）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: u32,
    pub name: String,
    pub cwd: String,
    pub command: String,
    pub status: Status,
    /// Unix 毫秒时间戳
    pub started_at: u64,
    pub last_activity_at: u64,
    /// 远程会话所在主机的显示名，本地会话为 None
    #[serde(default)]
    pub host: Option<String>,
    /// 远程会话所用主机的 id（文件树“在此目录新建会话”用），本地会话为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// 会话中 Claude Code 的状态（由 Claude hooks 上报），未运行 / 未启用集成时为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<ClaudeState>,
    /// 正被 hub 远程接管（GUI 据此显示标识与「收回」按钮）；未接管时不写出
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub taken_over: bool,
    /// 会话中 Claude Code 对话的 session_id（GUI 历史对话列表据此标注“已打开”），没有时不写出
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session: Option<String>,
}

/// 新建本地会话后恢复的 Claude Code 对话：shell 就绪后输入 `claude --resume`，带上原来的权限模式
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeResume {
    pub id: String,
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// 对话中出现过 bypassPermissions
    #[serde(default)]
    pub bypass: bool,
}

/// Claude Code 在会话中的状态
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaudeStatus {
    /// 已启动，尚未提交提示
    Idle,
    /// 已提交提示，正在处理
    Thinking,
    /// 等待用户确认 / 输入（Notification）
    Waiting,
    /// 主代理本轮已结束（Stop），但仍有后台任务（子代理、后台 shell、monitor 等）在运行；全部结束且主代理再次 Stop 后才算完成
    Background,
    /// 本轮回复完成（Stop，且没有后台任务在运行）
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeState {
    pub state: ClaudeStatus,
    /// Notification 的消息文本
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Background 时仍在运行的后台任务数
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// 状态变化时间，Unix 毫秒时间戳
    pub at: u64,
}

/// 文件树中的一项
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    /// 本地符号链接 / 联接按目标判断；远程符号链接恒为 false（GUI 展开时按目录尝试列出）
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    /// Unix 毫秒时间戳，未知为 0
    pub mtime: u64,
}

/// daemon 持有的用户配置，持久化到 %APPDATA%\tether\config.json
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// 新建会话启动的 shell 命令行，按空白拆分为程序 + 参数，双引号包裹的部分不拆；
    /// [`AUTO_SHELL`] 表示新建会话时现场选择（有 PowerShell 7 用它，否则 Windows PowerShell）；旧配置无此字段时为自动
    #[serde(default = "auto_shell")]
    pub shell: String,
    /// Tether 自己管理的 SSH 主机，以及对 ~/.ssh/config 主机的覆盖项（source 为 sshConfig，id 同为 `ssh:<alias>`）
    #[serde(default)]
    pub hosts: Vec<Host>,
    /// 外观：窗口透明度；旧配置无此字段时用默认值
    #[serde(default)]
    pub appearance: Appearance,
    /// 弹系统通知时播放内置提示音；旧配置无此字段时为开
    #[serde(default = "default_true")]
    pub sound: bool,
    /// 用户自定义联想命令（GUI 终端输入时弹出）；旧配置无此字段时为空
    #[serde(default)]
    pub completions: Vec<CompletionItem>,
    /// 管理服务器（hub）连接设置；旧配置无此字段时为未启用。token 不在这里，存 Windows 凭据管理器
    #[serde(default)]
    pub hub: HubConfig,
}

/// 管理服务器（hub）连接设置
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HubConfig {
    /// 是否启用；默认关闭，关闭时 daemon 不发起任何 hub 连接
    pub enabled: bool,
    /// 服务器地址：`host[:port]` 或 `ws(s)://host[:port]`，未写协议默认 wss
    pub address: String,
    /// 允许 `ws://` 明文连接（仅限局域网调试）
    pub allow_insecure: bool,
    /// 允许 hub 远程接管本机会话（查看并输入）；默认关闭，关闭时一律拒绝接管
    pub allow_takeover: bool,
}

/// daemon 与 hub 的连接状态
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HubState {
    /// 未启用
    #[default]
    Disabled,
    /// 连接中 / 退避等待重连中
    Connecting,
    Connected,
    /// 失败且不再自动重连（未配置、认证失败、协议不兼容等），等用户修改设置
    Failed,
}

/// 自定义联想命令：完整命令文本 + 说明
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionItem {
    pub text: String,
    #[serde(default)]
    pub desc: String,
}

fn default_true() -> bool {
    true
}

/// Config.shell 的特殊值：自动选择 shell
pub const AUTO_SHELL: &str = "auto";

fn auto_shell() -> String {
    AUTO_SHELL.into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell: auto_shell(),
            hosts: Vec::new(),
            appearance: Appearance::default(),
            sound: true,
            completions: Vec::new(),
            hub: HubConfig::default(),
        }
    }
}

/// 窗口外观；字段缺失时取默认值，其它未知字段忽略、保存时不再写出
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "RawAppearance")]
pub struct Appearance {
    /// 窗口背景不透明度（百分比），30–100；100 为完全不透明，文字与图标始终不透明
    pub opacity: u8,
}

/// 读配置用：兼容旧版背景图配置（含 background / blur）
#[derive(Deserialize)]
struct RawAppearance {
    opacity: Option<u8>,
    background: Option<serde::de::IgnoredAny>,
    blur: Option<serde::de::IgnoredAny>,
}

impl From<RawAppearance> for Appearance {
    fn from(raw: RawAppearance) -> Self {
        // 旧版 opacity 只是背景图上的面板不透明度（默认 70），沿用会让升级后的窗口突然变透明，故恢复默认
        let legacy = raw.background.is_some() || raw.blur.is_some();
        match raw.opacity {
            Some(opacity) if !legacy => Self { opacity },
            _ => Self::default(),
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self { opacity: 100 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// 私钥（指定的 keyPath 或 ~/.ssh 默认私钥）+ ssh-agent
    #[default]
    Key,
    Password,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostSource {
    /// Tether 管理，保存在 config.json
    #[default]
    Tether,
    /// 从 ~/.ssh/config 读出；出现在 Config.hosts 中时表示该主机在 Tether 内的覆盖项（不写回 ~/.ssh/config）
    SshConfig,
}

fn default_port() -> u16 {
    22
}

fn is_tether(s: &HostSource) -> bool {
    *s == HostSource::Tether
}

/// SSH 主机
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    /// Tether 管理的主机由 GUI 生成；~/.ssh/config 中的主机为 `ssh:<alias>`
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub auth: AuthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    #[serde(default, skip_serializing_if = "is_tether")]
    pub source: HostSource,
}

/// 新建会话的目标：本地目录，或远程主机（cwd 为空则进 home）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Target {
    Local { cwd: String },
    Remote {
        host_id: String,
        #[serde(default)]
        cwd: Option<String>,
    },
}

/// 最近关闭的会话目录，持久化到 %APPDATA%\tether\recent.json；以 target 去重，最新在前
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentItem {
    /// 本地为启动目录；远程为 hostId + 已解析的绝对路径，可直接用于 Create
    pub target: Target,
    /// 远程主机显示名（记录时的名称），本地为 None
    #[serde(default)]
    pub host: Option<String>,
    /// 关闭时间，Unix 毫秒时间戳
    pub closed_at: u64,
}

/// 敏感字符串（如密码）：JSON 中为普通字符串，Debug 输出不含内容，避免误打到日志
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// daemon 需要用户交互输入的类型
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PromptKind {
    Password,
    Passphrase,
    /// 未知主机指纹确认，回复 value=Some("yes") 表示信任
    HostKey,
}

/// daemon 探测到的可选 shell，供设置面板下拉选择
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellOption {
    /// 显示名称
    pub name: String,
    /// 写入 Config.shell 的命令行
    pub command: String,
}

/// FsOpen 的打开方式
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenMode {
    /// 按文件关联用系统默认程序打开
    #[default]
    Default,
    /// 用默认浏览器（https 协议的 UserChoice）打开，失败时回退为默认程序
    Browser,
}

/// FsCopy 的源位置：本地磁盘，或某台主机（按 host id 经连接池复用该主机的 SSH 连接，已断开则重新连接）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FsSource {
    Local,
    Remote { host_id: String },
}

/// FsCopy 的一项：源绝对路径；keep_both 为 true 时目标目录已有同名项则改名为 `name (1).ext`、`name (2).ext`…
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyItem {
    pub path: String,
    #[serde(default)]
    pub keep_both: bool,
}

/// 主机信息卡片：会话所在机器（本地会话为本机，远程会话为服务器）的一次采样。
/// 占用率与网速由相邻两次采样的差值计算，首次采样（或距上次过久）时为 None
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStats {
    /// CPU 型号，未知为空串
    pub cpu_model: String,
    /// 逻辑处理器数
    pub cores: u32,
    /// CPU 占用（百分比）
    pub cpu_usage: Option<f32>,
    /// 内存（字节）
    pub mem_used: u64,
    pub mem_total: u64,
    /// 本地为 DXGI 枚举到的显卡；远程只有 NVIDIA（nvidia-smi），没有时为空
    pub gpus: Vec<GpuStats>,
    /// 非回环网卡合计的下行 / 上行速率（字节/秒）
    pub net_rx: Option<u64>,
    pub net_tx: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuStats {
    pub name: String,
    /// 3D 引擎占用（百分比），取不到为 None
    pub usage: Option<f32>,
    /// 显存（字节），取不到为 None
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
}

/// GUI → daemon
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientMsg {
    List,
    /// command 仅对本地会话有效：指定时代替配置的 shell 运行该命令行（如安装 PowerShell 7 的 winget 命令）；
    /// resume 仅对本地会话有效：在 shell 中恢复该 Claude 对话
    Create {
        target: Target,
        cols: u16,
        rows: u16,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        resume: Option<ClaudeResume>,
    },
    /// daemon 回 snapshot，之后持续推送该会话 output；重复 attach 只重发 snapshot。
    /// history 为 true 时 snapshot 前拼上推出屏幕的历史（hub 接管结束后 GUI 清空终端重取）
    Attach {
        id: u32,
        #[serde(default)]
        history: bool,
    },
    Input { id: u32, data: String },
    Resize { id: u32, cols: u16, rows: u16 },
    Rename { id: u32, name: String },
    Close { id: u32 },
    /// GUI 当前激活会话与窗口焦点，用于判定是否弹系统通知
    Focus { active_id: Option<u32>, window_focused: bool },
    /// daemon 回 Config
    GetConfig,
    /// 保存配置，只影响之后新建的会话；daemon 回 Config
    SetConfig { config: Config },
    /// 设置（Some）/ 清除（None）主机的已保存密码：按 host_id 在当前配置中找到主机，存入 Windows 凭据管理器
    /// （账户 `user@host:port`），不进 config.json；daemon 回 Config
    SetHostPassword { host_id: String, password: Option<Secret> },
    /// 回复 Prompt：value 为 None 表示取消；save 仅对密码有效（保存到 Windows 凭据管理器）
    PromptReply {
        req_id: u64,
        value: Option<String>,
        #[serde(default)]
        save: bool,
    },
    // ---- 文件树：以 session_id 定位后端（本地磁盘 / 远程会话所属连接的 SFTP），daemon 以同一 req_id 回复 ----
    /// 列目录，回 FsEntries；本地会话 path 为空串时列出盘符
    FsList { req_id: u64, session_id: u32, path: String },
    /// 以下操作回 FsResult
    FsMkdir { req_id: u64, session_id: u32, path: String },
    FsCreateFile { req_id: u64, session_id: u32, path: String },
    FsRename { req_id: u64, session_id: u32, from: String, to: String },
    /// 文件夹递归删除
    FsDelete { req_id: u64, session_id: u32, path: String },
    /// 用系统默认程序 / 默认浏览器打开；远程文件先下载到 %TEMP%\tether\<user@host_port>\<远程路径>
    FsOpen {
        req_id: u64,
        session_id: u32,
        path: String,
        #[serde(default)]
        mode: OpenMode,
    },
    /// 读取文本文件（UTF-8，非法字节按 lossy 替换；超过 2MB 拒绝），回 FsContent，失败时回 FsResult
    FsRead { req_id: u64, session_id: u32, path: String },
    /// 本地文件 / 文件夹传到会话侧目录 dest_dir（同名文件覆盖、文件夹合并），过程中推送 Transfer
    Upload { req_id: u64, session_id: u32, local_paths: Vec<String>, dest_dir: String },
    /// 会话侧文件 / 文件夹传到本地目录 local_dir（同名文件覆盖、文件夹合并），过程中推送 Transfer
    Download { req_id: u64, session_id: u32, paths: Vec<String>, local_dir: String },
    /// 文件树复制粘贴：source 上的文件 / 文件夹复制到会话侧目录 dest_dir（未改名的同名文件覆盖、文件夹合并）；
    /// 两端都是远程时经 daemon 流式中转（不落临时文件）；过程中推送 Transfer
    FsCopy { req_id: u64, session_id: u32, source: FsSource, items: Vec<CopyItem>, dest_dir: String },
    /// 用默认浏览器打开 http(s) 链接（Markdown 预览中的外链），其它协议忽略；失败时回 Error
    OpenUrl { url: String },
    /// 删除一条最近关闭记录（按 target 匹配）；daemon 回 Recent
    RemoveRecent { target: Target },
    /// Claude Code hook 上报（由 `tetherd hook` 一次性连接发送，不是 GUI）：event 为 hook_event_name
    ClaudeEvent {
        session: u32,
        event: String,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        notification_type: Option<String>,
        /// SubagentStart / SubagentStop 的子代理 id 与类型（Claude Code 内部代理的类型为空串）
        #[serde(default)]
        agent_id: Option<String>,
        #[serde(default)]
        agent_type: Option<String>,
        /// hook JSON 中 background_tasks 里未结束的后台任务数（子代理、shell、monitor、teammate 等，不含 session_crons；
        /// SubagentStop 时不含该子代理自身）；该字段缺失（旧版 Claude Code）时为 None
        #[serde(default)]
        background: Option<u32>,
        /// background_tasks 各项的 `type:status`（逗号分隔），仅用于日志
        #[serde(default)]
        tasks: Option<String>,
        /// UserPromptSubmit 的 prompt 前 20 个字符（判断斜杠命令、写日志），不转发完整内容
        #[serde(default)]
        prompt: Option<String>,
        /// hook JSON 的 session_id 与 cwd：daemon 重启后据此 `claude --resume`
        #[serde(default)]
        claude_session: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        /// SessionEnd 的 reason（clear / logout / prompt_input_exit / other 等）
        #[serde(default)]
        reason: Option<String>,
        /// hook JSON 的 permission_mode（default / plan / acceptEdits / auto / dontAsk / bypassPermissions），
        /// 恢复时据此带上权限参数；部分事件不带
        #[serde(default)]
        permission_mode: Option<String>,
        /// SessionStart 的 source（startup / resume / clear / compact）
        #[serde(default)]
        source: Option<String>,
    },
    /// 查询 Claude Code 集成状态，daemon 回 ClaudeHooks
    GetClaudeHooks,
    /// 启用（写入 hooks）/ 移除 Claude Code 集成，daemon 回 ClaudeHooks
    SetClaudeHooks { enabled: bool },
    /// 试听提示音（设置面板），不受 Config.sound 开关影响
    PlaySound,
    /// GUI 连接后的首条消息：GUI 版本与协议版本，daemon 回 Hello（旧 daemon 不回复，GUI 超时后按旧版处理）
    Hello { version: String, proto: u32 },
    /// 让 daemon 保存会话列表后退出（GUI 更新后“重启后台”）；GUI 重连时拉起新 daemon 并恢复会话
    Shutdown,
    /// 采样会话所在机器的主机信息，daemon 以同一 req_id 回 HostStats（旧 daemon 不认识、不回复，GUI 超时后提示重启后台）
    HostStats { req_id: u64, session_id: u32 },
    /// 主机对话框“测试连接”：用对话框当前填写的配置（未保存）做 DNS 解析 → TCP 连接 → 主机密钥校验 → 认证，成功后立即断开，
    /// 不进连接池、不建会话。password 为对话框填写（或设置面板中尚未提交）的密码；为 None 且 use_saved 时用凭据管理器中
    /// 已保存的密码（按保存配置时的迁移规则查找）。daemon 以同一 req_id 回 TestHost（旧 daemon 不认识、不回复，GUI 超时后提示重启后台）
    TestHost {
        req_id: u64,
        host: Host,
        #[serde(default)]
        password: Option<Secret>,
        #[serde(default)]
        use_saved: bool,
    },
    /// 设置（Some）/ 清除（None）hub token：存入 Windows 凭据管理器，不进 config.json；hub 已启用时按新 token 重连；
    /// daemon 回 Config（其中 hubTokenSaved 反映结果）
    SetHubToken { token: Option<Secret> },
    /// 收回 hub 对该会话的接管：daemon 向 hub 发 detached（reclaimed）并推送 Sessions；未被接管时忽略
    HubReclaim { id: u32 },
    /// 设置面板“测试连接”：用面板当前填写的地址 / 明文开关 / token（未保存）新建一条独立连接，握手 → hello → 等 welcome
    /// 后立即关闭，不影响常驻连接。token 为 None 且 use_saved 时用凭据管理器中已保存的 token。
    /// daemon 以同一 req_id 回 TestHub（旧 daemon 不认识、不回复，GUI 超时后提示重启后台）
    TestHub {
        req_id: u64,
        address: String,
        #[serde(default)]
        allow_insecure: bool,
        #[serde(default)]
        token: Option<Secret>,
        #[serde(default)]
        use_saved: bool,
    },
}

/// daemon → GUI
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServerMsg {
    /// 会话列表全量，列表或状态变化时推送
    Sessions { sessions: Vec<SessionInfo> },
    /// 屏幕快照（base64），用于 GUI 接入时恢复画面
    Snapshot { id: u32, data: String },
    /// PTY 原始输出（base64）
    Output { id: u32, data: String },
    Notify { id: u32, message: String },
    /// 让 GUI 聚焦窗口并切到该会话
    Activate { id: u32 },
    Error { message: String },
    /// 当前配置 + 本机可选 shell（每次由 daemon 现场探测）+ 合并后的主机列表（~/.ssh/config 主机已应用覆盖项）
    /// + ~/.ssh/config 原始解析结果（设置面板“恢复默认”时显示原值）+ 凭据管理器中有已保存密码的主机 id（不含密码本身）
    Config {
        config: Config,
        shells: Vec<ShellOption>,
        hosts: Vec<Host>,
        ssh_hosts: Vec<Host>,
        #[serde(default)]
        password_saved: Vec<String>,
        /// 凭据管理器中是否有已保存的 hub token（不含 token 本身）；旧 daemon 无此字段
        #[serde(default)]
        hub_token_saved: bool,
    },
    /// 连接远程主机时需要用户输入；GUI 用 PromptReply 回复，超时视为取消
    Prompt {
        req_id: u64,
        kind: PromptKind,
        /// 主机显示，如 `user@host:port`
        host: String,
        message: String,
        /// hostKey 时为 SHA256 指纹
        #[serde(skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
    },
    /// FsList 的结果（目录在前、名称排序）；失败时改回 FsResult
    FsEntries { req_id: u64, path: String, entries: Vec<FsEntry> },
    /// FsRead 的结果
    FsContent { req_id: u64, path: String, text: String },
    /// 文件操作结果，error 为 None 表示成功
    FsResult { req_id: u64, error: Option<String> },
    /// 传输进度：name 为当前文件名，done / total 为整个请求的累计字节
    Transfer { req_id: u64, name: String, done: u64, total: u64 },
    /// 最近关闭的会话目录全量，GUI 连接时与列表变化时推送
    Recent { items: Vec<RecentItem> },
    /// Claude Code 集成状态：path 为 settings.json 路径；outdated 为已启用但缺少部分事件（旧版写入），需重新启用；
    /// error 为本次操作的错误
    ClaudeHooks {
        path: String,
        enabled: bool,
        #[serde(default)]
        outdated: bool,
        error: Option<String>,
    },
    /// 回复 GUI 的 Hello：daemon 自身版本与协议版本
    Hello { version: String, proto: u32 },
    /// HostStats 的结果：成功时 stats 有值，失败（会话不存在、远程非 Linux、SSH 断开等）时 error 有值
    HostStats { req_id: u64, stats: Option<HostStats>, error: Option<String> },
    /// TestHost 的结果：成功时 elapsed_ms 为耗时（毫秒），失败时 error 为按阶段区分的原因
    TestHost { req_id: u64, elapsed_ms: Option<u64>, error: Option<String> },
    /// hub 连接状态：GUI 请求配置时随 Config 推送，之后状态或原因变化时推送。
    /// error 为失败原因（failed），或退避等待中上一次失败的原因（connecting）
    HubStatus { state: HubState, error: Option<String> },
    /// TestHub 的结果：成功时 elapsed_ms 为耗时（毫秒）、hub_version 为 hub 版本；失败时 error 为按阶段区分的原因
    TestHub { req_id: u64, elapsed_ms: Option<u64>, hub_version: Option<String>, error: Option<String> },
    /// 本地会话进程树的内存占用（专用工作集，字节）全量，有 GUI 连接时周期采样、变化才推送；
    /// 不在列表中的会话不显示。旧 GUI 不认识此类型，解析失败即丢弃
    Memory { items: Vec<SessionMemory> },
}

/// 单个本地会话的内存占用
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMemory {
    pub id: u32,
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    // 测试以 debug 构建运行，应落到 dev 隔离名
    #[test]
    fn debug_build_uses_dev_names() {
        assert!(super::pipe_name().starts_with(r"\\.\pipe\tetherd-dev-"));
        assert_eq!(super::data_dir_name(), "tether-dev");
    }

    #[test]
    fn hello_and_shutdown_wire_format() {
        let hello = super::ClientMsg::Hello { version: "0.1.1".into(), proto: super::PROTO_VERSION };
        assert_eq!(serde_json::to_string(&hello).unwrap(), r#"{"type":"hello","version":"0.1.1","proto":1}"#);
        let shutdown: super::ClientMsg = serde_json::from_str(r#"{"type":"shutdown"}"#).unwrap();
        assert!(matches!(shutdown, super::ClientMsg::Shutdown));
        let reply: super::ServerMsg = serde_json::from_str(r#"{"type":"hello","version":"0.1.1","proto":1}"#).unwrap();
        assert!(matches!(reply, super::ServerMsg::Hello { proto: 1, .. }));
    }

    #[test]
    fn attach_history_defaults_false() {
        let msg: super::ClientMsg = serde_json::from_str(r#"{"type":"attach","id":3}"#).unwrap();
        assert!(matches!(msg, super::ClientMsg::Attach { id: 3, history: false }));
        let msg: super::ClientMsg = serde_json::from_str(r#"{"type":"attach","id":3,"history":true}"#).unwrap();
        assert!(matches!(msg, super::ClientMsg::Attach { id: 3, history: true }));
    }

    #[test]
    fn create_resume_json() {
        let json = r#"{"type":"create","target":{"kind":"local","cwd":"C:\\p"},"cols":80,"rows":24,"resume":{"id":"a1","permissionMode":"plan","bypass":true}}"#;
        let msg: super::ClientMsg = serde_json::from_str(json).unwrap();
        let super::ClientMsg::Create { resume: Some(r), .. } = msg else { panic!() };
        assert_eq!((r.id.as_str(), r.permission_mode.as_deref(), r.bypass), ("a1", Some("plan"), true));
        // 不带 resume / 权限字段时为空
        let msg: super::ClientMsg = serde_json::from_str(r#"{"type":"create","target":{"kind":"local","cwd":""},"cols":80,"rows":24}"#).unwrap();
        assert!(matches!(msg, super::ClientMsg::Create { resume: None, .. }));
        let r: super::ClaudeResume = serde_json::from_str(r#"{"id":"a1"}"#).unwrap();
        assert_eq!((r.permission_mode, r.bypass), (None, false));
    }

    #[test]
    fn session_info_claude_session() {
        let mut info: super::SessionInfo =
            serde_json::from_str(r#"{"id":1,"name":"p","cwd":"C:\\p","command":"pwsh","status":"running","startedAt":0,"lastActivityAt":0}"#).unwrap();
        assert_eq!(info.claude_session, None);
        assert!(!serde_json::to_string(&info).unwrap().contains("claudeSession"));
        info.claude_session = Some("a1".into());
        assert!(serde_json::to_string(&info).unwrap().contains(r#""claudeSession":"a1""#));
    }

    #[test]
    fn host_password_not_in_debug() {
        let json = r#"{"type":"setHostPassword","hostId":"h1","password":"s3cret"}"#;
        let msg: super::ClientMsg = serde_json::from_str(json).unwrap();
        assert!(!format!("{msg:?}").contains("s3cret"));
        assert_eq!(serde_json::to_string(&msg).unwrap(), json);
        let clear: super::ClientMsg = serde_json::from_str(r#"{"type":"setHostPassword","hostId":"h1","password":null}"#).unwrap();
        assert!(matches!(clear, super::ClientMsg::SetHostPassword { password: None, .. }));
    }

    #[test]
    fn fs_copy_json() {
        let json = r#"{"type":"fsCopy","reqId":1,"sessionId":2,"source":{"kind":"remote","hostId":"h1"},"items":[{"path":"/a","keepBoth":true},{"path":"/b"}],"destDir":"C:\\x"}"#;
        let msg: super::ClientMsg = serde_json::from_str(json).unwrap();
        let super::ClientMsg::FsCopy { source, items, .. } = msg else { panic!() };
        assert_eq!(source, super::FsSource::Remote { host_id: "h1".into() });
        assert!(items[0].keep_both && !items[1].keep_both);
        let local: super::FsSource = serde_json::from_str(r#"{"kind":"local"}"#).unwrap();
        assert_eq!(local, super::FsSource::Local);
    }

    #[test]
    fn host_stats_json() {
        let req: super::ClientMsg = serde_json::from_str(r#"{"type":"hostStats","reqId":1,"sessionId":2}"#).unwrap();
        assert!(matches!(req, super::ClientMsg::HostStats { req_id: 1, session_id: 2 }));
        let stats = super::HostStats { cores: 8, cpu_usage: Some(12.5), gpus: vec![Default::default()], ..Default::default() };
        let reply = super::ServerMsg::HostStats { req_id: 1, stats: Some(stats), error: None };
        let v = serde_json::to_value(&reply).unwrap();
        assert_eq!(v["type"], "hostStats");
        assert_eq!(v["stats"]["cpuUsage"], 12.5);
        assert_eq!(v["stats"]["gpus"][0]["memTotal"], serde_json::Value::Null);
        assert!(v["stats"]["netRx"].is_null());
    }

    #[test]
    fn test_host_json() {
        let json = r#"{"type":"testHost","reqId":7,"host":{"id":"h1","name":"n","host":"example.com","port":2222,"user":"u","auth":"password"},"password":"s3cret","useSaved":false}"#;
        let msg: super::ClientMsg = serde_json::from_str(json).unwrap();
        assert!(!format!("{msg:?}").contains("s3cret"));
        let super::ClientMsg::TestHost { req_id: 7, host, password: Some(p), use_saved: false } = msg else { panic!() };
        assert_eq!((host.port, host.auth, p.0.as_str()), (2222, super::AuthKind::Password, "s3cret"));
        // 不带密码：password / useSaved 缺省
        let saved: super::ClientMsg =
            serde_json::from_str(r#"{"type":"testHost","reqId":1,"host":{"id":"","name":"","host":"h","user":"u"},"password":null,"useSaved":true}"#).unwrap();
        assert!(matches!(saved, super::ClientMsg::TestHost { password: None, use_saved: true, .. }));
        let ok = super::ServerMsg::TestHost { req_id: 7, elapsed_ms: Some(120), error: None };
        assert_eq!(serde_json::to_string(&ok).unwrap(), r#"{"type":"testHost","reqId":7,"elapsedMs":120,"error":null}"#);
    }

    #[test]
    fn legacy_appearance_fields_ignored() {
        // 旧版背景图配置：background / blur 被忽略，旧 opacity（面板不透明度）不沿用为窗口透明度
        let old = r#"{"shell":"auto","appearance":{"background":"background-1.png","blur":16,"opacity":70}}"#;
        let config: super::Config = serde_json::from_str(old).unwrap();
        assert_eq!(config.appearance.opacity, 100);
        assert_eq!(serde_json::to_value(&config.appearance).unwrap(), serde_json::json!({ "opacity": 100 }));
        let old_null: super::Config = serde_json::from_str(r#"{"appearance":{"background":null,"blur":16,"opacity":70}}"#).unwrap();
        assert_eq!(old_null.appearance.opacity, 100);
        let new: super::Config = serde_json::from_str(r#"{"appearance":{"opacity":60}}"#).unwrap();
        assert_eq!(new.appearance.opacity, 60);
        let none: super::Config = serde_json::from_str(r#"{"shell":"auto"}"#).unwrap();
        assert_eq!(none.appearance.opacity, 100);
    }

    #[test]
    fn hub_config_defaults_off() {
        let old: super::Config = serde_json::from_str(r#"{"shell":"auto"}"#).unwrap();
        assert_eq!(old.hub, super::HubConfig::default());
        assert!(!old.hub.enabled);
        let v = serde_json::to_value(super::Config::default()).unwrap();
        assert_eq!(v["hub"], serde_json::json!({ "enabled": false, "address": "", "allowInsecure": false, "allowTakeover": false }));
        let partial: super::Config = serde_json::from_str(r#"{"hub":{"enabled":true,"address":"h:1"}}"#).unwrap();
        assert_eq!((partial.hub.enabled, partial.hub.address.as_str(), partial.hub.allow_insecure), (true, "h:1", false));
        assert!(!partial.hub.allow_takeover);
    }

    #[test]
    fn hub_messages_json() {
        let json = r#"{"type":"setHubToken","token":"tok-123"}"#;
        let msg: super::ClientMsg = serde_json::from_str(json).unwrap();
        assert!(!format!("{msg:?}").contains("tok-123"));
        assert_eq!(serde_json::to_string(&msg).unwrap(), json);
        let clear: super::ClientMsg = serde_json::from_str(r#"{"type":"setHubToken","token":null}"#).unwrap();
        assert!(matches!(clear, super::ClientMsg::SetHubToken { token: None }));

        let json = r#"{"type":"testHub","reqId":3,"address":"wss://h","allowInsecure":true,"token":"tok-123","useSaved":false}"#;
        let msg: super::ClientMsg = serde_json::from_str(json).unwrap();
        assert!(!format!("{msg:?}").contains("tok-123"));
        let super::ClientMsg::TestHub { req_id: 3, allow_insecure: true, token: Some(t), use_saved: false, .. } = msg else { panic!() };
        assert_eq!(t.0, "tok-123");
        let saved: super::ClientMsg = serde_json::from_str(r#"{"type":"testHub","reqId":1,"address":"h","useSaved":true}"#).unwrap();
        assert!(matches!(saved, super::ClientMsg::TestHub { token: None, use_saved: true, allow_insecure: false, .. }));

        let ok = super::ServerMsg::TestHub { req_id: 3, elapsed_ms: Some(42), hub_version: Some("0.1.0".into()), error: None };
        assert_eq!(serde_json::to_string(&ok).unwrap(), r#"{"type":"testHub","reqId":3,"elapsedMs":42,"hubVersion":"0.1.0","error":null}"#);
        let st = super::ServerMsg::HubStatus { state: super::HubState::Failed, error: Some("认证失败".into()) };
        assert_eq!(serde_json::to_string(&st).unwrap(), r#"{"type":"hubStatus","state":"failed","error":"认证失败"}"#);
        // 旧 daemon 的 Config 没有 hubTokenSaved
        let cfg: super::ServerMsg = serde_json::from_str(r#"{"type":"config","config":{},"shells":[],"hosts":[],"sshHosts":[]}"#).unwrap();
        assert!(matches!(cfg, super::ServerMsg::Config { hub_token_saved: false, .. }));

        let reclaim: super::ClientMsg = serde_json::from_str(r#"{"type":"hubReclaim","id":3}"#).unwrap();
        assert!(matches!(reclaim, super::ClientMsg::HubReclaim { id: 3 }));
    }

    #[test]
    fn sound_defaults_on() {
        let old: super::Config = serde_json::from_str(r#"{"shell":"auto"}"#).unwrap();
        assert!(old.sound);
        assert!(super::Config::default().sound);
        let off: super::Config = serde_json::from_str(r#"{"sound":false}"#).unwrap();
        assert!(!off.sound);
        let msg: super::ClientMsg = serde_json::from_str(r#"{"type":"playSound"}"#).unwrap();
        assert!(matches!(msg, super::ClientMsg::PlaySound));
    }

    #[test]
    fn memory_json() {
        let msg = super::ServerMsg::Memory { items: vec![super::SessionMemory { id: 2, bytes: 1 << 30 }] };
        assert_eq!(serde_json::to_string(&msg).unwrap(), r#"{"type":"memory","items":[{"id":2,"bytes":1073741824}]}"#);
    }
}
