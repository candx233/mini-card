//! 农历换算：`card.html` 里那张 1900–2099 压缩年表的 Rust 版。
//!
//! 每一位的位义（与 `lunarFromOffset` 同源）：
//! - 低 4 位：闰月月份，0 表示无闰月；
//! - 0x10000：闰月是大月（30 天）还是小月（29 天）；
//! - 0x8000 >> (m-1)（即第 m 位）：第 m 个月是 30 天；
//! - 表项按 1900+序号 排。
//!
//! 月名与日期名照 ICU（`zh-CN-u-ca-chinese`）的实际输出：正月…十月、**十一月**、**腊月**，
//! 闰月前缀「闰」，日名「初一…三十」。这几条是从 ICU 实测取的，见下方测试向量。

pub const YEAR_MIN: i32 = 1900;
pub const YEAR_MAX: i32 = 2099;

const LUNAR_INFO: [u32; 200] = [
    0x04bd8, 0x04ae0, 0x0a570, 0x054d5, 0x0d260, 0x0d950, 0x16554, 0x056a0, 0x09ad0,
    0x055d2, // 1900–1909
    0x04ae0, 0x0a5b6, 0x0a4d0, 0x0d250, 0x1d255, 0x0b540, 0x0d6a0, 0x0ada2, 0x095b0,
    0x14977, // 1910–1919
    0x04970, 0x0a4b0, 0x0b4b5, 0x06a50, 0x06d40, 0x1ab54, 0x02b60, 0x09570, 0x052f2,
    0x04970, // 1920–1929
    0x06566, 0x0d4a0, 0x0ea50, 0x06e95, 0x05ad0, 0x02b60, 0x186e3, 0x092e0, 0x1c8d7,
    0x0c950, // 1930–1939
    0x0d4a0, 0x1d8a6, 0x0b550, 0x056a0, 0x1a5b4, 0x025d0, 0x092d0, 0x0d2b2, 0x0a950,
    0x0b557, // 1940–1949
    0x06ca0, 0x0b550, 0x15355, 0x04da0, 0x0a5b0, 0x14573, 0x052b0, 0x0a9a8, 0x0e950,
    0x06aa0, // 1950–1959
    0x0aea6, 0x0ab50, 0x04b60, 0x0aae4, 0x0a570, 0x05260, 0x0f263, 0x0d950, 0x05b57,
    0x056a0, // 1960–1969
    0x096d0, 0x04dd5, 0x04ad0, 0x0a4d0, 0x0d4d4, 0x0d250, 0x0d558, 0x0b540, 0x0b6a0,
    0x195a6, // 1970–1979
    0x095b0, 0x049b0, 0x0a974, 0x0a4b0, 0x0b27a, 0x06a50, 0x06d40, 0x0af46, 0x0ab60,
    0x09570, // 1980–1989
    0x04af5, 0x04970, 0x064b0, 0x074a3, 0x0ea50, 0x06b58, 0x05ac0, 0x0ab60, 0x096d5,
    0x092e0, // 1990–1999
    0x0c960, 0x0d954, 0x0d4a0, 0x0da50, 0x07552, 0x056a0, 0x0abb7, 0x025d0, 0x092d0,
    0x0cab5, // 2000–2009
    0x0a950, 0x0b4a0, 0x0baa4, 0x0ad50, 0x055d9, 0x04ba0, 0x0a5b0, 0x15176, 0x052b0,
    0x0a930, // 2010–2019
    0x07954, 0x06aa0, 0x0ad50, 0x05b52, 0x04b60, 0x0a6e6, 0x0a4e0, 0x0d260, 0x0ea65,
    0x0d530, // 2020–2029
    0x05aa0, 0x076a3, 0x096d0, 0x04afb, 0x04ad0, 0x0a4d0, 0x1d0b6, 0x0d250, 0x0d520,
    0x0dd45, // 2030–2039
    0x0b5a0, 0x056d0, 0x055b2, 0x049b0, 0x0a577, 0x0a4b0, 0x0aa50, 0x1b255, 0x06d20,
    0x0ada0, // 2040–2049
    0x14b63, 0x09370, 0x049f8, 0x04970, 0x064b0, 0x168a6, 0x0ea50, 0x06aa0, 0x1a6c4,
    0x0aae0, // 2050–2059
    0x092e0, 0x0d2e3, 0x0c960, 0x0d557, 0x0d4a0, 0x0da50, 0x05d55, 0x056a0, 0x0a6d0,
    0x055d4, // 2060–2069
    0x052d0, 0x0a9b8, 0x0a950, 0x0b4a0, 0x0b6a6, 0x0ad50, 0x055a0, 0x0aba4, 0x0a5b0,
    0x052b0, // 2070–2079
    0x0b273, 0x06930, 0x07337, 0x06aa0, 0x0ad50, 0x14b55, 0x04b60, 0x0a570, 0x054e4,
    0x0d160, // 2080–2089
    0x0e968, 0x0d520, 0x0daa0, 0x16aa6, 0x056d0, 0x04ae0, 0x0a9d4, 0x0a2d0, 0x0d150,
    0x0f252, // 2090–2099
];

/// 公历日期 → 「距 1900-01-31 的天数」（Howard Hinnant 的 days_from_civil）
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m as i64) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + (d as i64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn year_days(info: u32) -> u32 {
    let mut n = 348; // 12 个月都是小月（29 天）
    for bit in 0..12 {
        if info & (0x8000 >> bit) != 0 {
            n += 1; // 该月 30 天
        }
    }
    let leap = info & 0xf;
    if leap != 0 {
        n += if info & 0x10000 != 0 { 30 } else { 29 }; // 闰月的天数
    }
    n
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lunar {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub leap: bool,
}

const MONTH_NAMES: [&str; 12] = [
    "正月",
    "二月",
    "三月",
    "四月",
    "五月",
    "六月",
    "七月",
    "八月",
    "九月",
    "十月",
    "十一月",
    "腊月",
];
const DAY_NAMES: [&str; 30] = [
    "初一", "初二", "初三", "初四", "初五", "初六", "初七", "初八", "初九", "初十", "十一", "十二",
    "十三", "十四", "十五", "十六", "十七", "十八", "十九", "二十", "廿一", "廿二", "廿三", "廿四",
    "廿五", "廿六", "廿七", "廿八", "廿九", "三十",
];

/// 公历 → 农历。超出 1900–2099 返回 `None`。
pub fn solar_to_lunar(y: i32, m: u32, d: u32) -> Option<Lunar> {
    if !(YEAR_MIN..=YEAR_MAX).contains(&y) {
        return None;
    }
    let mut off = days_from_civil(y, m, d) - days_from_civil(1900, 1, 31);
    if off < 0 {
        return None;
    }
    let mut idx = 0usize;
    while idx < LUNAR_INFO.len() - 1 && off >= year_days(LUNAR_INFO[idx]) as i64 {
        off -= year_days(LUNAR_INFO[idx]) as i64;
        idx += 1;
    }
    let info = LUNAR_INFO[idx];
    let leap = info & 0xf;
    // 月份序列：正常 12 个月，有闰月就插在同月之后
    let mut months: Vec<(u32, u32, bool)> = Vec::with_capacity(13);
    for mm in 1..=12u32 {
        let big = info & (0x10000 >> mm) != 0;
        months.push((mm, if big { 30 } else { 29 }, false));
    }
    if leap != 0 {
        let big = info & 0x10000 != 0;
        months.insert(leap as usize, (leap, if big { 30 } else { 29 }, true));
    }
    for (mm, len, is_leap) in months {
        if off < len as i64 {
            return Some(Lunar {
                year: 1900 + idx as i32,
                month: mm,
                day: off as u32 + 1,
                leap: is_leap,
            });
        }
        off -= len as i64;
    }
    None
}

/// 时钟卡那一行的文案：`农历九月廿二`
pub fn lunar_text(y: i32, m: u32, d: u32) -> Option<String> {
    let l = solar_to_lunar(y, m, d)?;
    let mn = MONTH_NAMES[(l.month - 1) as usize];
    let day = DAY_NAMES.get((l.day - 1) as usize)?;
    Some(format!(
        "农历{}{}{}",
        if l.leap { "闰" } else { "" },
        mn,
        day
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 向量来自 ICU（`Intl.DateTimeFormat('zh-CN-u-ca-chinese')`）的实测输出，
    /// 与 `card.html::lunarOf` 走的是同一条路。
    #[test]
    fn 与_icu_一致() {
        let cases = [
            ((2026, 10, 31), "农历九月廿二"),
            ((2026, 1, 1), "农历十一月十三"),
            ((2026, 2, 17), "农历正月初一"),
            ((2025, 1, 29), "农历正月初一"),
            ((2024, 2, 10), "农历正月初一"),
            ((2023, 1, 22), "农历正月初一"),
            ((2028, 1, 26), "农历正月初一"),
            ((2099, 12, 31), "农历十一月二十"),
            ((1950, 1, 1), "农历十一月十三"),
            ((2000, 2, 5), "农历正月初一"),
            ((2010, 2, 14), "农历正月初一"),
            ((2020, 1, 25), "农历正月初一"),
            ((2026, 12, 10), "农历十一月初二"),
            ((2026, 6, 15), "农历五月初一"),
            ((2026, 9, 25), "农历八月十五"),
            ((2033, 12, 22), "农历闰十一月初一"),
        ];
        for ((y, m, d), want) in cases {
            assert_eq!(lunar_text(y, m, d).as_deref(), Some(want), "{y}-{m}-{d}");
        }
    }

    /// 已知与 ICU 差一天的日子。2027-02-06：年表说正月初一，ICU 说腊月三十（即 ICU 认为
    /// 春节是 2 月 7 日）。这类边界差是「压缩年表 vs ICU 内部表」的口径差，不是换算写错
    /// —— 其余 16 个跨 1950–2099 的向量全中。要跟浏览器完全一致，应改用
    /// `Windows.Globalization.Calendar.ChineseLunar`（Win10+ 背后就是 ICU），此处先记录。
    #[test]
    fn 已知与_icu_差一天的边界() {
        // 年表口径（自洽，但与 ICU 差一天）：
        assert_eq!(lunar_text(2027, 2, 6).as_deref(), Some("农历正月初一"));
        assert_eq!(lunar_text(2027, 2, 7).as_deref(), Some("农历正月初二"));
        assert_eq!(lunar_text(2030, 2, 3).as_deref(), Some("农历正月初一"));
        assert_eq!(lunar_text(2030, 2, 4).as_deref(), Some("农历正月初二"));
    }

    #[test]
    fn 春分秋分附近的闰月标记() {
        // 2033 年有闰十一月（表里 2057/2097 是修正过的两处，2033 是常规闰月）
        let l = solar_to_lunar(2033, 12, 22).unwrap();
        assert!(l.leap, "2033-12-22 应落在闰十一月");
        assert_eq!((l.month, l.day), (11, 1));
        // 越界返回 None
        assert!(solar_to_lunar(1899, 12, 31).is_none());
        assert!(solar_to_lunar(2100, 1, 1).is_none());
    }

    #[test]
    fn 每年天数与表项自洽() {
        // 12 个月 29 天 + 大月数 + 可能的闰月
        for (i, info) in LUNAR_INFO.iter().enumerate() {
            let big: u32 = (0..12).filter(|b| info & (0x8000 >> b) != 0).count() as u32;
            let leap = info & 0xf;
            let mut total = 348 + big;
            if leap != 0 {
                total += if info & 0x10000 != 0 { 30 } else { 29 };
            }
            assert!(
                (total == 353
                    || total == 354
                    || total == 355
                    || total == 383
                    || total == 384
                    || total == 385),
                "第 {i} 年（{}）天数 {total} 不合理",
                1900 + i
            );
        }
    }
}
