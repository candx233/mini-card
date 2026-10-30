//! 条宽补间：与 `web/card.html` 的 `barEase/barTween/barMs` 同一条曲线、同一套口径。
//!
//! 关键约定（照搬浏览器版，别自己发明）：
//!   · 目标值一变就从**当前显示值**起步（位置连续，不回到 0）；
//!   · 目标值没变就别重启时间轴（否则速度断档）；
//!   · 文字立刻显示目标值，只有条宽是补间的；
//!   · 梯形速度剖面：起停各占 12% 时长，中段匀速（f(0)=0 / f(1)=1 / 两端 v=0）。

use std::time::Instant;

/// card.html 的 BAR_R
pub const BAR_R: f32 = 0.12;
/// card.html 的 barMs()：1 s 推送时 clamp(1000×0.95, 300..1500) = 950
pub const DEFAULT_MS: f32 = 950.0;

pub fn ease(u: f32) -> f32 {
    if u <= 0.0 {
        return 0.0;
    }
    if u >= 1.0 {
        return 1.0;
    }
    let a = 1.0 / (1.0 - BAR_R);
    if u < BAR_R {
        a * u * u / (2.0 * BAR_R)
    } else if u <= 1.0 - BAR_R {
        a * (BAR_R / 2.0 + u - BAR_R)
    } else {
        1.0 - a * (1.0 - u) * (1.0 - u) / (2.0 * BAR_R)
    }
}

#[derive(Clone, Copy)]
pub struct Tween {
    from: f32,
    pub to: f32,
    t0: Instant,
    ms: f32,
}

impl Tween {
    pub fn new(from: f32, to: f32, t0: Instant) -> Self {
        Self { from, to, t0, ms: DEFAULT_MS }
    }

    pub fn u(&self, now: Instant) -> f32 {
        if self.ms <= 0.0 {
            return 1.0;
        }
        (now.duration_since(self.t0).as_secs_f32() * 1000.0 / self.ms).clamp(0.0, 1.0)
    }

    pub fn value(&self, now: Instant) -> f32 {
        self.from + (self.to - self.from) * ease(self.u(now))
    }

    /// 还在动（还有行程没走完）
    pub fn live(&self, now: Instant) -> bool {
        (self.to - self.from).abs() > 0.001 && self.u(now) < 1.0
    }
}
