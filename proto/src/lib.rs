//! daemon 与 GUI 之间的管道协议：换行分隔的 JSON，每行一条消息，`type` 字段区分消息类型。
//! 二进制终端数据（snapshot / output）以 base64 字符串传输。

use serde::{Deserialize, Serialize};

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
    /// 主代理本轮已结束（Stop），但仍有后台子代理在运行；全部结束且主代理再次 Stop 后才算完成
    Background,
    /// 本轮回复完成（Stop，且没有后台子代理在运行）
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeState {
    pub state: ClaudeStatus,
    /// Notification 的消息文本
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Background 时仍在运行的后台子代理数
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
        Self { shell: auto_shell(), hosts: Vec::new(), appearance: Appearance::default(), sound: true, completions: Vec::new() }
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

/// GUI → daemon
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientMsg {
    List,
    /// command 仅对本地会话有效：指定时代替配置的 shell 运行该命令行（如安装 PowerShell 7 的 winget 命令）
    Create {
        target: Target,
        cols: u16,
        rows: u16,
        #[serde(default)]
        command: Option<String>,
    },
    /// daemon 回 snapshot，之后持续推送该会话 output
    Attach { id: u32 },
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
        /// hook JSON 中 background_tasks 里 type 为 subagent 的条目数；该字段缺失（旧版 Claude Code）时为 None
        #[serde(default)]
        background: Option<u32>,
        /// hook JSON 的 session_id 与 cwd：daemon 重启后据此 `claude --resume`
        #[serde(default)]
        claude_session: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        /// SessionEnd 的 reason（clear / logout / prompt_input_exit / other 等）
        #[serde(default)]
        reason: Option<String>,
    },
    /// 查询 Claude Code 集成状态，daemon 回 ClaudeHooks
    GetClaudeHooks,
    /// 启用（写入 hooks）/ 移除 Claude Code 集成，daemon 回 ClaudeHooks
    SetClaudeHooks { enabled: bool },
    /// 试听提示音（设置面板），不受 Config.sound 开关影响
    PlaySound,
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
    fn sound_defaults_on() {
        let old: super::Config = serde_json::from_str(r#"{"shell":"auto"}"#).unwrap();
        assert!(old.sound);
        assert!(super::Config::default().sound);
        let off: super::Config = serde_json::from_str(r#"{"sound":false}"#).unwrap();
        assert!(!off.sound);
        let msg: super::ClientMsg = serde_json::from_str(r#"{"type":"playSound"}"#).unwrap();
        assert!(matches!(msg, super::ClientMsg::PlaySound));
    }
}
