//! 进程内存探针：三个口径分列（见 plan §7）。
//!   PrivateUsage        = 私有提交（任务管理器「提交大小」那一列）
//!   PrivateWorkingSet   = 私有工作集（任务管理器「内存」那一列，跨进程不重复计）
//!   WorkingSetSize      = 工作集（含共享页，跨进程会重复计，别对外单独用）

use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows::Win32::System::Threading::GetCurrentProcess;

pub fn probe() -> (f32, f32, f32) {
    unsafe {
        let mut c = PROCESS_MEMORY_COUNTERS_EX2::default();
        let ok = GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
        );
        if ok.is_err() {
            return (0.0, 0.0, 0.0);
        }
        (
            c.PrivateUsage as f32 / 1048576.0,
            c.PrivateWorkingSetSize as f32 / 1048576.0,
            c.WorkingSetSize as f32 / 1048576.0,
        )
    }
}

pub fn log(tag: &str) {
    let (p, pw, w) = probe();
    crate::mlog!("[mem] {tag:<22} private {p:6.2} MB   pws {pw:6.2} MB   ws {w:6.2} MB");
}
