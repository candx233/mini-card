//! 采样层：CPU（GetSystemTimes 差分）/ 内存（GlobalMemoryStatusEx）/ GPU（PDH，聚合规则与主程序一致）。
//! 这一层对应 `src-tauri/src/data.rs` 的可搬运部分 —— spike 里不引 sysinfo，只有一个 60 行的探针。

use std::collections::HashMap;
use windows::core::PCWSTR;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

const PDH_SUCCESS: u32 = 0;

#[derive(Default, Clone, Debug)]
pub struct Snapshot {
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub gpu: Option<f32>,
}

pub struct Sampler {
    prev: Option<(u64, u64, u64)>, // idle, kernel, user
    pdh: Option<PdhProbe>,
}

impl Sampler {
    pub fn new() -> Self {
        Self { prev: None, pdh: PdhProbe::new() }
    }

    pub fn sample(&mut self) -> Snapshot {
        let mut s = Snapshot::default();
        unsafe {
            let (mut idle, mut kern, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
            if GetSystemTimes(Some(&mut idle), Some(&mut kern), Some(&mut user)).is_ok() {
                let (i, k, u) = (ft(idle), ft(kern), ft(user));
                if let Some((pi, pk, pu)) = self.prev {
                    let di = i.saturating_sub(pi);
                    let dt = (k.saturating_sub(pk)) + (u.saturating_sub(pu)); // kernel 已含 idle
                    if dt > 0 {
                        s.cpu = (100.0 * (1.0 - di as f64 / dt as f64)) as f32;
                    }
                }
                self.prev = Some((i, k, u));
            }

            let mut ms = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
            if GlobalMemoryStatusEx(&mut ms).is_ok() {
                s.mem_total = ms.ullTotalPhys;
                s.mem_used = ms.ullTotalPhys.saturating_sub(ms.ullAvailPhys);
            }
        }
        s.gpu = self.pdh.as_mut().and_then(|p| p.sample());
        s.cpu = s.cpu.clamp(0.0, 100.0);
        s
    }
}

fn ft(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

// ── GPU 探针（与 src-tauri/src/data.rs 同源，聚合口径不变）────────────────────────────
struct PdhProbe {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    buf: Vec<u64>,
}

impl PdhProbe {
    fn new() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != PDH_SUCCESS {
                return None;
            }
            let path: Vec<u16> = "\\GPU Engine(*)\\Utilization Percentage"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut counter = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &mut counter) != PDH_SUCCESS {
                let _ = PdhCloseQuery(query);
                return None;
            }
            let _ = PdhCollectQueryData(query);
            Some(Self { query, counter, buf: vec![0u64; 512] })
        }
    }

    fn sample(&mut self) -> Option<f32> {
        unsafe {
            if PdhCollectQueryData(self.query) != PDH_SUCCESS {
                return None;
            }
            let (mut bytes, mut count) = (0u32, 0u32);
            let st = PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, None);
            if st == PDH_SUCCESS && bytes == 0 {
                return Some(0.0);
            }
            if st != PDH_MORE_DATA {
                return None;
            }
            let words = (bytes as usize).div_ceil(8);
            if self.buf.len() < words {
                self.buf.resize(words + 64, 0);
            }
            let mut bytes2 = bytes;
            let mut count2 = 0u32;
            let st2 = PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes2,
                &mut count2,
                Some(self.buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
            );
            if st2 != PDH_SUCCESS || count2 == 0 {
                return Some(0.0);
            }
            let items = std::slice::from_raw_parts(
                self.buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W,
                count2 as usize,
            );
            let mut pair: HashMap<(String, String), f64> = HashMap::new();
            for it in items {
                let name = wide(it.szName.0);
                if name.is_empty() {
                    continue;
                }
                let val = it.FmtValue.Anonymous.doubleValue;
                *pair.entry(parse_instance(&name)).or_insert(0.0) += val;
            }
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
            let _ = PdhCloseQuery(self.query);
        }
    }
}

/// 实例名形如 `luid_0x15_0x45_pid_0x1234_engtype_3D`（段序各版本可能不同，用 token 扫描）
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

unsafe fn wide(p: *mut u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < 512 && *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
}
