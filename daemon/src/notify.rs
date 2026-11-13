//! 系统通知（Windows toast）与拉起 GUI。

use std::{
    os::windows::process::CommandExt,
    process::Command,
    sync::Mutex,
    time::{Duration, Instant},
};

use proto::ServerMsg;
use tauri_winrt_notification::Toast;
use windows_sys::Win32::{
    Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT},
    System::Com::CoIncrementMTAUsage,
};
use winreg::HKCU;

use crate::{AUMID, AUMID_KEY, session::Shared};

/// 不分配控制台窗口（debug 构建的 GUI 是控制台子系统程序）
const DETACHED_PROCESS: u32 = 0x0000_0008;
const GUI_EXE: &str = "tether.exe";
/// 内置提示音（scripts/gen-chirp.js 变体 d 生成）；未打包的 Win32 程序不能给 toast 指定自定义音频，
/// 故 toast 静音、由本进程用 winmm 播放
static SOUND: &[u8] = include_bytes!("../assets/notify.wav");
/// 这段时间内连续弹出的多条通知只响一次
const SOUND_DEBOUNCE: Duration = Duration::from_secs(1);
static LAST_SOUND: Mutex<Option<Instant>> = Mutex::new(None);

/// 启动时调用一次：让进程的 MTA 常驻到进程退出。
/// windows-core 把 XmlDocument / ToastNotification 等敏捷激活工厂的裸指针缓存在静态变量里且不持有 DLL；
/// 若首次弹通知时 MTA 恰好由其它线程临时持有，该线程 CoUninitialize 后 MTA 被拆除，COM 卸载 msxml6.dll / wpnapps.dll，
/// 下次弹通知解引用缓存指针即访问违规（2026-11-13 崩溃）。常驻 MTA 后这些 DLL 不再被卸载
pub fn init() {
    let mut cookie = std::ptr::null_mut();
    let hr = unsafe { CoIncrementMTAUsage(&mut cookie) };
    if hr < 0 {
        log!("CoIncrementMTAUsage 失败：0x{hr:08x}");
    }
}

/// 弹出静音 toast，成功且配置开启提示音时播放内置提示音（去抖）；
/// 点击（仍在屏幕上时）回调到本进程：GUI 已连接则让其切换会话，否则拉起 GUI
pub fn toast(state: &Shared, id: u32, title: &str, body: &str) {
    // 未执行 --install 注册 AUMID 时，回退为 PowerShell 的 AUMID（来源会显示为 Windows PowerShell）
    let app_id = if HKCU.open_subkey(AUMID_KEY).is_ok() { AUMID } else { Toast::POWERSHELL_APP_ID };
    let sound = state.lock().unwrap().config.sound;
    let state = state.clone();
    let result = Toast::new(app_id)
        .title(title)
        .text1(body)
        .sound(None)
        .on_activated(move |_| {
            let st = state.lock().unwrap();
            match &st.gui {
                Some(g) => g.send(&ServerMsg::Activate { id }),
                None => launch_gui(id),
            }
            Ok(())
        })
        .show();
    match result {
        Err(e) => log!("显示通知失败：{e}"),
        Ok(()) if sound && debounce(&mut LAST_SOUND.lock().unwrap(), Instant::now()) => play(),
        Ok(()) => {}
    }
}

/// 距上次播放超过 SOUND_DEBOUNCE 才播放，并记下本次时间
fn debounce(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|t| now.duration_since(t) < SOUND_DEBOUNCE) {
        return false;
    }
    *last = Some(now);
    true
}

/// 异步播放内置提示音（SND_MEMORY + SND_ASYNC 要求缓冲区在播放期间有效，SOUND 为 'static）
pub fn play() {
    let ok = unsafe { PlaySoundW(SOUND.as_ptr().cast(), std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT) };
    log!("播放提示音：{}", if ok != 0 { "成功" } else { "失败" });
}

/// GUI 可执行文件取 daemon 同目录（workspace 下 debug/release 产物同在 target/<profile>/）
fn launch_gui(id: u32) {
    let Ok(exe) = std::env::current_exe() else { return };
    let gui = exe.with_file_name(GUI_EXE);
    if !gui.exists() {
        return log!("未找到 GUI：{}", gui.display());
    }
    let spawned = Command::new(&gui).arg("--session").arg(id.to_string()).creation_flags(DETACHED_PROCESS).spawn();
    if let Err(e) = spawned {
        log!("拉起 GUI 失败：{e}");
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::null;

    use tauri_winrt_notification::Toast;
    use windows_sys::Win32::System::{
        Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
        LibraryLoader::GetModuleHandleW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    #[test]
    fn sound_debounce() {
        let mut last = None;
        let t0 = std::time::Instant::now();
        assert!(super::debounce(&mut last, t0));
        assert!(!super::debounce(&mut last, t0 + std::time::Duration::from_millis(500)));
        assert!(super::debounce(&mut last, t0 + std::time::Duration::from_millis(1200)));
    }

    fn loaded(dll: &str) -> bool {
        unsafe { !GetModuleHandleW(wide(dll).as_ptr()).is_null() }
    }

    /// 复现 2026-11-13 崩溃：另一线程临时持有 MTA 时弹首个通知，windows-core 把 XmlDocument / Toast 的激活工厂指针
    /// 缓存进静态变量（此时无需 CoIncrementMTAUsage）；该线程 CoUninitialize 使 MTA 拆除，COM 卸载 msxml6.dll / wpnapps.dll，
    /// 再弹通知即解引用已卸载 DLL 中的工厂（0xc0000005）。空 AUMID 让 Show 失败，不会真的弹出，但工厂已在此之前取得并缓存
    #[test]
    fn toast_survives_mta_teardown() {
        super::init();
        let (enter_tx, enter_rx) = std::sync::mpsc::channel();
        let (leave_tx, leave_rx) = std::sync::mpsc::channel::<()>();
        let mta = std::thread::spawn(move || unsafe {
            CoInitializeEx(null(), COINIT_MULTITHREADED as u32);
            enter_tx.send(()).unwrap();
            leave_rx.recv().unwrap();
            CoUninitialize();
        });
        enter_rx.recv().unwrap();
        let _ = Toast::new("").title("t").show();
        assert!(loaded("msxml6.dll"));
        leave_tx.send(()).unwrap();
        mta.join().unwrap();
        eprintln!("MTA 拆除后 msxml6.dll 已加载：{}", loaded("msxml6.dll"));
        // 修复前：此处访问违规，测试进程崩溃
        let _ = Toast::new("").title("t").show();
    }
}
