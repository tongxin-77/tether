//! 主机信息卡片：采样会话所在机器的 CPU / 内存 / GPU / 网络。
//!
//! 本地走 Win32 API（GetSystemTimes、GlobalMemoryStatusEx、GetIfTable2、DXGI、PDH），不起子进程；
//! 远程在会话所属的已有 SSH 连接上开一个 exec channel 跑一小段 sh 读 /proc，与交互 shell 的 channel 互不影响。
//! CPU 占用与网速由相邻两次采样的差值计算，上次采样按目标（`local` / 远程连接的 label）保存。

use std::{
    collections::{BTreeMap, HashMap},
    ffi::c_void,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use proto::{GpuStats, HostStats, ServerMsg};
use russh::ChannelMsg;

use crate::{fs, session::Shared, ssh::Conn};

/// 与上次采样间隔超过这段时间则不计算差值（卡片停止轮询后再打开，首个数据显示为“…”）
const STALE: Duration = Duration::from_secs(15);
/// 远程采样的最长等待；GUI 的等待更长，超时未回复才判定为旧 daemon
const REMOTE_TIMEOUT: Duration = Duration::from_secs(8);
const MIB: u64 = 1024 * 1024;

/// 远程采样脚本：单行、不含单引号，经 `sh -c '<脚本>'` 运行，兼容 fish / csh 等非 POSIX 登录 shell。
/// 各段以 `@名称` 行分隔；@phys 为有 device 的（物理）网卡，用于排除 docker / veth 等虚拟网卡的重复计数
const SCRIPT: &str = "export LC_ALL=C; echo @stat; head -n1 /proc/stat 2>/dev/null; echo @mem; cat /proc/meminfo 2>/dev/null; \
echo @net; cat /proc/net/dev 2>/dev/null; echo @phys; for i in /sys/class/net/*; do [ -e $i/device ] && echo ${i##*/}; done; \
echo @cpu; grep -m1 \"model name\" /proc/cpuinfo 2>/dev/null; echo @nproc; nproc 2>/dev/null || grep -c ^processor /proc/cpuinfo; \
echo @gpu; nvidia-smi --query-gpu=name,utilization.gpu,memory.used,memory.total --format=csv,noheader,nounits 2>/dev/null; true";

/// 一次采样的累计计数器
#[derive(Clone, Debug)]
pub struct Counters {
    pub at: Instant,
    /// CPU (忙碌, 总计) 时间，单位不限（本地 100ns，远程 jiffies）
    pub cpu: (u64, u64),
    /// 网卡累计 (接收, 发送) 字节；取不到为 None
    pub net: Option<(u64, u64)>,
}

static PREV: Mutex<BTreeMap<String, Counters>> = Mutex::new(BTreeMap::new());

/// 处理 ClientMsg::HostStats（异步任务）：按会话的文件后端区分本地 / 远程，结果以 ServerMsg::HostStats 回复
pub async fn run(state: Shared, req_id: u64, backend: fs::Backend) {
    let result = match backend {
        fs::Backend::Local => tokio::task::spawn_blocking(local).await.map_err(|e| e.to_string()).and_then(|r| r),
        fs::Backend::Remote { conn, label } => match conn.upgrade() {
            Some(conn) => remote(&conn, &label).await,
            None => Err(tr!("common.ssh_disconnected")),
        },
    };
    let (stats, error) = match result {
        Ok(s) => (Some(s), None),
        Err(e) => (None, Some(e)),
    };
    state.lock().unwrap().send(&ServerMsg::HostStats { req_id, stats, error });
}

/// 以本次计数器与该目标的上次采样计算 CPU 占用与网速，并记下本次采样
fn apply_deltas(key: &str, cur: Counters, stats: &mut HostStats) {
    let prev = PREV.lock().unwrap().insert(key.into(), cur.clone());
    if let Some(prev) = prev {
        let (cpu, net) = deltas(&prev, &cur);
        stats.cpu_usage = cpu;
        stats.net_rx = net.map(|n| n.0);
        stats.net_tx = net.map(|n| n.1);
    }
}

/// 两次采样的差值：CPU 占用（百分比）与网速（字节/秒）；间隔异常或计数器回退（重启、网卡移除）时为 None
pub fn deltas(prev: &Counters, cur: &Counters) -> (Option<f32>, Option<(u64, u64)>) {
    let secs = cur.at.saturating_duration_since(prev.at);
    if secs.is_zero() || secs > STALE {
        return (None, None);
    }
    let cpu = (|| {
        let total = cur.cpu.1.checked_sub(prev.cpu.1).filter(|&t| t > 0)?;
        let busy = cur.cpu.0.checked_sub(prev.cpu.0)?;
        Some((busy as f64 * 100.0 / total as f64).min(100.0) as f32)
    })();
    let secs = secs.as_secs_f64();
    let net = (|| {
        let (p, c) = (prev.net?, cur.net?);
        let rate = |a: u64, b: u64| b.checked_sub(a).map(|d| (d as f64 / secs).round() as u64);
        Some((rate(p.0, c.0)?, rate(p.1, c.1)?))
    })();
    (cpu, net)
}

// ---------------- 远程（Linux /proc） ----------------

async fn remote(conn: &Conn, key: &str) -> Result<HostStats, String> {
    let out = exec(conn, &format!("sh -c '{SCRIPT}'")).await?;
    let (mut stats, counters) = parse_remote(&out, Instant::now())?;
    apply_deltas(key, counters, &mut stats);
    Ok(stats)
}

/// 在已有连接上开 exec channel 运行命令，收集 stdout 直到 channel 关闭；超时也会关闭 channel
async fn exec(conn: &Conn, cmd: &str) -> Result<String, String> {
    let mut ch = conn.handle.channel_open_session().await.map_err(|e| tr!("common.open_channel_failed", error = e))?;
    ch.exec(true, cmd).await.map_err(|e| tr!("stats.exec_failed", error = e))?;
    let read = async {
        let mut out = Vec::new();
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { data } => out.extend_from_slice(&data),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        out
    };
    let result = tokio::time::timeout(REMOTE_TIMEOUT, read).await;
    let _ = ch.close().await;
    let out = result.map_err(|_| tr!("stats.timeout"))?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// 解析远程脚本输出；没有 /proc/stat（非 Linux）时报不支持
pub fn parse_remote(out: &str, at: Instant) -> Result<(HostStats, Counters), String> {
    let mut sections: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut cur = "";
    for line in out.lines() {
        match line.strip_prefix('@') {
            Some(name) => cur = name.trim(),
            None => sections.entry(cur).or_default().push(line),
        }
    }
    let get = |k: &str| sections.get(k).map(Vec::as_slice).unwrap_or(&[]);
    let cpu = parse_stat(get("stat")).ok_or_else(|| tr!("stats.not_linux"))?;
    let (mem_used, mem_total) = parse_meminfo(get("mem")).unwrap_or_default();
    let net = parse_net_dev(get("net"), get("phys"));
    let stats = HostStats {
        cpu_model: get("cpu").first().and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string()).unwrap_or_default(),
        cores: get("nproc").first().and_then(|l| l.trim().parse().ok()).unwrap_or(0),
        mem_used,
        mem_total,
        gpus: parse_nvidia(get("gpu")),
        ..Default::default()
    };
    Ok((stats, Counters { at, cpu, net: Some(net) }))
}

/// /proc/stat 首行 `cpu user nice system idle iowait irq softirq steal guest guest_nice` → (忙碌, 总计)；
/// guest 已计入 user，只取前 8 项；空闲 = idle + iowait
fn parse_stat(lines: &[&str]) -> Option<(u64, u64)> {
    let line = lines.iter().find(|l| l.starts_with("cpu "))?;
    let nums: Vec<u64> = line.split_whitespace().skip(1).map_while(|s| s.parse().ok()).collect();
    if nums.len() < 4 {
        return None;
    }
    let total: u64 = nums.iter().take(8).sum();
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    Some((total.saturating_sub(idle), total))
}

/// /proc/meminfo → (已用, 总计) 字节；已用 = MemTotal − MemAvailable（老内核没有 MemAvailable 时用 Free + Buffers + Cached）
fn parse_meminfo(lines: &[&str]) -> Option<(u64, u64)> {
    let kb: HashMap<&str, u64> = lines
        .iter()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim(), v.split_whitespace().next()?.parse().ok()?))
        })
        .collect();
    let total = *kb.get("MemTotal")?;
    let avail = kb.get("MemAvailable").copied().unwrap_or_else(|| {
        ["MemFree", "Buffers", "Cached"].iter().filter_map(|k| kb.get(k)).sum()
    });
    Some((total.saturating_sub(avail) * 1024, total * 1024))
}

/// /proc/net/dev → 累计 (接收, 发送) 字节：排除 lo；有物理网卡列表时只计其中的网卡
fn parse_net_dev(lines: &[&str], phys: &[&str]) -> (u64, u64) {
    let phys: Vec<&str> = phys.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
    let mut sum = (0, 0);
    for line in lines {
        let Some((name, rest)) = line.split_once(':') else { continue };
        let name = name.trim();
        if name == "lo" || (!phys.is_empty() && !phys.contains(&name)) {
            continue;
        }
        let f: Vec<u64> = rest.split_whitespace().map_while(|s| s.parse().ok()).collect();
        if f.len() >= 9 {
            sum.0 += f[0];
            sum.1 += f[8];
        }
    }
    sum
}

/// `nvidia-smi --query-gpu=name,utilization.gpu,memory.used,memory.total --format=csv,noheader,nounits`，
/// 显存单位 MiB；`[N/A]` 等非数字为 None
fn parse_nvidia(lines: &[&str]) -> Vec<GpuStats> {
    let num = |s: &str| s.trim().parse::<f64>().ok();
    lines
        .iter()
        .filter_map(|l| {
            let f: Vec<&str> = l.rsplitn(4, ',').collect();
            let [total, used, util, name] = f[..] else { return None };
            Some(GpuStats {
                name: name.trim().to_string(),
                usage: num(util).map(|v| v as f32),
                mem_used: num(used).map(|v| (v * MIB as f64) as u64),
                mem_total: num(total).map(|v| (v * MIB as f64) as u64),
            })
        })
        .collect()
}

// ---------------- 本地（Windows） ----------------

/// 不变的本机信息，首次采样时读取
struct LocalInfo {
    cpu_model: String,
    cores: u32,
    adapters: Vec<Adapter>,
}

struct Adapter {
    name: String,
    /// `0x<高 32 位>_0x<低 32 位>`（小写），与 PDH GPU 计数器实例名中的 luid 对应
    luid: String,
    vram: u64,
}

fn local() -> Result<HostStats, String> {
    static INFO: OnceLock<LocalInfo> = OnceLock::new();
    let info = INFO.get_or_init(|| LocalInfo { cpu_model: cpu_model(), cores: cores(), adapters: adapters() });
    let cpu = cpu_times().ok_or_else(|| tr!("stats.cpu_failed"))?;
    let (mem_used, mem_total) = memory().ok_or_else(|| tr!("stats.mem_failed"))?;
    let mut stats = HostStats {
        cpu_model: info.cpu_model.clone(),
        cores: info.cores,
        mem_used,
        mem_total,
        gpus: gpu_stats(&info.adapters),
        ..Default::default()
    };
    apply_deltas("local", Counters { at: Instant::now(), cpu, net: net_bytes() }, &mut stats);
    Ok(stats)
}

fn cpu_model() -> String {
    winreg::HKLM
        .open_subkey(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0")
        .and_then(|k| k.get_value::<String, _>("ProcessorNameString"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn cores() -> u32 {
    use windows_sys::Win32::System::Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount};
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }
}

/// GetSystemTimes → (忙碌, 总计)，单位 100ns；内核时间含空闲时间
fn cpu_times() -> Option<(u64, u64)> {
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetSystemTimes};
    let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    let (mut idle, mut kernel, mut user) = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return None;
    }
    let total = ft(kernel) + ft(user);
    Some((total.saturating_sub(ft(idle)), total))
}

fn memory() -> Option<(u64, u64)> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 {
        return None;
    }
    Some((m.ullTotalPhys.saturating_sub(m.ullAvailPhys), m.ullTotalPhys))
}

/// 硬件网卡的累计收发字节：只计 HardwareInterface 且非 FilterInterface 的行（排除回环、过滤驱动、
/// Hyper-V / WSL 虚拟交换机等，避免同一流量重复计数）
fn net_bytes() -> Option<(u64, u64)> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
    const HARDWARE: u8 = 1;
    const FILTER: u8 = 2;
    const LOOPBACK: u32 = 24;
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    if unsafe { GetIfTable2(&mut table) } != 0 || table.is_null() {
        return None;
    }
    let mut sum = (0u64, 0u64);
    unsafe {
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        for r in rows {
            let flags = r.InterfaceAndOperStatusFlags._bitfield;
            if flags & HARDWARE == 0 || flags & FILTER != 0 || r.Type == LOOPBACK {
                continue;
            }
            sum.0 += r.InOctets;
            sum.1 += r.OutOctets;
        }
        FreeMibTable(table as *const c_void);
    }
    Some(sum)
}

// ---- 显卡：DXGI 枚举名称 / 显存 / LUID（windows-sys 不含 COM 接口，按 vtable 下标手动调用） ----

#[link(name = "dxgi")]
unsafe extern "system" {
    fn CreateDXGIFactory1(riid: *const windows_sys::core::GUID, factory: *mut *mut c_void) -> i32;
}

const IID_IDXGI_FACTORY1: windows_sys::core::GUID = windows_sys::core::GUID {
    data1: 0x770a_ae78,
    data2: 0xf26f,
    data3: 0x4dba,
    data4: [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87],
};
/// vtable 下标：IUnknown(3) + IDXGIObject(4) + IDXGIFactory(5) 之后为 EnumAdapters1；
/// IUnknown(3) + IDXGIObject(4) + IDXGIAdapter(3) 之后为 GetDesc1
const RELEASE: usize = 2;
const ENUM_ADAPTERS1: usize = 12;
const GET_DESC1: usize = 10;
const DXGI_ADAPTER_FLAG_SOFTWARE: u32 = 2;

/// DXGI_ADAPTER_DESC1
#[repr(C)]
struct AdapterDesc1 {
    description: [u16; 128],
    vendor_id: u32,
    device_id: u32,
    sub_sys_id: u32,
    revision: u32,
    dedicated_video_memory: usize,
    dedicated_system_memory: usize,
    shared_system_memory: usize,
    luid_low: u32,
    luid_high: i32,
    flags: u32,
}

/// 取 COM 对象 vtable 中第 index 个函数指针
unsafe fn method(obj: *mut c_void, index: usize) -> usize {
    unsafe { *(*(obj as *const *const usize)).add(index) }
}

/// 枚举硬件显卡（跳过 Microsoft Basic Render Driver 等软件适配器）
fn adapters() -> Vec<Adapter> {
    type Release = unsafe extern "system" fn(*mut c_void) -> u32;
    type EnumAdapters1 = unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
    type GetDesc1 = unsafe extern "system" fn(*mut c_void, *mut AdapterDesc1) -> i32;
    let mut out = Vec::new();
    unsafe {
        let mut factory = std::ptr::null_mut();
        if CreateDXGIFactory1(&IID_IDXGI_FACTORY1, &mut factory) < 0 || factory.is_null() {
            return out;
        }
        let enum_adapters: EnumAdapters1 = std::mem::transmute(method(factory, ENUM_ADAPTERS1));
        for i in 0.. {
            let mut adapter = std::ptr::null_mut();
            // 枚举完返回 DXGI_ERROR_NOT_FOUND
            if enum_adapters(factory, i, &mut adapter) < 0 || adapter.is_null() {
                break;
            }
            let get_desc: GetDesc1 = std::mem::transmute(method(adapter, GET_DESC1));
            let mut d: AdapterDesc1 = std::mem::zeroed();
            if get_desc(adapter, &mut d) >= 0 && d.flags & DXGI_ADAPTER_FLAG_SOFTWARE == 0 {
                let len = d.description.iter().position(|&c| c == 0).unwrap_or(d.description.len());
                out.push(Adapter {
                    name: String::from_utf16_lossy(&d.description[..len]).trim().to_string(),
                    luid: format!("0x{:08x}_0x{:08x}", d.luid_high as u32, d.luid_low),
                    vram: d.dedicated_video_memory as u64,
                });
            }
            std::mem::transmute::<usize, Release>(method(adapter, RELEASE))(adapter);
        }
        std::mem::transmute::<usize, Release>(method(factory, RELEASE))(factory);
    }
    out
}

// ---- 显卡占用 / 显存用量：PDH 的 GPU Engine / GPU Adapter Memory 计数器（按实例名中的 luid 归到各显卡） ----

struct Pdh {
    query: *mut c_void,
    util: *mut c_void,
    mem: *mut c_void,
}

// PDH 句柄只在持锁时使用
unsafe impl Send for Pdh {}

/// 打开常驻的 PDH 查询；英文计数器名不受系统语言影响。系统不支持（Windows 10 1709 之前）时为 None
fn pdh_open() -> Option<Pdh> {
    use windows_sys::Win32::System::Performance::{PdhAddEnglishCounterW, PdhCloseQuery, PdhOpenQueryW};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    unsafe {
        let mut query = std::ptr::null_mut();
        if PdhOpenQueryW(std::ptr::null(), 0, &mut query) != 0 {
            return None;
        }
        let (mut util, mut mem) = (std::ptr::null_mut(), std::ptr::null_mut());
        let ok = PdhAddEnglishCounterW(query, wide(r"\GPU Engine(*engtype_3D)\Utilization Percentage").as_ptr(), 0, &mut util) == 0
            && PdhAddEnglishCounterW(query, wide(r"\GPU Adapter Memory(*)\Dedicated Usage").as_ptr(), 0, &mut mem) == 0;
        if !ok {
            PdhCloseQuery(query);
            return None;
        }
        Some(Pdh { query, util, mem })
    }
}

/// 读通配计数器的全部实例 (实例名, 值)；数据无效（如占用率首次采样）时为空
unsafe fn pdh_array(counter: *mut c_void) -> Vec<(String, f64)> {
    use windows_sys::Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_MORE_DATA,
        PdhGetFormattedCounterArrayW,
    };
    let (mut size, mut count) = (0u32, 0u32);
    unsafe {
        if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, std::ptr::null_mut()) != PDH_MORE_DATA {
            return Vec::new();
        }
        // 按 8 字节对齐分配
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, items) != 0 {
            return Vec::new();
        }
        (0..count as usize)
            .filter_map(|i| {
                let it = &*items.add(i);
                if it.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA && it.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA {
                    return None;
                }
                let len = (0..).take_while(|&j| *it.szName.add(j) != 0).count();
                let name = String::from_utf16_lossy(std::slice::from_raw_parts(it.szName, len));
                Some((name, it.FmtValue.Anonymous.doubleValue))
            })
            .collect()
    }
}

/// PDH 实例名中的 luid，如 `pid_1234_luid_0x00000000_0x0000D1A5_phys_0_eng_0_engtype_3D` → `0x00000000_0x0000d1a5`
fn luid_of(instance: &str) -> Option<String> {
    let i = instance.find("luid_")? + 5;
    instance.get(i..i + 21).map(str::to_ascii_lowercase)
}

fn gpu_stats(adapters: &[Adapter]) -> Vec<GpuStats> {
    use windows_sys::Win32::System::Performance::PdhCollectQueryData;
    static PDH: OnceLock<Option<Mutex<Pdh>>> = OnceLock::new();
    let (util, mem) = match PDH.get_or_init(|| pdh_open().map(Mutex::new)) {
        Some(p) => {
            let p = p.lock().unwrap();
            unsafe {
                PdhCollectQueryData(p.query);
                (pdh_array(p.util), pdh_array(p.mem))
            }
        }
        None => (Vec::new(), Vec::new()),
    };
    let sum = |items: &[(String, f64)], luid: &str| -> Option<f64> {
        let mut hit = items.iter().filter(|(n, _)| luid_of(n).as_deref() == Some(luid)).peekable();
        hit.peek()?;
        Some(hit.map(|x| x.1).sum())
    };
    adapters
        .iter()
        // 虚拟显示器（远程桌面 / 串流软件的 IddCx 驱动）在 DXGI 中以渲染显卡的名称出现，但没有显存计数器实例
        .filter(|a| mem.is_empty() || sum(&mem, &a.luid).is_some())
        .map(|a| GpuStats {
            name: a.name.clone(),
            // 占用率有数据时，没有 3D 实例的显卡视为空闲
            usage: (!util.is_empty()).then(|| sum(&util, &a.luid).unwrap_or(0.0).min(100.0) as f32),
            mem_used: sum(&mem, &a.luid).map(|v| v as u64),
            mem_total: Some(a.vram),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "@stat
cpu  4705 150 1120 16250 520 30 45 0 0 0
@mem
MemTotal:       16303428 kB
MemFree:         8000000 kB
MemAvailable:   12000000 kB
Buffers:          200000 kB
Cached:          3000000 kB
@net
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 9999999    1000    0    0    0     0          0         0  9999999    1000    0    0    0     0       0          0
  eth0:1000000    2000    0    0    0     0          0         0   500000    1500    0    0    0     0       0          0
docker0: 300000     100    0    0    0     0          0         0   300000     100    0    0    0     0       0          0
@phys
eth0
@cpu
model name\t: Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz
@nproc
8
@gpu
NVIDIA GeForce RTX 3060, 12, 1024, 12288
Tesla T4, [N/A], 0, 15360
";

    #[test]
    fn parse_full_sample() {
        let at = Instant::now();
        let (s, c) = parse_remote(SAMPLE, at).unwrap();
        assert_eq!(s.cpu_model, "Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz");
        assert_eq!(s.cores, 8);
        assert_eq!(s.mem_total, 16303428 * 1024);
        assert_eq!(s.mem_used, (16303428 - 12000000) * 1024);
        // 总计为前 8 项之和，空闲 = idle + iowait
        let total = 4705 + 150 + 1120 + 16250 + 520 + 30 + 45;
        assert_eq!(c.cpu, (total - 16250 - 520, total));
        // 只计物理网卡 eth0（lo 与 docker0 排除）
        assert_eq!(c.net, Some((1000000, 500000)));
        assert_eq!(s.gpus.len(), 2);
        assert_eq!(s.gpus[0].name, "NVIDIA GeForce RTX 3060");
        assert_eq!(s.gpus[0].usage, Some(12.0));
        assert_eq!(s.gpus[0].mem_used, Some(1024 * MIB));
        assert_eq!(s.gpus[0].mem_total, Some(12288 * MIB));
        assert_eq!(s.gpus[1].usage, None);
        assert!(s.cpu_usage.is_none() && s.net_rx.is_none());
    }

    #[test]
    fn parse_without_optional_parts() {
        // 无 nvidia-smi、无 model name、无物理网卡信息、老内核无 MemAvailable
        let out = "@stat\ncpu 10 0 10 80\n@mem\nMemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 100 kB\nCached: 300 kB\n\
@net\n  lo: 5 0 0 0 0 0 0 0 5 0 0 0 0 0 0 0\n  ens3: 7 0 0 0 0 0 0 0 9 0 0 0 0 0 0 0\n  wg0: 1 0 0 0 0 0 0 0 2 0 0 0 0 0 0 0\n\
@phys\n@cpu\n@nproc\n2\n@gpu\n";
        let (s, c) = parse_remote(out, Instant::now()).unwrap();
        assert_eq!(s.cpu_model, "");
        assert_eq!(s.cores, 2);
        assert_eq!((s.mem_used, s.mem_total), (500 * 1024, 1000 * 1024));
        assert_eq!(c.cpu, (20, 100));
        assert_eq!(c.net, Some((8, 11)));
        assert!(s.gpus.is_empty());
    }

    #[test]
    fn non_linux_unsupported() {
        // macOS / BSD：/proc 不存在，各段为空
        let out = "@stat\n@mem\n@net\n@phys\n@cpu\n@nproc\n10\n@gpu\n";
        assert!(parse_remote(out, Instant::now()).unwrap_err().contains("不支持"));
        assert!(parse_remote("", Instant::now()).is_err());
    }

    #[test]
    fn delta_computation() {
        let t0 = Instant::now();
        let a = Counters { at: t0, cpu: (100, 1000), net: Some((1_000, 2_000)) };
        let b = Counters { at: t0 + Duration::from_secs(2), cpu: (350, 2000), net: Some((5_000, 2_400)) };
        assert_eq!(deltas(&a, &b), (Some(25.0), Some((2_000, 200))));
        // 计数器回退（重启 / 网卡移除）
        let c = Counters { at: t0 + Duration::from_secs(4), cpu: (10, 100), net: Some((0, 0)) };
        assert_eq!(deltas(&b, &c), (None, None));
        // 间隔过久或为零不计算
        let late = Counters { at: t0 + STALE + Duration::from_secs(1), ..b.clone() };
        assert_eq!(deltas(&a, &late), (None, None));
        assert_eq!(deltas(&a, &a), (None, None));
        // 网卡计数不可用时只算 CPU
        let n = Counters { net: None, ..b.clone() };
        assert_eq!(deltas(&a, &n), (Some(25.0), None));
    }

    #[test]
    fn apply_deltas_keeps_previous_per_target() {
        let t0 = Instant::now();
        let mut s = HostStats::default();
        apply_deltas("test-a", Counters { at: t0, cpu: (0, 0), net: Some((0, 0)) }, &mut s);
        assert_eq!(s.cpu_usage, None);
        apply_deltas("test-b", Counters { at: t0, cpu: (0, 0), net: Some((0, 0)) }, &mut s);
        apply_deltas("test-a", Counters { at: t0 + Duration::from_secs(1), cpu: (50, 100), net: Some((10, 20)) }, &mut s);
        assert_eq!((s.cpu_usage, s.net_rx, s.net_tx), (Some(50.0), Some(10), Some(20)));
    }

    #[test]
    fn pdh_instance_luid() {
        let n = "pid_1234_luid_0x00000000_0x0000D1A5_phys_0_eng_0_engtype_3D";
        assert_eq!(luid_of(n).as_deref(), Some("0x00000000_0x0000d1a5"));
        assert_eq!(luid_of("luid_0x00000000_0x0000D1A5_phys_0").as_deref(), Some("0x00000000_0x0000d1a5"));
        assert_eq!(luid_of("_Total"), None);
    }

    #[test]
    fn script_has_no_single_quote() {
        // 经 sh -c '<脚本>' 运行，脚本内不能有单引号
        assert!(!SCRIPT.contains('\''));
    }

    /// 打印一次本机采样（手动检查用）：cargo test -p tetherd local_sample -- --ignored --nocapture
    #[test]
    #[ignore]
    fn local_sample() {
        println!("{:#?}", local());
        std::thread::sleep(Duration::from_secs(1));
        println!("{:#?}", local());
    }
}
