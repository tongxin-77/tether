//! 崩溃留痕：panic 与未处理的 SEH 异常（如访问违规）各写一行到 daemon.log，之后照常交给系统（abort / WER）。

use std::{
    fmt::Write as _,
    fs::File,
    io::Write as _,
    sync::OnceLock,
};

use windows_sys::Win32::System::{
    Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter},
    LibraryLoader::GetModuleHandleW,
};

/// 预先打开的 daemon.log（追加模式）：异常过滤器中只做 WriteFile，不再分配内存 / 打开文件
static LOG: OnceLock<File> = OnceLock::new();
/// EXCEPTION_CONTINUE_SEARCH：继续交给系统默认处理（WER 记录并结束进程）
const CONTINUE_SEARCH: i32 = 0;

pub fn install() {
    if let Some(f) = crate::open_log() {
        let _ = LOG.set(f);
    }
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        log!("panic（线程 {}）：{info}", thread.name().unwrap_or("?"));
    }));
    unsafe { SetUnhandledExceptionFilter(Some(on_exception)) };
}

/// 在栈上格式化，不分配
struct StackBuf {
    buf: [u8; 160],
    len: usize,
}

impl std::fmt::Write for StackBuf {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(std::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

unsafe extern "system" fn on_exception(info: *const EXCEPTION_POINTERS) -> i32 {
    let Some(mut log) = LOG.get() else { return CONTINUE_SEARCH };
    let (code, addr) = match unsafe { info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) } {
        Some(r) => (r.ExceptionCode as u32, r.ExceptionAddress as usize),
        None => (0, 0),
    };
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    let mut line = StackBuf { buf: [0; 160], len: 0 };
    let _ = write!(line, "[{}] 未处理异常 0x{code:08x}，地址 0x{addr:x}", crate::now_ms() / 1000);
    if addr >= base && base != 0 {
        let _ = write!(line, "（tetherd.exe+0x{:x}）", addr - base);
    }
    let _ = writeln!(line);
    let _ = log.write_all(&line.buf[..line.len]);
    CONTINUE_SEARCH
}

/// debug 构建专用：`tetherd --crash-test panic|av` 安装处理器后故意崩溃，用于验证日志
#[cfg(debug_assertions)]
pub fn crash_test(kind: &str) {
    install();
    match kind {
        "panic" => panic!("crash-test"),
        _ => unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u32>(), 1) },
    }
}
