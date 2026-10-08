//! 真数据采样线程：1s 一轮 → Arc<RwLock<DataSnapshot>>
//! CPU/内存/磁盘走 sysinfo；GPU 走 PDH `\GPU Engine(*)\Utilization Percentage`
//! （任务管理器同源；sysinfo 0.39 尚未发布 GPU 支持，故直读 PDH）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use sysinfo::{Disks, Networks, System};
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA,
};

/// windows-sys 未导出该常量；PDH_SUCCESS == 0x00000000（本版 PDH_STATUS = u32）
const PDH_SUCCESS: u32 = 0;

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiskInfo {
    /// 挂载点，如 "C:\"
    pub name: String,
    pub used: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GpuInfo {
    pub name: String,
    /// 0..=100
    pub usage: f32,
}

/// 网络吞吐（字节/秒，全部非回环接口求和）。采样节拍 1s，一次 refresh 的增量正好是 B/s。
#[derive(Debug, Clone, Default, Serialize)]
pub struct NetInfo {
    /// 下行
    pub rx: f64,
    /// 上行
    pub tx: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DataSnapshot {
    /// 0..=100
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub disks: Vec<DiskInfo>,
    pub gpus: Vec<GpuInfo>,
    pub net: NetInfo,
}

pub type SharedSnap = Arc<RwLock<DataSnapshot>>;

pub fn spawn_sampler() -> SharedSnap {
    let snap: SharedSnap = Arc::new(RwLock::new(DataSnapshot::default()));
    let out = Arc::clone(&snap);
    thread::Builder::new()
        .name("mini-card-sampler".into())
        .spawn(move || run(out))
        .expect("spawn sampler");
    snap
}

fn run(out: SharedSnap) {
    let mut sys = System::new();
    sys.refresh_cpu_usage(); // 建立 CPU 基线
    let mut disk_list = Disks::new_with_refreshed_list();
    disk_list.refresh(true);
    let mut nets = Networks::new_with_refreshed_list(); // 建立网络基线（首轮增量为 0）
    let mut pdh = PdhProbe::new();
    let has_gpu_probe = pdh.is_some();

    loop {
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        disk_list.refresh(true); // 参数 = 列表变化时移除消失的盘
        nets.refresh(true);

        let gpu_usage = pdh.as_mut().and_then(|p| p.sample()).unwrap_or(0.0);
        let mut gpus = Vec::new();
        if has_gpu_probe {
            gpus.push(GpuInfo {
                name: "GPU".to_owned(),
                usage: gpu_usage,
            });
        }

        let mut disks = Vec::new();
        for d in disk_list.list() {
            if d.is_removable() {
                continue;
            }
            let total = d.total_space();
            if total == 0 {
                continue;
            }
            disks.push(DiskInfo {
                name: d.mount_point().to_string_lossy().into_owned(),
                used: total.saturating_sub(d.available_space()),
                total,
            });
        }
        // 盘序按盘符稳定排序 + 只留前 DISK_SHOW_MAX 块（用户 2026-10-02：「比 5 盘多的只读前 5 个」）。
        // sysinfo 的枚举顺序不保证跨采样稳定；不排序的话「前 5 块」每次可能换人，
        // 而 host 是按这个列表长度决定窗口高度的 → 盘数来回跳、窗口跟着抖。
        // 卡片页不再自己过滤/截断，一律以这份列表为准（单一数据源）。
        disks.sort_by(|a, b| a.name.cmp(&b.name));
        disks.truncate(crate::config::DISK_SHOW_MAX);

        let net = {
            let mut rx = 0u64;
            let mut tx = 0u64;
            // 同一份累计计数出现两次 = 镜像接口（杀软的 NDIS 过滤驱动会挂一份与母卡逐字节相同的
            // InOctets/OutOctets，如「WLAN-Huorong NDIS Filter Driver-0000」）。不去重整卡速率翻倍。
            let mut seen: HashSet<(u64, u64)> = HashSet::new();
            for (name, d) in nets.iter() {
                // 回环/虚拟调试口不计（Hyper-V 的 vEthernet 也要排除，否则内网流量翻倍）
                let n = name.to_ascii_lowercase();
                if n.contains("loopback") || n.contains("vethernet") || n.contains("bluetooth") ||
                    n.contains("ndis filter")
                {
                    continue;
                }
                if !seen.insert((d.total_received(), d.total_transmitted())) {
                    continue; // 与已计接口同源，跳过
                }
                rx += d.received();
                tx += d.transmitted();
            }
            NetInfo {
                rx: rx as f64,
                tx: tx as f64,
            }
        };

        *out.write().unwrap() = DataSnapshot {
            cpu: sys.global_cpu_usage(),
            mem_used: sys.used_memory(),
            mem_total: sys.total_memory(),
            disks,
            gpus,
            net,
        };

        thread::sleep(Duration::from_millis(1000));
    }
}

/// PDH GPU 探针：聚合规则 = 按 (luid, engtype) 求和 → 每 luid 取各引擎类型最大值
/// → 全局最大 → clamp 0..=100（双显卡并发不会加爆）。
struct PdhProbe {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    buf: Vec<u64>,
}

impl PdhProbe {
    fn new() -> Option<Self> {
        unsafe {
            let mut query: PDH_HQUERY = std::mem::zeroed();
            if PdhOpenQueryW(std::ptr::null(), 0, &mut query) != PDH_SUCCESS {
                return None;
            }
            let path: Vec<u16> = "\\GPU Engine(*)\\Utilization Percentage"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut counter: PDH_HCOUNTER = std::mem::zeroed();
            if PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) != PDH_SUCCESS {
                PdhCloseQuery(query);
                return None;
            }
            PdhCollectQueryData(query); // 速率计数器基线
            Some(Self {
                query,
                counter,
                buf: vec![0u64; 512],
            })
        }
    }

    fn sample(&mut self) -> Option<f32> {
        unsafe {
            if PdhCollectQueryData(self.query) != PDH_SUCCESS {
                return None;
            }
            // 第一次调用（空缓冲）问尺寸
            let mut bytes: u32 = 0;
            let mut count: u32 = 0;
            let st = PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                std::ptr::null_mut(),
            );
            if st != PDH_MORE_DATA {
                return if st == PDH_SUCCESS && bytes == 0 {
                    Some(0.0)
                } else {
                    None
                };
            }
            let words = (bytes as usize).div_ceil(8);
            if self.buf.len() < words {
                self.buf.resize(words + 64, 0);
            }
            let mut bytes2 = bytes;
            let mut count2: u32 = 0;
            let st2 = PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes2,
                &mut count2,
                self.buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W,
            );
            if st2 != PDH_SUCCESS || count2 == 0 {
                return Some(0.0);
            }

            let items = std::slice::from_raw_parts(
                self.buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W,
                count2 as usize,
            );
            // (luid, engtype) → 该引擎类型总和
            let mut pair: HashMap<(String, String), f64> = HashMap::new();
            for it in items {
                let name = wide_to_string(it.szName);
                if name.is_empty() {
                    continue;
                }
                let val = it.FmtValue.Anonymous.doubleValue;
                *pair.entry(parse_instance(&name)).or_insert(0.0) += val;
            }
            // 每 luid 取最大引擎类型
            let mut per_luid: HashMap<String, f64> = HashMap::new();
            for ((luid, _eng), v) in &pair {
                let e = per_luid.entry(luid.clone()).or_insert(0.0);
                if *e < *v {
                    *e = *v;
                }
            }
            let max = per_luid.values().fold(0.0f64, |a, &b| a.max(b));
            Some((max as f32).clamp(0.0, 100.0))
        }
    }
}

impl Drop for PdhProbe {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

/// 实例名形如 `luid_0x15_0x45_pid_0x1234_engtype_3D`（各 Windows 版本段序可能不同，
/// 用 token 扫描稳妥）；luid 缺失时归入 "X" 组，最终结果有 clamp 兜底。
fn parse_instance(name: &str) -> (String, String) {
    let toks: Vec<&str> = name.split('_').collect();
    let luid = toks
        .iter()
        .position(|t| *t == "luid")
        .and_then(|i| toks.get(i + 1..i + 3))
        .map(|p| p.join("_"))
        .unwrap_or_else(|| "X".to_owned());
    let eng = toks
        .iter()
        .position(|t| *t == "engtype")
        .and_then(|i| toks.get(i + 1..).map(|r| r.join("_")))
        .unwrap_or_else(|| name.to_owned());
    (luid, eng)
}

unsafe fn wide_to_string(p: *mut u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < 512 && *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
}

#[cfg(test)]
mod net_source_probe {
    use super::*;
    use std::process::Command;
    use std::time::Instant;

    /// 对照「sysinfo 汇总的网卡速率」与「Windows 计数器（Get-NetAdapterStatistics）」。
    /// 排查 2026-10-01 用户报的「网络卡数据不准」：卡片读数约为真实值的 2 倍。
    /// 用法：cargo test net_source_probe -- --ignored --nocapture
    #[test]
    #[ignore]
    fn net_source_probe() {
        let url = "https://cdn.npmmirror.com/binaries/electron/33.2.0/electron-v33.2.0-win32-x64.zip";
        let out = std::env::temp_dir().join("mc_netprobe.bin");
        let mut child = Command::new("curl")
            .args(["-sS", "--noproxy", "*", "-o"])
            .arg(&out)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("curl spawn");

        let ps = |expr: &str| -> u64 {
            let o = Command::new("powershell")
                .args(["-NoProfile", "-Command", expr])
                .output()
                .expect("ps");
            let s = String::from_utf8_lossy(&o.stdout);
            s.chars().filter(|c| c.is_ascii_digit()).collect::<String>()
                .parse::<u64>()
                .unwrap_or(0)
        };

        let mut nets = Networks::new_with_refreshed_list();
        std::thread::sleep(std::time::Duration::from_secs(4)); // 等下载进入稳态
        let c0 = ps("(Get-NetAdapterStatistics -Name WLAN).ReceivedBytes");
        let t0 = Instant::now();
        nets.refresh(true); // 建基线
        std::thread::sleep(std::time::Duration::from_secs(6));
        nets.refresh(true);
        let el = t0.elapsed().as_secs_f64();
        let c1 = ps("(Get-NetAdapterStatistics -Name WLAN).ReceivedBytes");
        let _ = child.kill();

        let mut sum = 0u64;
        for (name, d) in nets.iter() {
            let rx = d.received();
            let tx = d.transmitted();
            sum += rx;
            println!("sysinfo {:38} rx={:9.2} MB/s  tx={:9.3} MB/s  mac={:?} total_rx={} total_tx={}",
                     name, rx as f64 / el / 1048576.0, tx as f64 / el / 1048576.0,
                     d.mac_address(), d.total_received(), d.total_transmitted());
        }
        println!("sysinfo 汇总                     rx={:9.2} MB/s   (窗口 {:.2}s)", sum as f64 / el / 1048576.0, el);
        println!("Windows WLAN 计数器              rx={:9.2} MB/s", (c1 - c0) as f64 / el / 1048576.0);
        let _ = std::fs::remove_file(&out);
    }
}
