//! 天气：**外壳适配层**（实现在 `minicard_core::weather`，自绘卡进程共用同一份）。
//!
//! 这里只保留 Tauri 特有的两件事：`emit("weather")` 广播、30 分钟轮询线程。

pub use minicard_core::weather::*;

use std::thread;
use tauri::{AppHandle, Emitter};

/// 拉一轮并广播；失败时保留旧值（卡片自己按 35 分钟判「数据源异常」）
pub fn refresh(shared: &SharedWeather, app: &AppHandle) -> Result<WeatherSnap, String> {
    let s = refresh_local(shared)?;
    let _ = app.emit("weather", &s);
    Ok(s)
}

/// 开机：先贴上次落盘的值（卡片立刻有数），再起 30 分钟轮询线程
pub fn spawn(shared: SharedWeather, app: AppHandle) {
    seed(&shared);
    thread::spawn(move || loop {
        let _ = refresh(&shared, &app);
        thread::sleep(PERIOD);
    });
}
