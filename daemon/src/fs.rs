//! 文件树的文件操作：本地磁盘（std::fs 放在 spawn_blocking 中执行，文件读写用 tokio::fs）与远程 SFTP 两种后端。
//! 每个请求在独立异步任务中执行，结果以 FsEntries / FsResult 回给 GUI；传输分块流式读写，过程中推送 Transfer。

use std::{
    collections::HashSet,
    fmt::Display,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Weak,
    time::{SystemTime, UNIX_EPOCH},
};

use proto::{CopyItem, FsEntry, Host, OpenMode, ServerMsg, TextEncoding};
use russh_sftp::{client::SftpSession, protocol::OpenFlags};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use winreg::{HKCR, HKCU};

use crate::{session::Shared, ssh, ssh::Conn};

/// 传输分块大小
const CHUNK: usize = 64 * 1024;
/// 每累计传输这么多字节推送一次进度（每个文件结束时也推送）
const PROGRESS_STEP: u64 = 512 * 1024;
/// 下载远程目录时跟随符号链接的最大深度，防止链接成环
const MAX_DEPTH: usize = 32;
/// FsRead 允许读取的最大字节数（Markdown / 纯文本预览用；日志可能较大）
const MAX_READ: u64 = 20 * 1024 * 1024;
/// 读取时检查是否含 NUL 字节（判为二进制）的前缀长度
const BINARY_PROBE: usize = 8 * 1024;
/// “都保留”改名时最多尝试的序号
const MAX_RENAME: u32 = 10_000;

/// 会话的文件后端
#[derive(Clone)]
pub enum Backend {
    Local,
    /// 只持有弱引用：连接寿命仍由终端 channel 决定；label 为 `user@host_port`，用作远程文件打开时的临时目录名
    Remote { conn: Weak<Conn>, label: String },
}

impl Backend {
    /// 远程连接的文件树正以 root 浏览（sudo.rs）；本地 / 连接已释放为 false
    pub fn is_root(&self) -> bool {
        matches!(self, Backend::Remote { conn, .. } if conn.upgrade().is_some_and(|c| c.is_root()))
    }
}

pub enum Op {
    List(String),
    Mkdir(String),
    CreateFile(String),
    Rename(String, String),
    Delete(String),
    Open(String, OpenMode),
    /// 读取文本内容
    Read(String),
    /// 写回文本（编辑器保存）
    Write(Box<WriteReq>),
    /// (本地路径列表, 会话侧目标目录)
    Upload(Vec<String>, String),
    /// (会话侧路径列表, 本地目标目录)
    Download(Vec<String>, String),
}

/// FsWrite 的参数，含义见 proto
pub struct WriteReq {
    pub path: String,
    pub text: String,
    pub encoding: TextEncoding,
    pub crlf: bool,
    pub mtime: u64,
    pub size: u64,
    pub force: bool,
}

type Res<T> = Result<T, String>;

/// 读取到的文本及其格式
struct TextFile {
    text: String,
    encoding: Option<TextEncoding>,
    crlf: bool,
    mtime: u64,
    size: u64,
}

/// 操作成功时的回复内容
enum Reply {
    Done,
    /// (目录, 目录项)
    Entries(String, Vec<FsEntry>),
    /// (文件路径, 文本)
    Content(String, TextFile),
    /// 写回结果：conflict 时未写入；(mtime, size) 为文件当前值
    Saved { conflict: bool, mtime: u64, size: u64 },
}

/// 执行一个文件请求并回复 GUI
pub async fn run(state: Shared, req_id: u64, backend: Backend, op: Op) {
    let mut p = Progress { state: state.clone(), req_id, name: String::new(), done: 0, total: 0, sent: 0 };
    let result = match backend {
        Backend::Local => local(&mut p, op).await,
        Backend::Remote { conn, label } => match conn.upgrade() {
            Some(conn) => match conn.sftp().await {
                Ok(sftp) => remote(&mut p, &sftp, &label, op).await,
                Err(e) => Err(e),
            },
            None => Err("SSH 连接已断开".into()),
        },
    };
    reply(&state, req_id, result);
}

/// 文件树复制粘贴：source 为 None 表示源在本地，Some 为源主机（经连接池复用 / 重连，按需弹认证提示）；
/// 目标为会话的文件后端。两端都是远程（含同一主机）时按块读出再写入，经 daemon 流式中转，不落临时文件
pub async fn copy(state: Shared, req_id: u64, dest: Backend, source: Option<Host>, items: Vec<CopyItem>, dest_dir: String) {
    let mut p = Progress { state: state.clone(), req_id, name: String::new(), done: 0, total: 0, sent: 0 };
    let result = async {
        // 传输期间持有两端连接，结束后释放（源主机已没有会话时连接随之断开）
        let src_conn = match &source {
            Some(host) => Some(ssh::connection(&state, host, false).await?),
            None => None,
        };
        let dst_conn = match dest {
            Backend::Local => None,
            Backend::Remote { conn, .. } => Some(conn.upgrade().ok_or("SSH 连接已断开")?),
        };
        let src_sftp = match &src_conn {
            Some(c) => Some(c.sftp().await?),
            None => None,
        };
        let dst_sftp = match &dst_conn {
            Some(c) => Some(c.sftp().await?),
            None => None,
        };
        let src = src_sftp.as_deref().map_or(End::Local, End::Remote);
        let dst = dst_sftp.as_deref().map_or(End::Local, End::Remote);
        copy_items(&mut p, src, dst, items, &dest_dir).await?;
        Ok(Reply::Done)
    }
    .await;
    reply(&state, req_id, result);
}

fn reply(state: &Shared, req_id: u64, result: Res<Reply>) {
    let msg = match result {
        Ok(Reply::Entries(path, entries)) => ServerMsg::FsEntries { req_id, path, entries },
        Ok(Reply::Content(path, f)) => {
            ServerMsg::FsContent { req_id, path, text: f.text, encoding: f.encoding, crlf: f.crlf, mtime: f.mtime, size: f.size }
        }
        Ok(Reply::Saved { conflict, mtime, size }) => ServerMsg::FsSaved { req_id, conflict, mtime, size },
        Ok(Reply::Done) => ServerMsg::FsResult { req_id, error: None },
        Err(error) => {
            log!("文件操作失败（请求 {req_id}）：{error}");
            ServerMsg::FsResult { req_id, error: Some(error) }
        }
    };
    state.lock().unwrap().send(&msg);
}

/// 给错误加上路径前缀
fn at<E: Display>(path: &str) -> impl FnOnce(E) -> String + '_ {
    move |e| format!("{path}：{e}")
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Res<T> + Send + 'static) -> Res<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

async fn local(p: &mut Progress, op: Op) -> Res<Reply> {
    match op {
        Op::List(path) => {
            let dir = path.clone();
            let entries = blocking(move || if dir.is_empty() { Ok(drives()) } else { list_local(&dir) }).await?;
            return Ok(Reply::Entries(path, entries));
        }
        Op::Mkdir(path) => tokio::fs::create_dir(&path).await.map_err(at(&path))?,
        Op::CreateFile(path) => drop(tokio::fs::OpenOptions::new().write(true).create_new(true).open(&path).await.map_err(at(&path))?),
        Op::Rename(from, to) => {
            // Windows 上 rename 会覆盖已存在的目标文件；仅大小写不同的改名放行
            if !from.eq_ignore_ascii_case(&to) && tokio::fs::try_exists(&to).await.unwrap_or(false) {
                return Err(format!("{to}：已存在"));
            }
            tokio::fs::rename(&from, &to).await.map_err(at(&from))?
        }
        Op::Delete(path) => blocking(move || remove_local(&path)).await?,
        Op::Open(path, mode) => blocking(move || open_with(Path::new(&path), mode)).await?,
        Op::Read(path) => {
            let text = read_text(End::Local, &path).await?;
            return Ok(Reply::Content(path, text));
        }
        Op::Write(w) => return write_text(End::Local, *w).await,
        // 本地会话的上传 / 下载都是复制
        Op::Upload(srcs, dir) | Op::Download(srcs, dir) => {
            let items = blocking(move || walk_local(&local_tops(&srcs)?, &dir, false)).await?;
            transfer(p, End::Local, End::Local, items).await?
        }
    }
    Ok(Reply::Done)
}

async fn remote(p: &mut Progress, sftp: &SftpSession, label: &str, op: Op) -> Res<Reply> {
    match op {
        Op::List(path) => {
            let mut entries: Vec<FsEntry> = sftp
                .read_dir(path.as_str())
                .await
                .map_err(at(&path))?
                .map(|e| {
                    let md = e.metadata();
                    let ft = md.file_type();
                    FsEntry {
                        name: e.file_name(),
                        is_dir: ft.is_dir(),
                        is_link: ft.is_symlink(),
                        size: md.size.unwrap_or(0),
                        mtime: md.mtime.map_or(0, |t| t as u64 * 1000),
                    }
                })
                .collect();
            sort_entries(&mut entries);
            return Ok(Reply::Entries(path, entries));
        }
        Op::Mkdir(path) => sftp.create_dir(path.as_str()).await.map_err(at(&path))?,
        Op::CreateFile(path) => {
            let flags = OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE;
            let mut f = sftp.open_with_flags(path.as_str(), flags).await.map_err(at(&path))?;
            f.shutdown().await.map_err(at(&path))?
        }
        Op::Rename(from, to) => sftp.rename(from.as_str(), to.as_str()).await.map_err(at(&from))?,
        Op::Delete(path) => remove_remote(sftp, &path).await?,
        Op::Open(path, mode) => {
            let local = fetch_for_open(p, sftp, label, &path).await?;
            blocking(move || open_with(&local, mode)).await?
        }
        Op::Read(path) => {
            let text = read_text(End::Remote(sftp), &path).await?;
            return Ok(Reply::Content(path, text));
        }
        Op::Write(w) => return write_text(End::Remote(sftp), *w).await,
        Op::Upload(srcs, dir) => {
            let items = blocking(move || walk_local(&local_tops(&srcs)?, &dir, true)).await?;
            transfer(p, End::Local, End::Remote(sftp), items).await?
        }
        Op::Download(srcs, dir) => {
            let items = walk_remote(sftp, &remote_tops(&srcs)?, &dir, false).await?;
            transfer(p, End::Remote(sftp), End::Local, items).await?
        }
    }
    Ok(Reply::Done)
}

// ---------------- 列目录 / 删除 ----------------

/// 存在的盘符（`C:\` 形式）
fn drives() -> Vec<FsEntry> {
    (b'A'..=b'Z')
        .map(|c| format!("{}:\\", c as char))
        .filter(|root| Path::new(root).exists())
        .map(|name| FsEntry { name, is_dir: true, is_link: false, size: 0, mtime: 0 })
        .collect()
}

fn list_local(path: &str) -> Res<Vec<FsEntry>> {
    let mut entries = Vec::new();
    for e in std::fs::read_dir(path).map_err(at(path))?.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let is_link = ft.is_symlink();
        // 符号链接 / 联接按目标判断是否目录；普通项直接用目录枚举带回的元数据
        let md = if is_link { std::fs::metadata(e.path()) } else { e.metadata() }.ok();
        entries.push(FsEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: md.as_ref().is_some_and(|m| m.is_dir()),
            is_link,
            size: md.as_ref().map_or(0, |m| if m.is_dir() { 0 } else { m.len() }),
            mtime: md.and_then(|m| m.modified().ok()).map_or(0, unix_ms),
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// 目录在前，名称不区分大小写排序
fn sort_entries(entries: &mut [FsEntry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

/// 文件夹递归删除（不跟随其中的符号链接 / 联接）；目录链接本身用 remove_dir 删除
fn remove_local(path: &str) -> Res<()> {
    let md = std::fs::symlink_metadata(path).map_err(at(path))?;
    if md.is_dir() {
        return std::fs::remove_dir_all(path).map_err(at(path));
    }
    std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path)).map_err(at(path))
}

/// 远程递归删除：先序收集目录并删除其中的文件（符号链接按文件删除，不跟随），最后逆序删目录
async fn remove_remote(sftp: &SftpSession, path: &str) -> Res<()> {
    let md = sftp.symlink_metadata(path).await.map_err(at(path))?;
    if !md.file_type().is_dir() {
        return sftp.remove_file(path).await.map_err(at(path));
    }
    let mut dirs = vec![path.to_string()];
    let mut i = 0;
    while i < dirs.len() {
        let dir = dirs[i].clone();
        i += 1;
        for e in sftp.read_dir(dir.as_str()).await.map_err(at(&dir))? {
            let p = e.path();
            if e.file_type().is_dir() {
                dirs.push(p);
            } else {
                sftp.remove_file(p.as_str()).await.map_err(at(&p))?;
            }
        }
    }
    for d in dirs.iter().rev() {
        sftp.remove_dir(d.as_str()).await.map_err(at(d))?;
    }
    Ok(())
}

// ---------------- 读取 / 写回 ----------------

/// 读取文本：最多读 MAX_READ + 1 字节，超出即拒绝（不依赖元数据中的大小，远程可能缺失）；二进制见 looks_binary，编码见 decode_text。
/// 读取前记下 mtime / size，供写回时检测外部修改
async fn read_text(end: End<'_>, path: &str) -> Res<TextFile> {
    let (mtime, size) = end.stat(path).await?;
    let mut buf = Vec::new();
    end.reader(path).await?.take(MAX_READ + 1).read_to_end(&mut buf).await.map_err(at(path))?;
    if buf.len() as u64 > MAX_READ {
        return Err(format!("{path}：文件超过 {} MB，不支持预览，请用默认程序打开", MAX_READ / 1024 / 1024));
    }
    if looks_binary(&buf) {
        return Err(format!("{path}：看起来是二进制文件，无法以文本打开"));
    }
    let (text, encoding) = decode_text(buf);
    let crlf = crlf_majority(&text);
    Ok(TextFile { text, encoding, crlf, mtime, size })
}

/// 前 BINARY_PROBE 字节含 NUL 字节，且不是带 BOM 的 UTF-16 文本
fn looks_binary(buf: &[u8]) -> bool {
    !buf.starts_with(b"\xFF\xFE") && !buf.starts_with(b"\xFE\xFF") && buf[..buf.len().min(BINARY_PROBE)].contains(&0)
}

/// 解码文本：有 BOM 按 BOM（UTF-8 / UTF-16 LE / BE，并去掉 BOM）；否则合法 UTF-8 直接用，
/// 不合法时按系统 ANSI 代码页（中文系统为 GBK）严格解码，仍失败则 UTF-8 lossy。
/// 编码为 None 表示有非法字节被替换（按原编码写回会损坏文件）
fn decode_text(buf: Vec<u8>) -> (String, Option<TextEncoding>) {
    if let Some(b) = buf.strip_prefix(b"\xEF\xBB\xBF") {
        return match std::str::from_utf8(b) {
            Ok(s) => (s.to_string(), Some(TextEncoding::Utf8Bom)),
            Err(_) => (String::from_utf8_lossy(b).into_owned(), None),
        };
    }
    let utf16 = |b: &[u8], f: fn([u8; 2]) -> u16, enc| {
        let (units, rest) = b.as_chunks::<2>();
        let units: Vec<u16> = units.iter().map(|c| f(*c)).collect();
        match String::from_utf16(&units) {
            Ok(s) if rest.is_empty() => (s, Some(enc)),
            _ => (String::from_utf16_lossy(&units), None),
        }
    };
    if let Some(b) = buf.strip_prefix(b"\xFF\xFE") {
        return utf16(b, u16::from_le_bytes, TextEncoding::Utf16Le);
    }
    if let Some(b) = buf.strip_prefix(b"\xFE\xFF") {
        return utf16(b, u16::from_be_bytes, TextEncoding::Utf16Be);
    }
    match String::from_utf8(buf) {
        Ok(s) => (s, Some(TextEncoding::Utf8)),
        Err(e) => {
            let buf = e.into_bytes();
            match ansi_decode(&buf) {
                Some(s) => (s, Some(TextEncoding::Ansi)),
                None => (String::from_utf8_lossy(&buf).into_owned(), None),
            }
        }
    }
}

/// 按系统 ANSI 代码页严格解码（遇非法字节返回 None）；代码页本身是 UTF-8 时无意义，直接返回 None
fn ansi_decode(buf: &[u8]) -> Option<String> {
    use windows_sys::Win32::Globalization::{CP_ACP, CP_UTF8, GetACP, MB_ERR_INVALID_CHARS, MultiByteToWideChar};
    let len = i32::try_from(buf.len()).ok()?;
    // SAFETY: 输入 / 输出缓冲区及其长度均来自有效切片
    unsafe {
        if GetACP() == CP_UTF8 {
            return None;
        }
        let n = MultiByteToWideChar(CP_ACP, MB_ERR_INVALID_CHARS, buf.as_ptr(), len, std::ptr::null_mut(), 0);
        if n <= 0 {
            return None;
        }
        let mut wide = vec![0u16; n as usize];
        let n = MultiByteToWideChar(CP_ACP, MB_ERR_INVALID_CHARS, buf.as_ptr(), len, wide.as_mut_ptr(), n);
        (n > 0).then(|| String::from_utf16_lossy(&wide[..n as usize]))
    }
}

/// 换行风格：CRLF 多于单独的 LF 时为 true（没有换行时为 LF）
fn crlf_majority(text: &str) -> bool {
    let crlf = text.matches("\r\n").count();
    crlf > text.matches('\n').count() - crlf
}

/// 编码要写回的文本：换行先统一为 LF 再按 crlf 转换，BOM 按原编码保留；
/// ANSI 代码页无法表示的字符报错（不静默替换、也不擅自改存为 UTF-8）
fn encode_text(text: &str, encoding: TextEncoding, crlf: bool) -> Res<Vec<u8>> {
    let lf = text.replace("\r\n", "\n");
    let text = if crlf { lf.replace('\n', "\r\n") } else { lf.clone() };
    let utf16 = |bom: &[u8], f: fn(u16) -> [u8; 2]| bom.iter().copied().chain(text.encode_utf16().flat_map(f)).collect();
    Ok(match encoding {
        TextEncoding::Utf8 => text.into_bytes(),
        TextEncoding::Utf8Bom => [b"\xEF\xBB\xBF", text.as_bytes()].concat(),
        TextEncoding::Utf16Le => utf16(b"\xFF\xFE", u16::to_le_bytes),
        TextEncoding::Utf16Be => utf16(b"\xFE\xFF", u16::to_be_bytes),
        TextEncoding::Ansi => ansi_encode(&text).ok_or_else(|| {
            // 只在出错时逐字符定位第一个无法表示的字符
            let (line, c) = lf
                .split('\n')
                .enumerate()
                .find_map(|(i, l)| l.chars().find(|c| ansi_encode(&c.to_string()).is_none()).map(|c| (i + 1, c)))
                .unwrap_or((0, '?'));
            format!("第 {line} 行的字符“{c}”无法用文件原编码（系统代码页 {}）保存，未写入；删去该字符后再保存", acp())
        })?,
    })
}

fn acp() -> u32 {
    // SAFETY: 无参数的系统调用
    unsafe { windows_sys::Win32::Globalization::GetACP() }
}

/// 按系统 ANSI 代码页严格编码：有字符无法表示（会被替换为默认字符）时返回 None
fn ansi_encode(text: &str) -> Option<Vec<u8>> {
    use windows_sys::Win32::Globalization::{CP_ACP, WC_NO_BEST_FIT_CHARS, WideCharToMultiByte};
    if text.is_empty() {
        return Some(Vec::new());
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    let len = i32::try_from(wide.len()).ok()?;
    let mut used = 0;
    // SAFETY: 输入 / 输出缓冲区及其长度均来自有效切片
    unsafe {
        let n = WideCharToMultiByte(CP_ACP, WC_NO_BEST_FIT_CHARS, wide.as_ptr(), len, std::ptr::null_mut(), 0, std::ptr::null(), &mut used);
        if n <= 0 || used != 0 {
            return None;
        }
        let mut out = vec![0u8; n as usize];
        let n = WideCharToMultiByte(CP_ACP, WC_NO_BEST_FIT_CHARS, wide.as_ptr(), len, out.as_mut_ptr(), n, std::ptr::null(), &mut used);
        (n > 0 && used == 0).then(|| {
            out.truncate(n as usize);
            out
        })
    }
}

/// 写回文本：先比对 mtime / size（force 跳过；文件已不存在也算冲突），一致才写入；回复写入后的 mtime / size
async fn write_text(end: End<'_>, w: WriteReq) -> Res<Reply> {
    let bytes = encode_text(&w.text, w.encoding, w.crlf).map_err(|e| format!("{}：{e}", w.path))?;
    if !w.force {
        let cur = end.stat(&w.path).await.ok();
        if cur != Some((w.mtime, w.size)) {
            let (mtime, size) = cur.unwrap_or((0, 0));
            return Ok(Reply::Saved { conflict: true, mtime, size });
        }
    }
    match end {
        End::Local => {
            let path = w.path.clone();
            blocking(move || replace_local(&path, &bytes)).await?
        }
        // SFTP 没有原子替换：打开已有文件截断后写入（保留原权限与属主），不存在时新建
        End::Remote(sftp) => {
            let flags = OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE;
            let mut f = sftp.open_with_flags(w.path.as_str(), flags).await.map_err(at(&w.path))?;
            f.write_all(&bytes).await.map_err(at(&w.path))?;
            f.shutdown().await.map_err(at(&w.path))?
        }
    }
    let (mtime, size) = end.stat(&w.path).await?;
    Ok(Reply::Saved { conflict: false, mtime, size })
}

/// 本地原子写：同目录写临时文件并落盘，再 ReplaceFileW 替换（保留原文件的属性、ACL、创建时间）；
/// 符号链接写到其目标；只读文件拒绝；文件已不存在时直接新建
fn replace_local(path: &str, bytes: &[u8]) -> Res<()> {
    use std::io::Write;
    let Ok(target) = std::fs::canonicalize(path) else {
        return std::fs::write(path, bytes).map_err(at(path));
    };
    if std::fs::metadata(&target).map_err(at(path))?.permissions().readonly() {
        return Err(format!("{path}：文件为只读，未写入"));
    }
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.{}.tether-tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        replace_file(&target, &tmp)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(at(path))
}

/// ReplaceFileW 失败时（如部分网络文件系统不支持）退回 std::fs::rename：Windows 上同样原子覆盖已存在的目标，但不保留 ACL
fn replace_file(target: &Path, tmp: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_IGNORE_ACL_ERRORS, REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW};
    let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (t, r) = (wide(target), wide(tmp));
    let flags = REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS;
    // SAFETY: 两个路径均为以 0 结尾的宽字符串，其余指针参数允许为空
    let ok = unsafe { ReplaceFileW(t.as_ptr(), r.as_ptr(), std::ptr::null(), flags, std::ptr::null(), std::ptr::null()) };
    if ok != 0 {
        return Ok(());
    }
    log!("ReplaceFileW 失败，改用 rename：{}", std::io::Error::last_os_error());
    std::fs::rename(tmp, target)
}

// ---------------- 打开 ----------------

fn open_with(path: &Path, mode: OpenMode) -> Res<()> {
    match mode {
        OpenMode::Default => shell_open(path),
        OpenMode::Browser => browser_open(&file_url(path)).or_else(|e| {
            log!("默认浏览器打开失败，改用默认程序：{e}");
            shell_open(path)
        }),
    }
}

/// Markdown 预览中的外链：仅 http(s)，用默认浏览器打开，失败回退 explorer.exe；仍失败时回 Error
pub async fn open_url(state: Shared, url: String) {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return log!("忽略非 http(s) 链接：{url}");
    }
    // 链接作为命令行参数传给浏览器：空白、引号、控制字符一律百分号编码，避免破坏参数边界
    let url: String = url
        .chars()
        .map(|c| if c.is_whitespace() || c.is_control() || c == '"' { pct(c) } else { c.to_string() })
        .collect();
    let result = blocking(move || {
        browser_open(&url).or_else(|e| {
            log!("默认浏览器打开失败，改用 explorer.exe：{e}");
            shell_open(Path::new(&url))
        })
    })
    .await;
    if let Err(message) = result {
        state.lock().unwrap().send(&ServerMsg::Error { message });
    }
}

/// 字符按 UTF-8 字节百分号编码
fn pct(c: char) -> String {
    c.to_string().bytes().map(|b| format!("%{b:02X}")).collect()
}

/// 用默认浏览器打开 URL（直接 CreateProcess，不经 shell）
fn browser_open(url: &str) -> Res<()> {
    let cmdline = default_browser_command()?;
    let (exe, args) = browser_command(&cmdline, url).ok_or_else(|| format!("无法解析浏览器命令行：{cmdline}"))?;
    Command::new(&exe).raw_arg(&args).spawn().map(drop).map_err(|e| format!("启动 {exe} 失败：{e}"))
}

/// 默认浏览器的打开命令行模板：https（取不到再试 http）协议 UserChoice 的 ProgId → HKCR\<ProgId>\shell\open\command
fn default_browser_command() -> Res<String> {
    const ASSOC: &str = r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations";
    let prog_id = ["https", "http"]
        .iter()
        .find_map(|scheme| {
            let key = HKCU.open_subkey(format!(r"{ASSOC}\{scheme}\UserChoice")).ok()?;
            key.get_value::<String, _>("ProgId").ok().filter(|s| !s.is_empty())
        })
        .ok_or("未找到默认浏览器")?;
    HKCR.open_subkey(format!(r"{prog_id}\shell\open\command"))
        .and_then(|k| k.get_value::<String, _>(""))
        .map_err(|e| format!("读取 {prog_id} 的打开命令失败：{e}"))
}

/// 由命令行模板生成（可执行文件, 参数串）：可执行文件路径可带引号；第一种出现的占位符 `%1` / `%L` 原样替换为 URL
/// （保留模板自带的引号，不额外加引号——Chrome / Edge 的 `--single-argument %1` 会把引号当作 URL 的一部分），
/// 没有占位符时追加到末尾；`%*` 去掉。调用方保证 URL 不含空白与引号（已百分号编码），不加引号也不会破坏参数边界
fn browser_command(cmdline: &str, url: &str) -> Option<(String, String)> {
    let line = cmdline.trim();
    let (exe, rest) = match line.strip_prefix('"') {
        Some(s) => {
            let end = s.find('"')?;
            (&s[..end], &s[end + 1..])
        }
        None => {
            // 未加引号：优先截到 .exe（路径可能含空格），否则截到第一个空白
            let lower = line.to_ascii_lowercase();
            let end = lower.find(".exe").map(|i| i + 4).or_else(|| line.find(char::is_whitespace)).unwrap_or(line.len());
            line.split_at(end)
        }
    };
    if exe.is_empty() {
        return None;
    }
    let rest = rest.replace("%*", "");
    let args = match ["%1", "%L", "%l"].into_iter().find(|ph| rest.contains(ph)) {
        Some(ph) => rest.replace(ph, url),
        None => format!("{} {url}", rest.trim()),
    };
    Some((exe.to_string(), args.trim().to_string()))
}

/// 本地路径转 file URL：反斜杠转正斜杠，字母数字与 `-._~/:` 以外的字节（含空格、`#`、`%`、`?`、中文）按 UTF-8 百分号编码；
/// UNC 路径 `\\server\share\x` 转为 `file://server/share/x`
fn file_url(path: &Path) -> String {
    let s = path.display().to_string().replace('\\', "/");
    let (mut out, rest) = match s.strip_prefix("//") {
        Some(unc) => (String::from("file://"), unc),
        None => (String::from("file:///"), s.as_str()),
    };
    for b in rest.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 用系统默认程序打开（explorer.exe 按文件关联打开；路径总是加引号，避免逗号等被 explorer 当作分隔符）
fn shell_open(path: &Path) -> Res<()> {
    Command::new("explorer.exe")
        .raw_arg(format!("\"{}\"", path.display()))
        .spawn()
        .map(drop)
        .map_err(|e| format!("打开 {} 失败：{e}", path.display()))
}

/// 远程文件下载到 %TEMP%\tether\<user@host_port>\<远程绝对路径各段>，保留路径层级避免同名冲突；返回本地路径
async fn fetch_for_open(p: &mut Progress, sftp: &SftpSession, label: &str, path: &str) -> Res<PathBuf> {
    let mut local = std::env::temp_dir().join("tether").join(sanitize(label));
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        local.push(sanitize(seg));
    }
    if let Some(dir) = local.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(at(&dir.display().to_string()))?;
    }
    let size = sftp.metadata(path).await.map_err(at(path))?.size.unwrap_or(0);
    let name = remote_name(path).unwrap_or(path).to_string();
    let item = Item::File { src: path.into(), dst: local.display().to_string(), size, name };
    transfer(p, End::Remote(sftp), End::Local, vec![item]).await?;
    Ok(local)
}

/// 替换 Windows 文件名中的非法字符
fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect()
}

// ---------------- 传输 ----------------

/// 传输的一端
#[derive(Clone, Copy)]
enum End<'a> {
    Local,
    Remote(&'a SftpSession),
}

/// 传输计划中的一项：先建目录，再传文件（目录总在其内容之前）
enum Item {
    Dir(String),
    File { src: String, dst: String, size: u64, name: String },
}

impl End<'_> {
    /// 确保目录存在（已存在则合并）
    async fn ensure_dir(self, path: &str) -> Res<()> {
        match self {
            End::Local => tokio::fs::create_dir_all(path).await.map_err(at(path)),
            End::Remote(sftp) => match sftp.create_dir(path).await {
                Ok(()) => Ok(()),
                Err(e) => match sftp.metadata(path).await {
                    Ok(md) if md.file_type().is_dir() => Ok(()),
                    _ => Err(format!("{path}：{e}")),
                },
            },
        }
    }

    /// 路径是否已存在（不跟随符号链接；断链也算存在）
    async fn exists(self, path: &str) -> bool {
        match self {
            End::Local => tokio::fs::symlink_metadata(path).await.is_ok(),
            End::Remote(sftp) => sftp.symlink_metadata(path).await.is_ok(),
        }
    }

    /// 是否为目录（跟随符号链接）
    async fn is_dir(self, path: &str) -> Res<bool> {
        match self {
            End::Local => tokio::fs::metadata(path).await.map(|m| m.is_dir()).map_err(at(path)),
            End::Remote(sftp) => sftp.metadata(path).await.map(|m| m.file_type().is_dir()).map_err(at(path)),
        }
    }

    /// (mtime Unix 毫秒, 大小)，跟随符号链接；远程 mtime 精度为秒
    async fn stat(self, path: &str) -> Res<(u64, u64)> {
        match self {
            End::Local => {
                let md = tokio::fs::metadata(path).await.map_err(at(path))?;
                Ok((md.modified().map_or(0, unix_ms), md.len()))
            }
            End::Remote(sftp) => {
                let md = sftp.metadata(path).await.map_err(at(path))?;
                Ok((md.mtime.map_or(0, |t| t as u64 * 1000), md.size.unwrap_or(0)))
            }
        }
    }

    async fn reader(self, path: &str) -> Res<Box<dyn AsyncRead + Unpin + Send>> {
        Ok(match self {
            End::Local => Box::new(tokio::fs::File::open(path).await.map_err(at(path))?),
            End::Remote(sftp) => Box::new(sftp.open(path).await.map_err(at(path))?),
        })
    }

    /// 同名文件直接覆盖
    async fn writer(self, path: &str) -> Res<Box<dyn AsyncWrite + Unpin + Send>> {
        Ok(match self {
            End::Local => Box::new(tokio::fs::File::create(path).await.map_err(at(path))?),
            End::Remote(sftp) => Box::new(sftp.create(path).await.map_err(at(path))?),
        })
    }
}

/// 按计划逐项传输
async fn transfer(p: &mut Progress, src: End<'_>, dst: End<'_>, items: Vec<Item>) -> Res<()> {
    p.total = items.iter().map(|i| if let Item::File { size, .. } = i { *size } else { 0 }).sum();
    p.emit();
    let mut buf = vec![0u8; CHUNK];
    for item in items {
        let (s, d, size, name) = match item {
            Item::Dir(d) => {
                dst.ensure_dir(&d).await?;
                continue;
            }
            Item::File { src, dst, size, name } => (src, dst, size, name),
        };
        p.name = name;
        // 本地复制到原位置（如把文件拖回它所在的目录）时跳过，否则会截断源文件
        if matches!((src, dst), (End::Local, End::Local)) && same_file(&s, &d).await {
            p.done += size;
            p.emit();
            continue;
        }
        let mut r = src.reader(&s).await?;
        let mut w = dst.writer(&d).await?;
        loop {
            let n = r.read(&mut buf).await.map_err(at(&s))?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n]).await.map_err(at(&d))?;
            p.add(n as u64);
        }
        w.shutdown().await.map_err(at(&d))?;
        p.emit();
    }
    Ok(())
}

async fn same_file(a: &str, b: &str) -> bool {
    matches!((tokio::fs::canonicalize(a).await, tokio::fs::canonicalize(b).await), (Ok(x), Ok(y)) if x == y)
}

/// 目标路径拼接：远程一律 `/`，本地用 PathBuf
fn join(remote: bool, dir: &str, name: &str) -> String {
    match (remote, dir.ends_with('/')) {
        (true, true) => format!("{dir}{name}"),
        (true, false) => format!("{dir}/{name}"),
        (false, _) => Path::new(dir).join(name).display().to_string(),
    }
}

/// 远程路径的最后一段
fn remote_name(path: &str) -> Option<&str> {
    path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty())
}

/// 本地源路径与其在目标目录下的名称（取最后一段）
fn local_tops(srcs: &[String]) -> Res<Vec<(String, String)>> {
    srcs.iter()
        .map(|src| {
            let name = Path::new(src).file_name().ok_or_else(|| format!("{src}：不支持传输整个磁盘"))?;
            Ok((src.clone(), name.to_string_lossy().into_owned()))
        })
        .collect()
}

/// 远程源路径与其在目标目录下的名称（取最后一段）
fn remote_tops(srcs: &[String]) -> Res<Vec<(String, String)>> {
    srcs.iter()
        .map(|src| Ok((src.clone(), remote_name(src).ok_or_else(|| format!("{src}：不支持传输根目录"))?.to_string())))
        .collect()
}

/// 展开本地源路径为传输计划；tops 为（源路径, 目标目录下的名称）（跟随链接；目标端为远程时用 `/` 拼接）
fn walk_local(tops: &[(String, String)], dest_dir: &str, dest_remote: bool) -> Res<Vec<Item>> {
    let mut items = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = Vec::new();
    for (src, name) in tops {
        let path = Path::new(src);
        let dst = join(dest_remote, dest_dir, name);
        let md = std::fs::metadata(path).map_err(at(src))?;
        if md.is_dir() {
            stack.push((path.to_path_buf(), dst));
        } else {
            items.push(Item::File { src: src.clone(), dst, size: md.len(), name: name.clone() });
        }
    }
    while let Some((dir, dst)) = stack.pop() {
        items.push(Item::Dir(dst.clone()));
        let dir_str = dir.display().to_string();
        for e in std::fs::read_dir(&dir).map_err(at(&dir_str))?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let target = join(dest_remote, &dst, &name);
            let Ok(md) = std::fs::metadata(e.path()) else { continue };
            if md.is_dir() {
                stack.push((e.path(), target));
            } else {
                items.push(Item::File { src: e.path().display().to_string(), dst: target, size: md.len(), name });
            }
        }
    }
    Ok(items)
}

/// 展开远程源路径为传输计划；tops 为（源路径, 目标目录下的名称）；目标为本地时名称中的非法字符替换为 `_`；
/// 符号链接跟随到目标，断链跳过
async fn walk_remote(sftp: &SftpSession, tops: &[(String, String)], dest_dir: &str, dest_remote: bool) -> Res<Vec<Item>> {
    let fix = |name: &str| if dest_remote { name.to_string() } else { sanitize(name) };
    let mut items = Vec::new();
    let mut stack: Vec<(String, String, usize)> = Vec::new();
    for (src, name) in tops {
        let dst = join(dest_remote, dest_dir, &fix(name));
        let md = sftp.metadata(src.as_str()).await.map_err(at(src))?;
        if md.file_type().is_dir() {
            stack.push((src.clone(), dst, 0));
        } else {
            items.push(Item::File { src: src.clone(), dst, size: md.size.unwrap_or(0), name: name.clone() });
        }
    }
    while let Some((dir, dst, depth)) = stack.pop() {
        items.push(Item::Dir(dst.clone()));
        for e in sftp.read_dir(dir.as_str()).await.map_err(at(&dir))? {
            let (path, name) = (e.path(), e.file_name());
            let target = join(dest_remote, &dst, &fix(&name));
            let mut md = e.metadata();
            if md.file_type().is_symlink() {
                match sftp.metadata(path.as_str()).await {
                    Ok(m) => md = m,
                    Err(_) => continue,
                }
            }
            if !md.file_type().is_dir() {
                items.push(Item::File { src: path, dst: target, size: md.size.unwrap_or(0), name });
            } else if depth < MAX_DEPTH {
                stack.push((path, target, depth + 1));
            }
        }
    }
    Ok(items)
}

/// 复制粘贴：确定各项在目标目录下的名称（keep_both 时避开已存在的名称与本次已占用的名称），再展开并传输
async fn copy_items(p: &mut Progress, src: End<'_>, dst: End<'_>, items: Vec<CopyItem>, dest_dir: &str) -> Res<()> {
    let dest_remote = matches!(dst, End::Remote(_));
    let paths: Vec<String> = items.iter().map(|i| i.path.clone()).collect();
    let named = if matches!(src, End::Remote(_)) { remote_tops(&paths)? } else { local_tops(&paths)? };
    // 本次已占用的名称：本地目标不区分大小写
    let key = |n: &str| if dest_remote { n.to_string() } else { n.to_lowercase() };
    let mut taken = HashSet::new();
    let mut tops = Vec::with_capacity(named.len());
    for ((path, name), item) in named.into_iter().zip(items) {
        let mut name = if dest_remote { name } else { sanitize(&name) };
        if item.keep_both && (taken.contains(&key(&name)) || dst.exists(&join(dest_remote, dest_dir, &name)).await) {
            let is_dir = src.is_dir(&path).await?;
            let mut found = None;
            for n in 1..=MAX_RENAME {
                let cand = keep_both_name(&name, is_dir, n);
                if !taken.contains(&key(&cand)) && !dst.exists(&join(dest_remote, dest_dir, &cand)).await {
                    found = Some(cand);
                    break;
                }
            }
            name = found.ok_or_else(|| format!("{name}：找不到可用的新名称"))?;
        }
        taken.insert(key(&name));
        tops.push((path, name));
    }
    let plan = match src {
        End::Local => {
            let dir = dest_dir.to_string();
            blocking(move || walk_local(&tops, &dir, dest_remote)).await?
        }
        End::Remote(sftp) => walk_remote(sftp, &tops, dest_dir, dest_remote).await?,
    };
    transfer(p, src, dst, plan).await
}

/// “都保留”的第 n 个候选名：文件为 `name (n).ext`（扩展名取最后一个点之后；以点开头的隐藏文件如 `.bashrc` 视为无扩展名），
/// 文件夹为 `name (n)`
fn keep_both_name(name: &str, is_dir: bool, n: u32) -> String {
    match name.rfind('.').filter(|&i| !is_dir && i > 0) {
        Some(i) => format!("{} ({n}){}", &name[..i], &name[i..]),
        None => format!("{name} ({n})"),
    }
}

/// 传输进度：按字节步长节流推送
struct Progress {
    state: Shared,
    req_id: u64,
    name: String,
    done: u64,
    total: u64,
    sent: u64,
}

impl Progress {
    fn add(&mut self, n: u64) {
        self.done += n;
        if self.done - self.sent >= PROGRESS_STEP {
            self.emit();
        }
    }

    fn emit(&mut self) {
        self.sent = self.done;
        let msg = ServerMsg::Transfer { req_id: self.req_id, name: self.name.clone(), done: self.done, total: self.total };
        self.state.lock().unwrap().send(&msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "file:///C:/a%20b/x.html";

    #[test]
    fn quoted_exe_with_placeholder() {
        let cmd = r#""C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" --single-argument %1"#;
        let (exe, args) = browser_command(cmd, URL).unwrap();
        assert_eq!(exe, r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe");
        assert_eq!(args, format!("--single-argument {URL}"));
    }

    #[test]
    fn quoted_placeholder_keeps_template_quotes() {
        let cmd = r#""C:\Program Files\Mozilla Firefox\firefox.exe" -osint -url "%1""#;
        let (exe, args) = browser_command(cmd, URL).unwrap();
        assert_eq!(exe, r"C:\Program Files\Mozilla Firefox\firefox.exe");
        assert_eq!(args, format!("-osint -url \"{URL}\""));
    }

    #[test]
    fn no_placeholder_appends() {
        let (exe, args) = browser_command(r#""C:\b\browser.exe""#, URL).unwrap();
        assert_eq!((exe.as_str(), args), (r"C:\b\browser.exe", URL.to_string()));
        let (exe, args) = browser_command(r#""C:\b\browser.exe" --new-window"#, URL).unwrap();
        assert_eq!((exe.as_str(), args), (r"C:\b\browser.exe", format!("--new-window {URL}")));
    }

    #[test]
    fn unquoted_exe_with_spaces() {
        let (exe, args) = browser_command(r"C:\Program Files\B\b.exe %1 %*", URL).unwrap();
        assert_eq!((exe.as_str(), args), (r"C:\Program Files\B\b.exe", URL.to_string()));
    }

    #[test]
    fn file_url_encoding() {
        let url = file_url(Path::new(r"C:\Users\me\My Docs\#1 100%\中文 页.html"));
        assert_eq!(url, "file:///C:/Users/me/My%20Docs/%231%20100%25/%E4%B8%AD%E6%96%87%20%E9%A1%B5.html");
        assert_eq!(file_url(Path::new(r"\\srv\share\a b.htm")), "file://srv/share/a%20b.htm");
    }

    #[test]
    fn keep_both_names() {
        assert_eq!(keep_both_name("a.txt", false, 1), "a (1).txt");
        assert_eq!(keep_both_name("a.tar.gz", false, 2), "a.tar (2).gz");
        assert_eq!(keep_both_name("README", false, 1), "README (1)");
        assert_eq!(keep_both_name(".bashrc", false, 1), ".bashrc (1)");
        assert_eq!(keep_both_name(".config.json", false, 3), ".config (3).json");
        assert_eq!(keep_both_name("my.dir", true, 1), "my.dir (1)");
        assert_eq!(keep_both_name("src", true, 12), "src (12)");
    }

    #[test]
    fn decode_text_encodings() {
        use TextEncoding::*;
        let d = |b: &[u8]| decode_text(b.to_vec());
        assert_eq!(d(b"\xEF\xBB\xBF# \xE6\xA0\x87\xE9\xA2\x98\r\n"), ("# 标题\r\n".into(), Some(Utf8Bom)));
        assert_eq!(d("中文 ok".as_bytes()), ("中文 ok".into(), Some(Utf8)));
        assert_eq!(d(b"\xFF\xFEa\x00-N"), ("a中".into(), Some(Utf16Le)));
        assert_eq!(d(b"\xFE\xFF\x00aN-"), ("a中".into(), Some(Utf16Be)));
        assert_eq!(d(b""), (String::new(), Some(Utf8)));
        // 有非法字节被替换的不可写回
        assert_eq!(d(b"\xEF\xBB\xBFa\xFF").1, None);
        assert_eq!(d(b"\xFF\xFEa\x00b").1, None);
        assert_eq!(d(b"\xFF\xFE\x00\xD8").1, None);
        // GBK 的“中文”：中文系统（代码页 936）按 GBK 解码，其它系统至少不 panic
        let gbk = d(b"\xD6\xD0\xCE\xC4");
        if acp() == 936 {
            assert_eq!(gbk, ("中文".into(), Some(Ansi)));
        }
    }

    #[test]
    fn binary_detection() {
        assert!(looks_binary(b"PK\x03\x04\x00\x00"));
        assert!(!looks_binary(b"\xFF\xFEa\x00"));
        assert!(!looks_binary(b"plain\r\ntext"));
        // 只看前 8KB
        let mut late = vec![b'a'; BINARY_PROBE];
        late.push(0);
        assert!(!looks_binary(&late));
    }

    #[test]
    fn newline_style() {
        assert!(!crlf_majority(""));
        assert!(!crlf_majority("no newline"));
        assert!(crlf_majority("a\r\nb\r\nc\n"));
        assert!(!crlf_majority("a\r\nb\nc\n"));
        assert!(!crlf_majority("a\r\nb\n"));
    }

    /// 编码 / 换行往返：读出的文本按原编码与换行写回，字节不变；编辑器给的 LF 文本按原换行风格写回
    #[test]
    fn encode_round_trip() {
        use TextEncoding::*;
        let mut cases: Vec<Vec<u8>> = vec![
            "a: 1\nb: 中文\n".into(),
            "a: 1\r\nb: 中文\r\n".into(),
            b"\xEF\xBB\xBFa\r\n\xE4\xB8\xAD\r\n".to_vec(),
            b"\xFF\xFEa\x00\r\x00\n\x00-N=\xD8\x00\xDE".to_vec(),
            b"\xFE\xFF\x00a\x00\nN-".to_vec(),
            Vec::new(),
        ];
        if acp() == 936 {
            cases.push(b"\xD6\xD0\xCE\xC4\r\nok\r\n".to_vec());
        }
        for bytes in cases {
            let (text, enc) = decode_text(bytes.clone());
            let enc = enc.unwrap();
            let crlf = crlf_majority(&text);
            assert_eq!(encode_text(&text, enc, crlf).unwrap(), bytes, "{enc:?}");
            // 编辑器内部统一 LF
            assert_eq!(encode_text(&text.replace("\r\n", "\n"), enc, crlf).unwrap(), bytes, "{enc:?} (LF)");
        }
        // 混合换行按占多数者统一
        assert_eq!(encode_text("a\nb\r\nc\n", Utf8, true).unwrap(), b"a\r\nb\r\nc\r\n");
        assert_eq!(encode_text("a\r\nb\n", Utf8, false).unwrap(), b"a\nb\n");
        assert_eq!(encode_text("中", Utf8Bom, false).unwrap(), b"\xEF\xBB\xBF\xE4\xB8\xAD");
    }

    /// GBK 无法表示的字符：报错并指出行号，不静默替换
    #[test]
    fn ansi_unencodable() {
        if acp() != 936 {
            return;
        }
        let e = encode_text("ok\n中文😀\n", TextEncoding::Ansi, false).unwrap_err();
        assert!(e.contains("第 2 行") && e.contains('😀'), "{e}");
        assert!(encode_text("中文 ½", TextEncoding::Ansi, false).is_err());
    }

    /// 写回：mtime / size 一致才写入；外部修改或删除后回冲突、不写入；force 覆盖；只读文件拒绝
    #[tokio::test]
    async fn write_conflict() {
        let root = std::env::temp_dir().join(format!("tether-writetest-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let file = root.join("a.yaml");
        write(&file, "a: 1\r\nb: 2\r\n");
        let path = file.display().to_string();
        let f = read_text(End::Local, &path).await.unwrap();
        assert_eq!((f.encoding, f.crlf, f.size), (Some(TextEncoding::Utf8), true, 12));
        let req = |text: &str, mtime, size, force| WriteReq {
            path: path.clone(),
            text: text.into(),
            encoding: TextEncoding::Utf8,
            crlf: true,
            mtime,
            size,
            force,
        };

        let Reply::Saved { conflict: false, mtime, size } = write_text(End::Local, req("a: 3\n", f.mtime, f.size, false)).await.unwrap() else {
            panic!()
        };
        assert_eq!(read(&file), "a: 3\r\n");
        assert_eq!((mtime, size), End::Local.stat(&path).await.unwrap());
        // 没有遗留临时文件
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);

        // 外部修改：大小变化即冲突，文件不动
        write(&file, "external\n");
        let r = write_text(End::Local, req("a: 4\n", mtime, size, false)).await.unwrap();
        assert!(matches!(r, Reply::Saved { conflict: true, size: 9, .. }));
        assert_eq!(read(&file), "external\n");
        // 强制覆盖
        let r = write_text(End::Local, req("a: 4\n", 0, 0, true)).await.unwrap();
        assert!(matches!(r, Reply::Saved { conflict: false, size: 6, .. }));
        assert_eq!(read(&file), "a: 4\r\n");

        // 已删除：冲突（mtime / size 为 0）；强制则新建
        std::fs::remove_file(&file).unwrap();
        let r = write_text(End::Local, req("x\n", mtime, size, false)).await.unwrap();
        assert!(matches!(r, Reply::Saved { conflict: true, mtime: 0, size: 0 }));
        write_text(End::Local, req("x\n", 0, 0, true)).await.unwrap();
        assert_eq!(read(&file), "x\r\n");

        // 只读：拒绝
        let mut perm = std::fs::metadata(&file).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&file, perm.clone()).unwrap();
        assert!(write_text(End::Local, req("y\n", 0, 0, true)).await.is_err());
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&file, perm).unwrap();

        // 二进制拒绝读取
        std::fs::write(&file, b"\x00\x01bin").unwrap();
        assert!(read_text(End::Local, &path).await.is_err_and(|e| e.contains("二进制")));

        std::fs::remove_dir_all(&root).unwrap();
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn item(path: &Path, keep_both: bool) -> CopyItem {
        CopyItem { path: path.display().to_string(), keep_both }
    }

    /// 本地 → 本地复制：文件夹递归、覆盖 / 合并、都保留改名（含同目录粘贴、同批次互不冲突）
    #[tokio::test]
    async fn local_copy() {
        let root = std::env::temp_dir().join(format!("tether-copytest-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (src, dst) = (root.join("src"), root.join("dst"));
        write(&src.join("a.txt"), "new a");
        write(&src.join(".env"), "env");
        write(&src.join("sub").join("b.txt"), "b");
        write(&src.join("sub").join("deep").join("c.txt"), "c");
        write(&dst.join("a.txt"), "old a");
        write(&dst.join("sub").join("b.txt"), "old b");
        write(&dst.join("sub").join("keep.txt"), "keep");

        let state = Shared::default();
        let mut p = Progress { state, req_id: 1, name: String::new(), done: 0, total: 0, sent: 0 };
        let d = dst.display().to_string();
        // 覆盖：文件覆盖，文件夹合并（内部同名文件覆盖，其它保留）
        let items = vec![item(&src.join("a.txt"), false), item(&src.join("sub"), false)];
        copy_items(&mut p, End::Local, End::Local, items, &d).await.unwrap();
        assert_eq!(read(&dst.join("a.txt")), "new a");
        assert_eq!(read(&dst.join("sub").join("b.txt")), "b");
        assert_eq!(read(&dst.join("sub").join("keep.txt")), "keep");
        assert_eq!(read(&dst.join("sub").join("deep").join("c.txt")), "c");
        assert_eq!(p.done, p.total);

        // 都保留：已存在则改名；不冲突的项保持原名
        let items = vec![item(&src.join("a.txt"), true), item(&src.join("sub"), true), item(&src.join(".env"), true)];
        copy_items(&mut p, End::Local, End::Local, items, &d).await.unwrap();
        assert_eq!(read(&dst.join("a (1).txt")), "new a");
        assert_eq!(read(&dst.join("sub (1)").join("deep").join("c.txt")), "c");
        assert_eq!(read(&dst.join(".env")), "env");
        copy_items(&mut p, End::Local, End::Local, vec![item(&src.join("a.txt"), true)], &d).await.unwrap();
        assert_eq!(read(&dst.join("a (2).txt")), "new a");

        // 同目录粘贴：源不被截断，副本改名
        let s = src.display().to_string();
        copy_items(&mut p, End::Local, End::Local, vec![item(&src.join("a.txt"), true), item(&src.join("sub"), true)], &s)
            .await
            .unwrap();
        assert_eq!(read(&src.join("a.txt")), "new a");
        assert_eq!(read(&src.join("a (1).txt")), "new a");
        assert_eq!(read(&src.join("sub (1)").join("b.txt")), "b");

        // 同一批次两个同名项（来自不同目录）互不覆盖
        write(&root.join("other").join("a.txt"), "other a");
        let items = vec![item(&src.join("a.txt"), true), item(&root.join("other").join("a.txt"), true)];
        let fresh = root.join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        copy_items(&mut p, End::Local, End::Local, items, &fresh.display().to_string()).await.unwrap();
        assert_eq!(read(&fresh.join("a.txt")), "new a");
        assert_eq!(read(&fresh.join("a (1).txt")), "other a");

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 只打印本机解析出的默认浏览器命令，不启动：cargo test -p tetherd -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_local_browser() {
        let cmdline = default_browser_command().unwrap();
        println!("命令行模板：{cmdline}");
        let (exe, args) = browser_command(&cmdline, URL).unwrap();
        println!("可执行文件：{exe}\n参数：{args}");
    }
}
