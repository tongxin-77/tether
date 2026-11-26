//! daemon 面向用户的文字（系统通知、发往 GUI / hub 的错误与提示）的多语言词条。
//! 当前语言由 GUI 以 SetLocale 设置并持久化到 config.json（Config.locale），默认 zh-CN；daemon.log 日志不翻译。
//! 用法：`tr!("ssh.password_prompt", host = label)`，模板中的 `{host}` 被替换。

use std::{
    fmt::Display,
    sync::atomic::{AtomicU8, Ordering},
};

pub use proto::i18n::Locale;
use proto::i18n::{Entry, fill, lookup};

/// 取当前语言的词条并替换占位符：`tr!("key")`、`tr!("key", name = value, ...)`
macro_rules! tr {
    ($key:literal) => {
        $crate::i18n::tr($key, &[])
    };
    ($key:literal, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::i18n::tr($key, &[$((stringify!($name), &$val as &dyn std::fmt::Display)),+])
    };
}

static CURRENT: AtomicU8 = AtomicU8::new(Locale::ZhCn as u8);

/// 设置当前语言（任意 BCP 47 代码按回退链归到支持的语言），返回归一后的语言
pub fn set(code: &str) -> Locale {
    let l = Locale::parse(code);
    CURRENT.store(l as u8, Ordering::Relaxed);
    l
}

pub fn current() -> Locale {
    Locale::from_index(CURRENT.load(Ordering::Relaxed))
}

/// 当前语言的词条（key 也可以是运行时选定的词条键，如连接阶段）
pub fn tr(key: &'static str, args: &[(&str, &dyn Display)]) -> String {
    fill(lookup(TABLE, current(), key), args)
}

/// 词条表：(键, [zh-CN, en, zh-TW, ja])
pub static TABLE: &[Entry] = &[
    // ---------------- 通用 ----------------
    ("common.sep", ["；", "; ", "；", "。"]),
    ("common.list_sep", ["、", ", ", "、", "、"]),
    ("common.no_appdata", ["未找到 APPDATA 环境变量", "APPDATA environment variable not found", "找不到 APPDATA 環境變數", "環境変数 APPDATA が見つかりません"]),
    ("common.no_home", ["未找到用户目录", "User profile folder not found", "找不到使用者目錄", "ユーザー フォルダーが見つかりません"]),
    ("common.ssh_disconnected", ["SSH 连接已断开", "SSH connection lost", "SSH 連線已中斷", "SSH 接続が切断されました"]),
    ("common.dir_not_found", ["目录不存在：{dir}", "Directory does not exist: {dir}", "目錄不存在：{dir}", "ディレクトリが存在しません：{dir}"]),
    ("common.host_not_found", ["未找到主机 {host}", "Host not found: {host}", "找不到主機 {host}", "ホスト {host} が見つかりません"]),
    ("common.session_gone", ["会话已不存在", "The session no longer exists", "工作階段已不存在", "セッションはすでに存在しません"]),
    ("common.invalid_address", ["地址无效：{error}", "Invalid address: {error}", "位址無效：{error}", "アドレスが無効です：{error}"]),
    ("common.open_channel_failed", ["打开 channel 失败：{error}", "Failed to open channel: {error}", "開啟 channel 失敗：{error}", "channel を開けませんでした：{error}"]),
    ("common.open_sftp_channel_failed", ["打开 SFTP channel 失败：{error}", "Failed to open SFTP channel: {error}", "開啟 SFTP channel 失敗：{error}", "SFTP channel を開けませんでした：{error}"]),
    (
        "common.connect_timeout_stage",
        [
            "连接超时：{stage}阶段 {secs} 秒内未完成",
            "Connection timed out: {stage} did not complete within {secs} s",
            "連線逾時：{stage}階段 {secs} 秒內未完成",
            "接続タイムアウト：{stage}が {secs} 秒以内に完了しませんでした",
        ],
    ),
    // 连接阶段（超时提示中使用）
    ("stage.dns", ["DNS 解析", "DNS lookup", "DNS 解析", "DNS 名前解決"]),
    ("stage.tcp", ["TCP 连接", "TCP connection", "TCP 連線", "TCP 接続"]),
    ("stage.ssh", ["SSH 握手", "SSH handshake", "SSH 交握", "SSH ハンドシェイク"]),
    ("stage.auth", ["认证", "authentication", "驗證", "認証"]),
    ("stage.tls", ["TLS 握手", "TLS handshake", "TLS 交握", "TLS ハンドシェイク"]),
    ("stage.ws", ["WebSocket 握手", "WebSocket handshake", "WebSocket 交握", "WebSocket ハンドシェイク"]),
    ("stage.connect", ["连接", "connection", "連線", "接続"]),
    // ---------------- 系统通知 ----------------
    ("notify.attention", ["会话需要你的关注", "Session needs your attention", "工作階段需要你的注意", "セッションが対応を待っています"]),
    ("notify.claude_attention", ["Claude 需要你的关注", "Claude needs your attention", "Claude 需要你的注意", "Claude が対応を待っています"]),
    ("notify.task_done", ["任务已完成", "Task completed", "任務已完成", "タスクが完了しました"]),
    // ---------------- 会话 / 设置 ----------------
    ("session.save_config_failed", ["保存配置失败：{error}", "Failed to save settings: {error}", "儲存設定失敗：{error}", "設定を保存できませんでした：{error}"]),
    ("session.save_password_failed", ["保存密码失败：{error}", "Failed to save password: {error}", "儲存密碼失敗：{error}", "パスワードを保存できませんでした：{error}"]),
    ("session.save_hub_token_failed", ["保存 hub token 失败：{error}", "Failed to save hub token: {error}", "儲存 hub token 失敗：{error}", "hub token を保存できませんでした：{error}"]),
    ("session.source_host_not_found", ["未找到源主机 {host}", "Source host not found: {host}", "找不到來源主機 {host}", "コピー元ホスト {host} が見つかりません"]),
    ("session.shell_start_failed", ["启动 shell 失败（{shell}）：{error}", "Failed to start shell ({shell}): {error}", "啟動 shell 失敗（{shell}）：{error}", "shell を起動できませんでした（{shell}）：{error}"]),
    ("session.no_default_shell", ["默认 shell 未配置", "No default shell configured", "未設定預設 shell", "既定の shell が設定されていません"]),
    ("shell.auto", ["自动（PowerShell 7 优先）", "Auto (prefer PowerShell 7)", "自動（優先使用 PowerShell 7）", "自動（PowerShell 7 を優先）"]),
    ("shell.cmd", ["命令提示符", "Command Prompt", "命令提示字元", "コマンド プロンプト"]),
    // ---------------- Claude Code 集成 ----------------
    ("claude.exe_path_failed", ["无法获取 tetherd 路径：{error}", "Cannot determine tetherd path: {error}", "無法取得 tetherd 路徑：{error}", "tetherd のパスを取得できません：{error}"]),
    ("claude.read_failed", ["读取 {path} 失败：{error}", "Failed to read {path}: {error}", "讀取 {path} 失敗：{error}", "{path} を読み込めませんでした：{error}"]),
    ("claude.not_object", ["{path} 顶层不是 JSON 对象，未修改", "{path} is not a JSON object at the top level; left unchanged", "{path} 頂層不是 JSON 物件，未修改", "{path} の最上位が JSON オブジェクトではないため、変更していません"]),
    ("claude.invalid_json", ["{path} 不是合法 JSON（{error}），未修改", "{path} is not valid JSON ({error}); left unchanged", "{path} 不是有效的 JSON（{error}），未修改", "{path} は有効な JSON ではないため（{error}）、変更していません"]),
    ("claude.backup_failed", ["备份失败：{error}", "Backup failed: {error}", "備份失敗：{error}", "バックアップに失敗しました：{error}"]),
    ("claude.write_failed", ["写入 {path} 失败：{error}", "Failed to write {path}: {error}", "寫入 {path} 失敗：{error}", "{path} に書き込めませんでした：{error}"]),
    ("claude.hooks_not_object", ["settings.json 中的 hooks 不是对象，未修改", "hooks in settings.json is not an object; left unchanged", "settings.json 中的 hooks 不是物件，未修改", "settings.json の hooks がオブジェクトではないため、変更していません"]),
    (
        "claude.hooks_event_not_array",
        [
            "settings.json 中的 hooks.{event} 不是数组，未修改",
            "hooks.{event} in settings.json is not an array; left unchanged",
            "settings.json 中的 hooks.{event} 不是陣列，未修改",
            "settings.json の hooks.{event} が配列ではないため、変更していません",
        ],
    ),
    // ---------------- 文件面板 ----------------
    ("fs.exists", ["{path}：已存在", "{path}: already exists", "{path}：已存在", "{path}：すでに存在します"]),
    (
        "fs.too_large",
        [
            "{path}：文件超过 {mb} MB，不支持预览，请用默认程序打开",
            "{path}: file is larger than {mb} MB and cannot be previewed; open it with the default app",
            "{path}：檔案超過 {mb} MB，不支援預覽，請用預設程式開啟",
            "{path}：ファイルが {mb} MB を超えているためプレビューできません。既定のアプリで開いてください",
        ],
    ),
    ("fs.binary", ["{path}：看起来是二进制文件，无法以文本打开", "{path}: looks like a binary file and cannot be opened as text", "{path}：看起來是二進位檔案，無法以文字開啟", "{path}：バイナリ ファイルのようなので、テキストとして開けません"]),
    (
        "fs.unencodable",
        [
            "第 {line} 行的字符“{char}”无法用文件原编码（系统代码页 {codepage}）保存，未写入；删去该字符后再保存",
            "Character “{char}” on line {line} cannot be saved in the file's original encoding (system code page {codepage}); nothing was written. Remove the character and save again",
            "第 {line} 行的字元「{char}」無法用檔案原編碼（系統字碼頁 {codepage}）儲存，未寫入；刪除該字元後再儲存",
            "{line} 行目の文字「{char}」はファイル元のエンコード（システム コード ページ {codepage}）で保存できないため、書き込みませんでした。この文字を削除してから保存してください",
        ],
    ),
    ("fs.readonly", ["{path}：文件为只读，未写入", "{path}: file is read-only; nothing was written", "{path}：檔案為唯讀，未寫入", "{path}：ファイルが読み取り専用のため、書き込みませんでした"]),
    ("fs.bad_browser_cmd", ["无法解析浏览器命令行：{cmdline}", "Cannot parse browser command line: {cmdline}", "無法解析瀏覽器命令列：{cmdline}", "ブラウザーのコマンド ラインを解析できません：{cmdline}"]),
    ("fs.launch_failed", ["启动 {exe} 失败：{error}", "Failed to start {exe}: {error}", "啟動 {exe} 失敗：{error}", "{exe} を起動できませんでした：{error}"]),
    ("fs.no_browser", ["未找到默认浏览器", "Default browser not found", "找不到預設瀏覽器", "既定のブラウザーが見つかりません"]),
    ("fs.read_open_cmd_failed", ["读取 {prog_id} 的打开命令失败：{error}", "Failed to read the open command of {prog_id}: {error}", "讀取 {prog_id} 的開啟命令失敗：{error}", "{prog_id} の起動コマンドを読み込めませんでした：{error}"]),
    ("fs.open_failed", ["打开 {path} 失败：{error}", "Failed to open {path}: {error}", "開啟 {path} 失敗：{error}", "{path} を開けませんでした：{error}"]),
    ("fs.whole_disk", ["{path}：不支持传输整个磁盘", "{path}: transferring an entire drive is not supported", "{path}：不支援傳輸整個磁碟", "{path}：ドライブ全体の転送はサポートされていません"]),
    ("fs.root_dir", ["{path}：不支持传输根目录", "{path}: transferring the root directory is not supported", "{path}：不支援傳輸根目錄", "{path}：ルート ディレクトリの転送はサポートされていません"]),
    ("fs.no_new_name", ["{name}：找不到可用的新名称", "{name}: no available new name found", "{name}：找不到可用的新名稱", "{name}：使用できる新しい名前が見つかりません"]),
    // ---------------- 以 root 浏览（sudo） ----------------
    ("sudo.need_password", ["sudo 需要密码", "sudo requires a password", "sudo 需要密碼", "sudo にはパスワードが必要です"]),
    ("sudo.wrong_password", ["sudo 密码错误", "Incorrect sudo password", "sudo 密碼錯誤", "sudo のパスワードが違います"]),
    ("sudo.not_allowed", ["当前用户不在 sudoers 中，无权使用 sudo", "The current user is not in sudoers and may not use sudo", "目前使用者不在 sudoers 中，無權使用 sudo", "現在のユーザーは sudoers に含まれていないため、sudo を使用できません"]),
    (
        "sudo.requiretty",
        [
            "sudoers 要求在终端中使用 sudo（requiretty），无法用于文件面板",
            "sudoers requires sudo to run in a terminal (requiretty), so it cannot be used by the file panel",
            "sudoers 要求在終端機中使用 sudo（requiretty），無法用於檔案面板",
            "sudoers が端末での sudo 実行を要求している（requiretty）ため、ファイル パネルでは使用できません",
        ],
    ),
    ("sudo.no_sudo", ["远程主机没有安装 sudo", "sudo is not installed on the remote host", "遠端主機未安裝 sudo", "リモート ホストに sudo がインストールされていません"]),
    ("sudo.no_sftp_server", ["远程主机上找不到 sftp-server（已尝试 {paths}）", "sftp-server not found on the remote host (tried {paths})", "遠端主機上找不到 sftp-server（已嘗試 {paths}）", "リモート ホストに sftp-server が見つかりません（試行：{paths}）"]),
    ("sudo.failed", ["sudo 失败：{error}", "sudo failed: {error}", "sudo 失敗：{error}", "sudo に失敗しました：{error}"]),
    ("sudo.local", ["本地会话不支持以 root 浏览", "Browsing as root is not supported for local sessions", "本機工作階段不支援以 root 瀏覽", "ローカル セッションでは root として参照できません"]),
    ("sudo.prompt", ["请输入 {host} 的 sudo 密码，用于以 root 浏览文件", "Enter the sudo password for {host} to browse files as root", "請輸入 {host} 的 sudo 密碼，用於以 root 瀏覽檔案", "root としてファイルを参照するため、{host} の sudo パスワードを入力してください"]),
    ("sudo.saved_rejected", ["已保存的登录密码不能用于 sudo，请输入 {host} 的 sudo 密码", "The saved login password does not work for sudo; enter the sudo password for {host}", "已儲存的登入密碼無法用於 sudo，請輸入 {host} 的 sudo 密碼", "保存済みのログイン パスワードは sudo に使用できません。{host} の sudo パスワードを入力してください"]),
    ("sudo.retry", ["sudo 密码错误，请重新输入 {host} 的 sudo 密码", "Incorrect sudo password; enter the sudo password for {host} again", "sudo 密碼錯誤，請重新輸入 {host} 的 sudo 密碼", "sudo のパスワードが違います。{host} の sudo パスワードをもう一度入力してください"]),
    ("sudo.too_many", ["sudo 密码错误次数过多", "Too many incorrect sudo password attempts", "sudo 密碼錯誤次數過多", "sudo のパスワード入力の失敗回数が多すぎます"]),
    ("sudo.exec_failed", ["执行 sudo 失败：{error}", "Failed to run sudo: {error}", "執行 sudo 失敗：{error}", "sudo を実行できませんでした：{error}"]),
    ("sudo.write_password_failed", ["写入 sudo 密码失败：{error}", "Failed to send the sudo password: {error}", "寫入 sudo 密碼失敗：{error}", "sudo パスワードを送信できませんでした：{error}"]),
    ("sudo.timeout", ["等待 sudo 响应超时", "Timed out waiting for sudo", "等待 sudo 回應逾時", "sudo の応答待ちがタイムアウトしました"]),
    ("sudo.root_sftp_failed", ["启动 root SFTP 失败：{error}", "Failed to start root SFTP: {error}", "啟動 root SFTP 失敗：{error}", "root SFTP を起動できませんでした：{error}"]),
    ("sudo.root_sftp_timeout", ["启动 root SFTP 超时", "Timed out starting root SFTP", "啟動 root SFTP 逾時", "root SFTP の起動がタイムアウトしました"]),
    ("sudo.exit_code", ["退出码 {code}", "exit code {code}", "結束代碼 {code}", "終了コード {code}"]),
    ("sudo.no_exit_status", ["未返回退出状态", "no exit status returned", "未回傳結束狀態", "終了ステータスが返されませんでした"]),
    // ---------------- 主机信息 ----------------
    ("stats.not_linux", ["不支持：远程主机不是 Linux（无 /proc/stat）", "Not supported: the remote host is not Linux (no /proc/stat)", "不支援：遠端主機不是 Linux（無 /proc/stat）", "サポートされていません：リモート ホストが Linux ではありません（/proc/stat なし）"]),
    ("stats.cpu_failed", ["读取 CPU 时间失败", "Failed to read CPU times", "讀取 CPU 時間失敗", "CPU 時間を取得できませんでした"]),
    ("stats.mem_failed", ["读取内存信息失败", "Failed to read memory information", "讀取記憶體資訊失敗", "メモリ情報を取得できませんでした"]),
    // ---------------- git（远程） ----------------
    ("git.local_only", ["本地会话的 git 由界面直接查询", "Git for local sessions is queried by the app directly", "本機工作階段的 git 由介面直接查詢", "ローカル セッションの git はアプリが直接取得します"]),
    ("git.not_installed", ["远程主机未安装 git", "git is not installed on the remote host", "遠端主機未安裝 git", "リモート ホストに git がインストールされていません"]),
    ("git.not_repo", ["不是 git 仓库：{dir}", "Not a git repository: {dir}", "不是 git 儲存庫：{dir}", "git リポジトリではありません：{dir}"]),
    (
        "git.unsafe_dir",
        [
            "git 拒绝访问该仓库（所有者不是当前用户），可在远程执行 git config --global --add safe.directory {dir}",
            "git refused to access the repository (owned by another user); run git config --global --add safe.directory {dir} on the remote host",
            "git 拒絕存取該儲存庫（擁有者不是目前使用者），可在遠端執行 git config --global --add safe.directory {dir}",
            "git がリポジトリへのアクセスを拒否しました（所有者が現在のユーザーではありません）。リモートで git config --global --add safe.directory {dir} を実行してください",
        ],
    ),
    ("git.status_failed", ["git status 执行失败（需要 git 2.11 及以上）", "git status failed (git 2.11 or later is required)", "git status 執行失敗（需要 git 2.11 以上）", "git status に失敗しました（git 2.11 以降が必要です）"]),
    ("git.failed", ["git 执行失败：{error}", "git failed: {error}", "git 執行失敗：{error}", "git の実行に失敗しました：{error}"]),
    // ---------------- SSH ----------------
    ("ssh.exec_failed", ["执行失败：{error}", "Command failed: {error}", "執行失敗：{error}", "実行に失敗しました：{error}"]),
    ("ssh.exec_timeout", ["远程命令 {secs} 秒内未完成", "Remote command did not finish within {secs} s", "遠端命令 {secs} 秒內未完成", "リモート コマンドが {secs} 秒以内に完了しませんでした"]),
    (
        "ssh.no_prompt",
        [
            "网页新建不弹提示，请先在桌面端连接一次（保存密码 / 信任主机指纹）或配置免口令密钥",
            "Sessions created from the web cannot show prompts. Connect once from the desktop app first (save the password / trust the host fingerprint), or set up a key without a passphrase",
            "網頁新建不會彈出提示，請先在桌面端連線一次（儲存密碼 / 信任主機指紋）或設定免密碼短語的金鑰",
            "Web から作成したセッションではプロンプトを表示できません。先にデスクトップ アプリで一度接続する（パスワードを保存 / ホストのフィンガープリントを信頼）か、パスフレーズなしの鍵を設定してください",
        ],
    ),
    ("ssh.connect_host_failed", ["连接 {host} 失败：{error}", "Failed to connect to {host}: {error}", "連線 {host} 失敗：{error}", "{host} に接続できませんでした：{error}"]),
    ("ssh.sftp_unsupported", ["服务器不支持 SFTP：{error}", "The server does not support SFTP: {error}", "伺服器不支援 SFTP：{error}", "サーバーが SFTP をサポートしていません：{error}"]),
    ("ssh.sftp_init_failed", ["初始化 SFTP 失败：{error}", "Failed to initialize SFTP: {error}", "初始化 SFTP 失敗：{error}", "SFTP を初期化できませんでした：{error}"]),
    ("ssh.shell_failed", ["启动远程 shell 失败：{error}", "Failed to start remote shell: {error}", "啟動遠端 shell 失敗：{error}", "リモート shell を起動できませんでした：{error}"]),
    ("ssh.connect_failed", ["连接失败：{error}", "Connection failed: {error}", "連線失敗：{error}", "接続に失敗しました：{error}"]),
    ("ssh.dns_failed", ["DNS 解析失败：无法解析主机名 {host}（{error}）", "DNS lookup failed: cannot resolve host name {host} ({error})", "DNS 解析失敗：無法解析主機名稱 {host}（{error}）", "DNS 名前解決に失敗しました：ホスト名 {host} を解決できません（{error}）"]),
    ("ssh.hostkey_rejected", ["主机密钥校验未通过：{reason}", "Host key verification failed: {reason}", "主機金鑰驗證未通過：{reason}", "ホスト鍵の検証に失敗しました：{reason}"]),
    ("ssh.handshake_failed", ["SSH 握手失败：{error}", "SSH handshake failed: {error}", "SSH 交握失敗：{error}", "SSH ハンドシェイクに失敗しました：{error}"]),
    ("ssh.auth_failed", ["认证失败：{error}", "Authentication failed: {error}", "驗證失敗：{error}", "認証に失敗しました：{error}"]),
    ("ssh.tcp_refused", ["连接被拒绝：{host}:{port} 没有在监听 SSH 服务（{error}）", "Connection refused: nothing is listening for SSH on {host}:{port} ({error})", "連線被拒絕：{host}:{port} 沒有在監聽 SSH 服務（{error}）", "接続が拒否されました：{host}:{port} で SSH サービスが待ち受けていません（{error}）"]),
    ("ssh.tcp_timeout", ["连接超时：{host}:{port} 无响应（{error}）", "Connection timed out: {host}:{port} is not responding ({error})", "連線逾時：{host}:{port} 無回應（{error}）", "接続タイムアウト：{host}:{port} から応答がありません（{error}）"]),
    ("ssh.tcp_unreachable", ["网络不可达：无法到达 {host}:{port}（{error}）", "Network unreachable: cannot reach {host}:{port} ({error})", "網路無法連線：無法到達 {host}:{port}（{error}）", "ネットワークに到達できません：{host}:{port} に到達できません（{error}）"]),
    ("ssh.tcp_failed", ["TCP 连接失败：{host}:{port}（{error}）", "TCP connection failed: {host}:{port} ({error})", "TCP 連線失敗：{host}:{port}（{error}）", "TCP 接続に失敗しました：{host}:{port}（{error}）"]),
    ("ssh.no_window", ["需要输入，但没有打开的 Tether 窗口", "Input is required, but no Tether window is open", "需要輸入，但沒有開啟的 Tether 視窗", "入力が必要ですが、Tether のウィンドウが開いていません"]),
    ("ssh.cancelled", ["已取消", "Cancelled", "已取消", "キャンセルしました"]),
    ("ssh.prompt_timeout", ["等待输入超时，已取消", "Timed out waiting for input; cancelled", "等待輸入逾時，已取消", "入力待ちがタイムアウトしたため、キャンセルしました"]),
    ("ssh.host_cert", ["不支持主机证书", "Host certificates are not supported", "不支援主機憑證", "ホスト証明書はサポートされていません"]),
    (
        "ssh.key_changed",
        [
            "主机指纹与 ~/.ssh/known_hosts 第 {line} 行的记录不一致，可能遭到中间人攻击，已拒绝连接",
            "The host fingerprint does not match line {line} of ~/.ssh/known_hosts; this may be a man-in-the-middle attack. Connection refused",
            "主機指紋與 ~/.ssh/known_hosts 第 {line} 行的記錄不一致，可能遭到中間人攻擊，已拒絕連線",
            "ホストのフィンガープリントが ~/.ssh/known_hosts の {line} 行目の記録と一致しません。中間者攻撃の可能性があるため、接続を拒否しました",
        ],
    ),
    ("ssh.known_hosts_read_failed", ["读取 known_hosts 失败：{error}", "Failed to read known_hosts: {error}", "讀取 known_hosts 失敗：{error}", "known_hosts を読み込めませんでした：{error}"]),
    ("ssh.hostkey_quiet", ["首次连接该主机，需要确认主机指纹：{reason}", "First connection to this host; the host fingerprint must be confirmed: {reason}", "首次連線此主機，需要確認主機指紋：{reason}", "このホストへの初回接続のため、ホストのフィンガープリントの確認が必要です：{reason}"]),
    (
        "ssh.hostkey_prompt",
        [
            "首次连接该主机，无法确认其真实性。请核对 {algorithm} 主机密钥指纹，信任后将记录到 ~/.ssh/known_hosts。",
            "This is the first connection to this host and its authenticity cannot be confirmed. Check the {algorithm} host key fingerprint; once trusted it will be recorded in ~/.ssh/known_hosts.",
            "首次連線此主機，無法確認其真實性。請核對 {algorithm} 主機金鑰指紋，信任後將記錄到 ~/.ssh/known_hosts。",
            "このホストへの初回接続のため、真正性を確認できません。{algorithm} ホスト鍵のフィンガープリントを確認してください。信頼すると ~/.ssh/known_hosts に記録されます。",
        ],
    ),
    ("ssh.hostkey_untrusted", ["未信任主机指纹", "Host fingerprint not trusted", "未信任主機指紋", "ホストのフィンガープリントが信頼されていません"]),
    ("ssh.known_hosts_write_failed", ["写入 known_hosts 失败：{error}", "Failed to write known_hosts: {error}", "寫入 known_hosts 失敗：{error}", "known_hosts に書き込めませんでした：{error}"]),
    ("ssh.no_default_keys", ["~/.ssh 下没有默认私钥（{keys}）", "No default private key in ~/.ssh ({keys})", "~/.ssh 下沒有預設私密金鑰（{keys}）", "~/.ssh に既定の秘密鍵がありません（{keys}）"]),
    ("ssh.key_auth_failed", ["密钥认证失败", "Key authentication failed", "金鑰驗證失敗", "鍵認証に失敗しました"]),
    ("ssh.key_missing", ["私钥文件 {path} 不存在", "Private key file {path} does not exist", "私密金鑰檔案 {path} 不存在", "秘密鍵ファイル {path} が存在しません"]),
    ("ssh.key_needs_passphrase", ["私钥 {path} 有口令，需要输入", "Private key {path} has a passphrase that must be entered", "私密金鑰 {path} 有密碼短語，需要輸入", "秘密鍵 {path} にはパスフレーズがあり、入力が必要です"]),
    ("ssh.passphrase_prompt", ["请输入私钥口令：{path}", "Enter the passphrase for the private key: {path}", "請輸入私密金鑰密碼短語：{path}", "秘密鍵のパスフレーズを入力してください：{path}"]),
    ("ssh.passphrase_retry", ["口令错误，请重新输入私钥口令：{path}", "Incorrect passphrase; enter the private key passphrase again: {path}", "密碼短語錯誤，請重新輸入私密金鑰密碼短語：{path}", "パスフレーズが違います。秘密鍵のパスフレーズをもう一度入力してください：{path}"]),
    ("ssh.key_locked", ["私钥 {path} 未解锁", "Private key {path} was not unlocked", "私密金鑰 {path} 未解鎖", "秘密鍵 {path} のロックが解除されませんでした"]),
    ("ssh.key_parse_failed", ["无法解析私钥 {path}：{error}", "Cannot parse private key {path}: {error}", "無法解析私密金鑰 {path}：{error}", "秘密鍵 {path} を解析できません：{error}"]),
    ("ssh.key_rejected", ["服务器未接受私钥 {path}", "The server did not accept private key {path}", "伺服器未接受私密金鑰 {path}", "サーバーが秘密鍵 {path} を受け付けませんでした"]),
    ("ssh.key_error", ["私钥 {path} 认证出错：{error}", "Error authenticating with private key {path}: {error}", "私密金鑰 {path} 驗證出錯：{error}", "秘密鍵 {path} での認証中にエラーが発生しました：{error}"]),
    ("ssh.agent_not_running", ["ssh-agent 未运行", "ssh-agent is not running", "ssh-agent 未執行", "ssh-agent が実行されていません"]),
    ("ssh.agent_read_failed", ["读取 ssh-agent 中的密钥失败", "Failed to read keys from ssh-agent", "讀取 ssh-agent 中的金鑰失敗", "ssh-agent の鍵を読み込めませんでした"]),
    ("ssh.agent_empty", ["ssh-agent 中没有密钥", "ssh-agent has no keys", "ssh-agent 中沒有金鑰", "ssh-agent に鍵がありません"]),
    ("ssh.agent_rejected", ["服务器未接受 ssh-agent 中的 {n} 个密钥", "The server accepted none of the keys in ssh-agent ({n})", "伺服器未接受 ssh-agent 中的 {n} 個金鑰", "サーバーが ssh-agent の鍵（{n} 個）を受け付けませんでした"]),
    ("ssh.password_missing", ["未填写密码，凭据管理器中也没有已保存的密码", "No password entered, and none is saved in Credential Manager", "未填寫密碼，認證管理員中也沒有已儲存的密碼", "パスワードが入力されておらず、資格情報マネージャーにも保存されていません"]),
    ("ssh.password_wrong", ["密码错误", "Incorrect password", "密碼錯誤", "パスワードが違います"]),
    ("ssh.password_quiet", ["需要输入 {host} 的密码：{reason}", "A password is required for {host}: {reason}", "需要輸入 {host} 的密碼：{reason}", "{host} のパスワードの入力が必要です：{reason}"]),
    ("ssh.password_prompt", ["请输入 {host} 的密码", "Enter the password for {host}", "請輸入 {host} 的密碼", "{host} のパスワードを入力してください"]),
    ("ssh.password_retry", ["密码错误，请重新输入 {host} 的密码", "Incorrect password; enter the password for {host} again", "密碼錯誤，請重新輸入 {host} 的密碼", "パスワードが違います。{host} のパスワードをもう一度入力してください"]),
    ("ssh.password_too_many", ["密码错误次数过多", "Too many incorrect password attempts", "密碼錯誤次數過多", "パスワード入力の失敗回数が多すぎます"]),
    // ---------------- hub ----------------
    ("hub.no_token_saved", ["未保存 token：请在设置中填写 hub 签发的 token", "No token saved: enter the token issued by the hub in Settings", "未儲存 token：請在設定中填寫 hub 簽發的 token", "token が保存されていません：hub が発行した token を設定で入力してください"]),
    ("hub.takeover_disabled", ["本机未允许 hub 接管", "This machine does not allow hub takeover", "本機未允許 hub 接管", "このマシンでは hub による引き継ぎが許可されていません"]),
    ("hub.no_token", ["未填写 token，凭据管理器中也没有已保存的 token", "No token entered, and none is saved in Credential Manager", "未填寫 token，認證管理員中也沒有已儲存的 token", "token が入力されておらず、資格情報マネージャーにも保存されていません"]),
    ("hub.keyring_unavailable", ["无法打开 Windows 凭据管理器", "Cannot open Windows Credential Manager", "無法開啟 Windows 認證管理員", "Windows 資格情報マネージャーを開けません"]),
    (
        "hub.replaced",
        [
            "另一台机器使用了相同的机器 ID（例如克隆的虚拟机），已停止重连",
            "Another machine is using the same machine ID (e.g. a cloned VM); stopped reconnecting",
            "另一台機器使用了相同的機器 ID（例如複製的虛擬機），已停止重新連線",
            "別のマシンが同じマシン ID を使用しています（複製した仮想マシンなど）。再接続を停止しました",
        ],
    ),
    ("hub.addr_empty", ["未填写服务器地址", "No server address entered", "未填寫伺服器位址", "サーバー アドレスが入力されていません"]),
    ("hub.addr_bad_scheme", ["不支持的协议 {scheme}://，只能是 wss:// 或 ws://", "Unsupported scheme {scheme}://; only wss:// or ws:// is allowed", "不支援的協定 {scheme}://，只能是 wss:// 或 ws://", "サポートされていないプロトコル {scheme}:// です。wss:// または ws:// のみ使用できます"]),
    (
        "hub.addr_insecure",
        [
            "ws:// 为明文连接，需勾选「允许明文连接（仅限局域网调试）」",
            "ws:// is an unencrypted connection; enable “Allow plaintext connection (LAN debugging only)”",
            "ws:// 為明文連線，需勾選「允許明文連線（僅限區域網路偵錯）」",
            "ws:// は暗号化されない接続です。「平文での接続を許可（LAN でのデバッグ専用）」をオンにしてください",
        ],
    ),
    ("hub.addr_host_port_only", ["地址只能包含主机和端口", "The address may contain only a host and port", "位址只能包含主機和連接埠", "アドレスにはホストとポートのみ指定できます"]),
    ("hub.addr_no_path", ["地址不能包含路径（连接路径固定为 {path}）", "The address may not contain a path (the connection path is always {path})", "位址不能包含路徑（連線路徑固定為 {path}）", "アドレスにパスは指定できません（接続パスは {path} 固定）"]),
    ("hub.addr_v6_bracket", ["IPv6 地址缺少右方括号", "IPv6 address is missing the closing bracket", "IPv6 位址缺少右方括號", "IPv6 アドレスに閉じ角かっこがありません"]),
    ("hub.addr_bad_v6", ["{host} 不是合法的 IPv6 地址", "{host} is not a valid IPv6 address", "{host} 不是有效的 IPv6 位址", "{host} は有効な IPv6 アドレスではありません"]),
    ("hub.addr_v6_port", ["IPv6 地址后只能跟 :端口", "Only :port may follow an IPv6 address", "IPv6 位址後只能接 :連接埠", "IPv6 アドレスの後には :ポート のみ指定できます"]),
    ("hub.addr_v6_brackets_needed", ["IPv6 地址需用方括号括起，如 [fe80::1]:8443", "IPv6 addresses must be enclosed in brackets, e.g. [fe80::1]:8443", "IPv6 位址需用方括號括起，如 [fe80::1]:8443", "IPv6 アドレスは角かっこで囲んでください（例：[fe80::1]:8443）"]),
    ("hub.addr_no_host", ["缺少主机名", "Host name is missing", "缺少主機名稱", "ホスト名がありません"]),
    ("hub.addr_bad_host", ["主机名 {host} 含非法字符", "Host name {host} contains invalid characters", "主機名稱 {host} 含有無效字元", "ホスト名 {host} に無効な文字が含まれています"]),
    ("hub.addr_bad_port", ["端口 {port} 无效，应为 1–65535", "Invalid port {port}; must be 1–65535", "連接埠 {port} 無效，應為 1–65535", "ポート {port} は無効です。1–65535 を指定してください"]),
    ("hub.detail", ["（{message}）", " ({message})", "（{message}）", "（{message}）"]),
    ("hub.proto_unsupported", ["协议不兼容：hub 不支持协议 {proto}{detail}", "Incompatible protocol: the hub does not support protocol {proto}{detail}", "協定不相容：hub 不支援協定 {proto}{detail}", "プロトコルに互換性がありません：hub はプロトコル {proto} をサポートしていません{detail}"]),
    ("hub.revoked", ["token 已被 hub 吊销{detail}", "The token has been revoked by the hub{detail}", "token 已被 hub 撤銷{detail}", "token は hub によって失効されました{detail}"]),
    ("hub.bad_hello", ["hub 拒绝了 hello{detail}", "The hub rejected hello{detail}", "hub 拒絕了 hello{detail}", "hub が hello を拒否しました{detail}"]),
    ("hub.error_code", ["hub 返回错误 {code}{detail}", "The hub returned error {code}{detail}", "hub 回傳錯誤 {code}{detail}", "hub がエラー {code} を返しました{detail}"]),
    ("hub.closed", ["hub 关闭了连接", "The hub closed the connection", "hub 關閉了連線", "hub が接続を閉じました"]),
    ("hub.closed_code", ["hub 关闭了连接（关闭码 {code}）", "The hub closed the connection (close code {code})", "hub 關閉了連線（關閉代碼 {code}）", "hub が接続を閉じました（クローズ コード {code}）"]),
    ("hub.closed_code_reason", ["hub 关闭了连接（关闭码 {code}：{reason}）", "The hub closed the connection (close code {code}: {reason})", "hub 關閉了連線（關閉代碼 {code}：{reason}）", "hub が接続を閉じました（クローズ コード {code}：{reason}）"]),
    ("hub.auth_failed", ["认证失败：token 无效或已吊销（HTTP 401）", "Authentication failed: the token is invalid or revoked (HTTP 401)", "驗證失敗：token 無效或已撤銷（HTTP 401）", "認証に失敗しました：token が無効か失効しています（HTTP 401）"]),
    ("hub.ws_rejected", ["WebSocket 握手被拒绝：HTTP {status}（地址或端口可能指向了其它服务）", "WebSocket handshake rejected: HTTP {status} (the address or port may point to another service)", "WebSocket 交握被拒絕：HTTP {status}（位址或連接埠可能指向了其他服務）", "WebSocket ハンドシェイクが拒否されました：HTTP {status}（アドレスまたはポートが別のサービスを指している可能性があります）"]),
    ("hub.ws_io", ["WebSocket 握手时连接断开（{error}）", "Connection lost during WebSocket handshake ({error})", "WebSocket 交握時連線中斷（{error}）", "WebSocket ハンドシェイク中に接続が切断されました（{error}）"]),
    ("hub.ws_closed", ["WebSocket 握手时连接断开", "Connection lost during WebSocket handshake", "WebSocket 交握時連線中斷", "WebSocket ハンドシェイク中に接続が切断されました"]),
    ("hub.ws_failed", ["WebSocket 握手失败，对端可能不是 Tether hub（{error}）", "WebSocket handshake failed; the peer may not be a Tether hub ({error})", "WebSocket 交握失敗，對端可能不是 Tether hub（{error}）", "WebSocket ハンドシェイクに失敗しました。接続先が Tether hub ではない可能性があります（{error}）"]),
    ("hub.tcp_refused", ["TCP 连接被拒绝：{addr} 没有在监听（{error}）", "TCP connection refused: nothing is listening on {addr} ({error})", "TCP 連線被拒絕：{addr} 沒有在監聽（{error}）", "TCP 接続が拒否されました：{addr} で待ち受けていません（{error}）"]),
    ("hub.tcp_timeout", ["TCP 连接超时：{addr} 无响应（{error}）", "TCP connection timed out: {addr} is not responding ({error})", "TCP 連線逾時：{addr} 無回應（{error}）", "TCP 接続タイムアウト：{addr} から応答がありません（{error}）"]),
    ("hub.tcp_unreachable", ["TCP 连接失败：网络不可达 {addr}（{error}）", "TCP connection failed: network unreachable {addr} ({error})", "TCP 連線失敗：網路無法連線 {addr}（{error}）", "TCP 接続に失敗しました：ネットワークに到達できません {addr}（{error}）"]),
    ("hub.tcp_failed", ["TCP 连接失败：{addr}（{error}）", "TCP connection failed: {addr} ({error})", "TCP 連線失敗：{addr}（{error}）", "TCP 接続に失敗しました：{addr}（{error}）"]),
    (
        "hub.tls_untrusted",
        [
            "TLS 失败：证书不受信任（签发者不在 Windows 受信任根证书中；自签证书需先导入证书库）",
            "TLS failed: certificate not trusted (the issuer is not among the Windows trusted root certificates; import self-signed certificates into the certificate store first)",
            "TLS 失敗：憑證不受信任（簽發者不在 Windows 受信任的根憑證中；自簽憑證需先匯入憑證存放區）",
            "TLS に失敗しました：証明書が信頼されていません（発行者が Windows の信頼されたルート証明書にありません。自己署名証明書は先に証明書ストアへインポートしてください）",
        ],
    ),
    ("hub.tls_name_mismatch", ["TLS 失败：证书与服务器地址不匹配", "TLS failed: the certificate does not match the server address", "TLS 失敗：憑證與伺服器位址不符", "TLS に失敗しました：証明書がサーバー アドレスと一致しません"]),
    ("hub.tls_expired", ["TLS 失败：证书已过期", "TLS failed: the certificate has expired", "TLS 失敗：憑證已過期", "TLS に失敗しました：証明書の有効期限が切れています"]),
    ("hub.tls_invalid", ["TLS 失败：证书无效（{error}）", "TLS failed: invalid certificate ({error})", "TLS 失敗：憑證無效（{error}）", "TLS に失敗しました：証明書が無効です（{error}）"]),
    ("hub.tls_failed", ["TLS 失败：{error}", "TLS failed: {error}", "TLS 失敗：{error}", "TLS に失敗しました：{error}"]),
    (
        "hub.tls_not_tls",
        [
            "TLS 失败：{error}（对端可能未启用 TLS，明文 hub 需写 ws:// 并勾选允许明文）",
            "TLS failed: {error} (the peer may not have TLS enabled; for an unencrypted hub use ws:// and allow unencrypted connections)",
            "TLS 失敗：{error}（對端可能未啟用 TLS，明文 hub 需寫 ws:// 並勾選允許明文）",
            "TLS に失敗しました：{error}（接続先で TLS が無効な可能性があります。平文の hub には ws:// を指定し、平文接続を許可してください）",
        ],
    ),
    ("hub.tls_no_roots", ["TLS 失败：无法读取 Windows 证书库中的受信任根证书", "TLS failed: cannot read trusted root certificates from the Windows certificate store", "TLS 失敗：無法讀取 Windows 憑證存放區中受信任的根憑證", "TLS に失敗しました：Windows 証明書ストアの信頼されたルート証明書を読み込めません"]),
    ("hub.tls_init_failed", ["TLS 初始化失败：{error}", "TLS initialization failed: {error}", "TLS 初始化失敗：{error}", "TLS の初期化に失敗しました：{error}"]),
    ("hub.dns_failed", ["DNS 解析失败：无法解析 {host}（{error}）", "DNS lookup failed: cannot resolve {host} ({error})", "DNS 解析失敗：無法解析 {host}（{error}）", "DNS 名前解決に失敗しました：{host} を解決できません（{error}）"]),
    ("hub.tls_bad_name", ["地址无效：{host} 不能用作 TLS 服务器名", "Invalid address: {host} cannot be used as a TLS server name", "位址無效：{host} 無法用作 TLS 伺服器名稱", "アドレスが無効です：{host} は TLS サーバー名として使用できません"]),
    ("hub.token_invalid_chars", ["token 含非法字符（只能是可见 ASCII 字符）", "The token contains invalid characters (only printable ASCII is allowed)", "token 含有無效字元（只能是可見 ASCII 字元）", "token に無効な文字が含まれています（表示可能な ASCII 文字のみ使用できます）"]),
    ("hub.serialize_failed", ["序列化失败：{error}", "Serialization failed: {error}", "序列化失敗：{error}", "シリアル化に失敗しました：{error}"]),
    ("hub.send_failed", ["发送失败，连接已断开（{error}）", "Send failed; connection lost ({error})", "傳送失敗，連線已中斷（{error}）", "送信に失敗しました。接続が切断されています（{error}）"]),
    ("hub.disconnected", ["连接断开（{error}）", "Connection lost ({error})", "連線中斷（{error}）", "接続が切断されました（{error}）"]),
    ("hub.proto_mismatch", ["协议不兼容：hub 使用协议 {hub}，本机为 {local}", "Incompatible protocol: the hub uses protocol {hub}, this machine uses {local}", "協定不相容：hub 使用協定 {hub}，本機為 {local}", "プロトコルに互換性がありません：hub はプロトコル {hub}、このマシンは {local} です"]),
    ("hub.welcome_timeout", ["超时：{secs} 秒内未收到 hub 的 welcome", "Timed out: no welcome from the hub within {secs} s", "逾時：{secs} 秒內未收到 hub 的 welcome", "タイムアウト：{secs} 秒以内に hub から welcome を受信しませんでした"]),
    ("hub.idle_timeout", ["{secs} 秒内未收到 hub 的任何数据，判定断线", "No data from the hub for {secs} s; treating the connection as lost", "{secs} 秒內未收到 hub 的任何資料，判定斷線", "{secs} 秒間 hub からデータを受信しなかったため、切断とみなしました"]),
];

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn table_consistent() {
        if let Err(e) = proto::i18n::check(TABLE) {
            panic!("{e}");
        }
    }

    /// 源码中的 tr! 调用：键都在词条表中、命名参数与 zh-CN 模板的占位符一致；
    /// 以 "stage." 开头的字面量（连接阶段，运行时查表）也须在表中；表中每个键都被源码用到
    #[test]
    fn source_keys_match_table() {
        let mut sources = String::new();
        collect(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
        let mut used = std::collections::HashSet::new();
        let mut errors = Vec::new();
        for (i, _) in sources.match_indices("tr!(\"") {
            let call = macro_args(&sources[i + 3..]);
            let mut args = split_args(call);
            let key = args.remove(0).trim().trim_matches('"').to_string();
            let mut names: Vec<String> = args.iter().filter_map(|a| a.split_once('=').map(|(n, _)| n.trim().to_string())).collect();
            names.sort();
            match TABLE.iter().find(|(k, _)| *k == key) {
                None => errors.push(format!("词条表中没有 {key}")),
                Some((_, texts)) => {
                    let want: Vec<String> = proto::i18n::placeholders(texts[0]).into_iter().map(String::from).collect();
                    if names != want {
                        errors.push(format!("tr!(\"{key}\") 参数 {names:?} 与占位符 {want:?} 不一致"));
                    }
                }
            }
            used.insert(key);
        }
        for (i, _) in sources.match_indices("\"stage.") {
            let key = &sources[i + 1..i + 1 + sources[i + 1..].find('"').unwrap()];
            if !TABLE.iter().any(|(k, _)| *k == key) {
                errors.push(format!("词条表中没有 {key}"));
            }
            used.insert(key.to_string());
        }
        for (key, _) in TABLE {
            if !used.contains(*key) {
                errors.push(format!("词条 {key} 未被使用"));
            }
        }
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }

    /// 读取目录下除本文件外的全部 .rs
    fn collect(dir: &Path, out: &mut String) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|e| e == "rs") && p.file_name().is_some_and(|n| n != "i18n.rs") {
                out.push_str(&std::fs::read_to_string(&p).unwrap());
            }
        }
    }

    /// s 以 `(` 开头：返回与之匹配的 `)` 之间的内容（跳过字符串字面量）
    fn macro_args(s: &str) -> &str {
        let (mut depth, mut quoted, mut escaped) = (0, false, false);
        for (i, c) in s.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' if quoted => escaped = true,
                '"' => quoted = !quoted,
                '(' | '[' | '{' if !quoted => depth += 1,
                ')' | ']' | '}' if !quoted => {
                    depth -= 1;
                    if depth == 0 {
                        return &s[1..i];
                    }
                }
                _ => {}
            }
        }
        panic!("tr! 调用括号不匹配：{}", &s[..s.len().min(80)]);
    }

    /// 按顶层逗号拆分宏参数（跳过括号与字符串内的逗号）
    fn split_args(s: &str) -> Vec<&str> {
        let (mut depth, mut quoted, mut escaped, mut start) = (0, false, false, 0);
        let mut out = Vec::new();
        for (i, c) in s.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' if quoted => escaped = true,
                '"' => quoted = !quoted,
                '(' | '[' | '{' if !quoted => depth += 1,
                ')' | ']' | '}' if !quoted => depth -= 1,
                ',' if !quoted && depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        out.push(&s[start..]);
        out.into_iter().filter(|a| !a.trim().is_empty()).collect()
    }
}
