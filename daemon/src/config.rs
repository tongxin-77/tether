//! 用户配置读写：%APPDATA%\tether\config.json，文件不存在或解析失败时用默认值。

use std::path::PathBuf;

use proto::{AuthKind, Config, Host, HostSource, ShellOption, AUTO_SHELL};

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join(proto::data_dir_name()).join("config.json"))
}

pub fn load() -> Config {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(config: &Config) -> Result<(), String> {
    let p = path().ok_or_else(|| tr!("common.no_appdata"))?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(p, json).map_err(|e| e.to_string())
}

/// 按空白拆分命令行为程序 + 参数；双引号包裹的部分（如带空格的路径）不拆，引号本身去掉
pub fn split_command(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut has_token) = (false, false);
    for c in line.chars() {
        match c {
            '"' => (quoted, has_token) = (!quoted, true),
            c if c.is_whitespace() && !quoted => {
                if has_token {
                    args.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        args.push(cur);
    }
    args
}

/// 探测本机可用的 shell，按固定顺序返回；不存在的不列出
pub fn detect_shells() -> Vec<ShellOption> {
    let opt = |name: String, command: String| ShellOption { name, command };
    let mut shells = vec![
        opt(tr!("shell.auto"), AUTO_SHELL.into()),
        opt("Windows PowerShell".into(), WINDOWS_POWERSHELL.into()),
    ];
    if let Some(pwsh) = pwsh() {
        shells.push(opt("PowerShell 7".into(), pwsh));
    }
    shells.push(opt(tr!("shell.cmd"), "cmd.exe".into()));
    if let Some(bash) = git_bash() {
        shells.push(opt("Git Bash".into(), format!("\"{}\" --login -i", bash.display())));
    }
    let wsl = std::env::var_os("SystemRoot").map(|r| PathBuf::from(r).join(r"System32\wsl.exe"));
    if wsl.is_some_and(|p| p.is_file()) {
        shells.push(opt("WSL".into(), "wsl.exe".into()));
    }
    shells
}

const WINDOWS_POWERSHELL: &str = "powershell.exe -NoLogo";

/// PowerShell 7 的启动命令行，未安装为 None。daemon 常驻，PATH 可能是安装前的旧值，
/// 所以 PATH 中找不到时再查默认安装位置 %ProgramFiles%\PowerShell\7\pwsh.exe（此时用完整路径启动）
fn pwsh() -> Option<String> {
    if find_in_path("pwsh.exe").is_some() {
        return Some("pwsh.exe -NoLogo".into());
    }
    let fixed = std::env::var_os("ProgramFiles").map(|d| PathBuf::from(d).join(r"PowerShell\7\pwsh.exe"))?;
    fixed.is_file().then(|| format!("\"{}\" -NoLogo", fixed.display()))
}

/// 新建本地会话实际使用的 shell 命令行：自动模式现场探测 PowerShell 7
pub fn resolve_shell(shell: &str) -> String {
    resolve_shell_with(shell, pwsh())
}

/// 自动模式下有 PowerShell 7（pwsh 为其命令行）则用它，否则 Windows PowerShell；其它值原样返回
fn resolve_shell_with(shell: &str, pwsh: Option<String>) -> String {
    match shell {
        AUTO_SHELL => pwsh.unwrap_or_else(|| WINDOWS_POWERSHELL.into()),
        _ => shell.into(),
    }
}

/// 在 PATH 各目录中查找文件
fn find_in_path(file: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(file)).find(|p| p.is_file())
}

/// Git Bash：由 PATH 中 git.exe 所在目录（如 Git\cmd）的上级推导 Git 根目录，回退 %ProgramFiles%\Git
fn git_bash() -> Option<PathBuf> {
    let from_path = find_in_path("git.exe")
        .and_then(|git| git.parent()?.parent().map(|root| root.join(r"bin\bash.exe")));
    let fallback = std::env::var_os("ProgramFiles").map(|d| PathBuf::from(d).join(r"Git\bin\bash.exe"));
    [from_path, fallback].into_iter().flatten().find(|p| p.is_file())
}

/// 用户主目录（%USERPROFILE%）
pub fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

/// `~` 或 `~/`、`~\` 开头的路径展开为主目录
pub fn expand_tilde(p: &str) -> String {
    let rest = match p.strip_prefix('~') {
        Some(r) if r.is_empty() || r.starts_with(['/', '\\']) => r,
        _ => return p.into(),
    };
    match home() {
        Some(h) => format!("{}{rest}", h.display()),
        None => p.into(),
    }
}

/// 合并后的主机列表：Tether 管理的在前，~/.ssh/config 中的主机在后（有覆盖项时整条替换为覆盖项）；
/// ~/.ssh/config 中已不存在的 alias 对应的覆盖项不列出
pub fn all_hosts(config: &Config) -> Vec<Host> {
    all_hosts_with(config, ssh_config_hosts())
}

/// 同 all_hosts，~/.ssh/config 的解析结果由调用方给出（同一次操作中新旧配置按同一份解析结果合并）
pub fn all_hosts_with(config: &Config, ssh_hosts: Vec<Host>) -> Vec<Host> {
    let (mut hosts, overrides): (Vec<Host>, Vec<Host>) =
        config.hosts.iter().cloned().partition(|h| h.source == HostSource::Tether);
    hosts.extend(ssh_hosts.into_iter().map(|s| overrides.iter().find(|o| o.id == s.id).cloned().unwrap_or(s)));
    hosts
}

/// 最小解析 ~/.ssh/config：只认 Host / HostName / User / Port / IdentityFile，关键字大小写不敏感；
/// 含通配符（* ?）或取反（!）的模式跳过，Match 块忽略；同一关键字先出现的生效（与 OpenSSH 一致）。只读，不修改文件
pub fn ssh_config_hosts() -> Vec<Host> {
    let Some(text) = home().and_then(|h| std::fs::read_to_string(h.join(".ssh").join("config")).ok()) else {
        return Vec::new();
    };
    let mut hosts: Vec<Host> = Vec::new();
    // 当前 Host 块对应的条目在 hosts 中的起始下标；None 表示不在可用的 Host 块内
    let mut block: Option<usize> = None;
    // 当前块内已出现过的关键字
    let mut seen: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // 关键字与值之间可用空白或 '='
        let (key, value) = match line.find(|c: char| c.is_whitespace() || c == '=') {
            Some(i) => (&line[..i], line[i..].trim_start_matches(|c: char| c.is_whitespace() || c == '=')),
            None => continue,
        };
        let key = key.to_ascii_lowercase();
        let value = value.trim().trim_matches('"');
        if key == "host" || key == "match" {
            seen.clear();
            block = None;
            if key == "match" {
                continue;
            }
            let start = hosts.len();
            for alias in value.split_whitespace().filter(|a| !a.contains(['*', '?', '!'])) {
                hosts.push(Host {
                    id: format!("ssh:{alias}"),
                    name: alias.into(),
                    host: alias.into(),
                    port: 22,
                    // 未写 User 时默认 root；可在设置中覆盖
                    user: "root".into(),
                    auth: AuthKind::Key,
                    key_path: None,
                    source: HostSource::SshConfig,
                });
            }
            if hosts.len() > start {
                block = Some(start);
            }
            continue;
        }
        let Some(start) = block else { continue };
        if seen.contains(&key) {
            continue;
        }
        seen.push(key.clone());
        for h in &mut hosts[start..] {
            match key.as_str() {
                "hostname" => h.host = value.into(),
                "user" => h.user = value.into(),
                "port" => h.port = value.parse().unwrap_or(22),
                "identityfile" => h.key_path = Some(expand_tilde(value)),
                _ => {}
            }
        }
    }
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prefers_pwsh() {
        let pwsh = r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo"#;
        assert_eq!(resolve_shell_with(AUTO_SHELL, Some(pwsh.into())), pwsh);
        assert_eq!(resolve_shell_with(AUTO_SHELL, None), WINDOWS_POWERSHELL);
    }

    #[test]
    fn explicit_shell_unchanged() {
        assert_eq!(resolve_shell_with("cmd.exe", Some("pwsh.exe -NoLogo".into())), "cmd.exe");
        assert_eq!(resolve_shell_with("powershell.exe -NoLogo", Some("pwsh.exe -NoLogo".into())), "powershell.exe -NoLogo");
    }

    #[test]
    fn missing_shell_field_is_auto() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.shell, AUTO_SHELL);
        let c: Config = serde_json::from_str(r#"{"shell":"cmd.exe"}"#).unwrap();
        assert_eq!(c.shell, "cmd.exe");
    }
}
