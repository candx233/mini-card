//! 天气数据：**实况走中国天气网（国家气象局站点观测，跟手机天气同源）**，
//! 三日高低温仍用 Open-Meteo（数值预报，免 key）+ IP 归属地自动定城市。
//!
//! 为什么两套：2026-09-30 同时刻实测温州，中国天气网 29° 阴 vs Open-Meteo 25.8° 雷阵雨
//! —— Open-Meteo 的 `current` 是数值模式插值，不是站点实况，跟手机对不上是必然的。
//! 中国天气网免 key 但要城市代码，代码表 = wxcities.txt（省|名|id），选城市时解析一次存 config。
//!
//! 30 分钟一轮刷新 → `Arc<RwLock<Option<WeatherSnap>>>`，同时 `emit("weather")`
//! 推给卡片页；并落盘 `%APPDATA%\mini-card\weather.json`，重启先显上次的值。
//! 城市来源两种：手动（config 里有坐标 → src="manual"）或自动（无坐标 → 按公网
//! IP 归属地猜，src="ip"；本机走代理时会猜成出口地，界面会如实标注来源）。
//!
//! HTTP 用 ureq（阻塞 + rustls 内置根证书），不引入 async 运行时。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

const FORECAST: &str = "https://api.open-meteo.com/v1/forecast";
const AIR: &str = "https://air-quality-api.open-meteo.com/v1/air-quality";
const GEOCODE: &str = "https://geocoding-api.open-meteo.com/v1/search";
const IPWHO: &str = "https://ipwho.is/";
const IPAPI: &str = "http://ip-api.com/json/?lang=zh-CN";

/// 中国天气网实况。**必须带浏览器 UA + Referer，少了 UA 直接 403**（实测）；
/// 返回体是 `var dataSK={...};` 要自己剥壳。
const CMA_SK: &str = "http://d1.weather.com.cn/sk_2d/{}.html";
const CMA_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
const CMA_REFERER: &str = "http://www.weather.com.cn/";
/// 全国城市代码表，行格式 `省|名|id`（2567 条，见 wxcities.txt）
const WXCITIES: &str = include_str!("wxcities.txt");

/// 刷新间隔：30 分钟（设计稿写的就是 30 分钟一刷；每天 48 次 × 2 接口，远低于免费额度）
const PERIOD: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DayOut {
    /// 今天 / 明天 / 后天
    pub label: String,
    /// WMO 天气码
    pub code: i32,
    pub hi: f64,
    pub lo: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WeatherSnap {
    pub city: String,
    pub lat: f64,
    pub lon: f64,
    /// 当前气温 ℃
    pub temp: f64,
    pub code: i32,
    /// PM2.5 µg/m³，-1 = 没取到
    pub pm25: f64,
    /// 空气质量等级中文（优/良/轻度污染…），空 = 没取到
    pub air: String,
    /// 当前天气中文（晴/多云/阴…）
    pub text: String,
    pub humidity: i64,
    pub days: Vec<DayOut>,
    /// unix 秒
    pub at: i64,
    /// 城市来源：manual / ip
    pub src: String,
    /// 实况来源："中国天气网"（站点观测）/ "Open-Meteo"（数值模式兜底）
    #[serde(default)]
    pub provider: String,
    /// 用到的中国天气网城市代码（没解析出来 = 空）
    #[serde(default)]
    pub wx_id: Option<String>,
}

pub type SharedWeather = Arc<RwLock<Option<WeatherSnap>>>;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Place {
    /// "绍兴 · 浙江省"
    pub label: String,
    pub city: String,
    pub region: String,
    pub lat: f64,
    pub lon: f64,
}

/// WMO 天气码 → 中文（卡片和图标的映射在 web 侧，这里只给文案）
pub fn wmo_text(code: i32) -> &'static str {
    match code {
        0 => "晴",
        1 => "晴间多云",
        2 => "多云",
        3 => "阴",
        45 | 48 => "雾",
        51 | 53 | 55 => "毛毛雨",
        56 | 57 => "冻雨",
        61 => "小雨",
        63 => "中雨",
        65 => "大雨",
        66 | 67 => "冻雨",
        71 => "小雪",
        73 => "中雪",
        75 | 77 => "大雪",
        80 => "阵雨",
        81 | 82 => "强阵雨",
        85 | 86 => "阵雪",
        95 => "雷阵雨",
        96 | 99 => "雷暴冰雹",
        _ => "未知",
    }
}

/// PM2.5 → 中国 AQI 等级（HJ 633-2012 24h 均值分级）
pub fn air_text(pm25: f64) -> &'static str {
    if pm25 < 0.0 {
        ""
    } else if pm25 <= 35.0 {
        "优"
    } else if pm25 <= 75.0 {
        "良"
    } else if pm25 <= 115.0 {
        "轻度污染"
    } else if pm25 <= 150.0 {
        "中度污染"
    } else if pm25 <= 250.0 {
        "重度污染"
    } else {
        "严重污染"
    }
}

/// AQI（中国天气网 `aqi` 字段给的就是 AQI）→ 等级。HJ633-2012 分级，数字与 PM2.5 分界
/// 巧合同值但语义不同，别跟上面的 `air_text` 混用。
pub fn aqi_text(aqi: f64) -> &'static str {
    if aqi < 0.0 {
        ""
    } else if aqi <= 50.0 {
        "优"
    } else if aqi <= 100.0 {
        "良"
    } else if aqi <= 150.0 {
        "轻度污染"
    } else if aqi <= 200.0 {
        "中度污染"
    } else if aqi <= 300.0 {
        "重度污染"
    } else {
        "严重污染"
    }
}

/// 中国天气网中文天气现象 → WMO 码（卡片图标走 `wmoIcon(code)`，分组口径与
/// web/card.html 一致）。顺序要紧：先雷/雹、再雪、再雨，「雷阵雨」不能被「雨」吃掉。
pub fn cn_to_wmo(t: &str) -> i32 {
    let s = t.trim();
    if s.contains('雹') {
        return 96;
    }
    if s.contains("雷") {
        return 95;
    }
    if s.contains("雪") {
        if s.contains("雨夹雪") {
            return 66;
        }
        if s.contains("阵") {
            return 85;
        }
        if s.contains("中") {
            return 73;
        }
        if s.contains('大') || s.contains('暴') {
            return 75;
        }
        return 71;
    }
    if s.contains("雨") {
        if s.contains("毛毛") || s.contains("零星") {
            return 53;
        }
        if s.contains("冻") {
            return 66;
        }
        if s.contains("阵") {
            return 80;
        }
        if s.contains('暴') {
            return 65;
        }
        if s.contains("中") {
            return 63;
        }
        if s.contains('小') {
            return 61;
        }
        return 63;
    }
    if s.contains("雾") || s.contains("霾") || s.contains("烟") {
        return 45;
    }
    if s.contains('沙') || s.contains('尘') {
        return 45;
    }
    if s.contains("阴") {
        return 3;
    }
    if s.contains("多云") || s.contains("少云") || s.contains("晴间") {
        return 2;
    }
    if s.contains("晴") {
        return 0;
    }
    if s.contains("云") {
        return 2;
    }
    3
}

/// `省|名|id` 代码表（wxcities.txt，2567 条）；解析一次常驻
fn wx_table() -> &'static [(&'static str, &'static str, &'static str)] {
    static T: std::sync::OnceLock<Vec<(&'static str, &'static str, &'static str)>> =
        std::sync::OnceLock::new();
    T.get_or_init(|| {
        WXCITIES
            .lines()
            .filter_map(|l| {
                let mut it = l.split('|');
                let p = it.next()?;
                let n = it.next()?;
                let id = it.next()?;
                Some((p, n, id))
            })
            .collect()
    })
}

/// 「温州市」→「温州」（≥2 字才剥，防止单字被剥空）
fn strip_city(name: &str) -> &str {
    // 顺序：长后缀在前，否则「广西壮族自治区」会被「自治区」剥成「广西壮族」
    for suf in ["特别行政区", "维吾尔自治区", "壮族自治区", "回族自治区", "自治区", "地区", "省", "市", "县", "区", "盟"] {
        if let Some(b) = name.strip_suffix(suf) {
            if b.chars().count() >= 2 {
                return b;
            }
        }
    }
    name
}

/// 城市名 + 省 → 中国天气网城市代码。查不到返回 None，实况回退 Open-Meteo。
/// Photon 会返回「温州南」「温州龙湾国际机场」这类 POI，精确名查不到时按最长前缀兜底。
pub fn wx_resolve(city: &str, region: &str) -> Option<String> {
    let t = wx_table();
    let city = city.trim();
    let region = strip_city(region.trim()); // 「浙江省」→「浙江」；strip_city 只剥后缀，省名同样适用

    let mut names: Vec<&str> = vec![city];
    let stripped = strip_city(city);
    if stripped != city {
        names.push(stripped);
    }

    for n in &names {
        let mut hits: Vec<&(&str, &str, &str)> =
            t.iter().filter(|(_, name, _)| name == n).collect();
        if !region.is_empty() {
            let f: Vec<_> = hits.iter().copied().filter(|(p, _, _)| *p == region).collect();
            if !f.is_empty() {
                hits = f;
            }
        }
        if let Some((_, _, id)) = hits.first() {
            return Some((*id).to_owned());
        }
    }
    for n in &names {
        let mut hits: Vec<&(&str, &str, &str)> = t
            .iter()
            .filter(|(_, name, _)| name.chars().count() >= 2 && n.starts_with(name))
            .collect();
        if !region.is_empty() {
            let f: Vec<_> = hits.iter().copied().filter(|(p, _, _)| *p == region).collect();
            if !f.is_empty() {
                hits = f;
            }
        }
        hits.sort_by_key(|(_, name, _)| std::cmp::Reverse(name.chars().count()));
        if let Some((_, _, id)) = hits.first() {
            return Some((*id).to_owned());
        }
    }
    None
}

#[derive(Debug)]
struct CmaNow {
    temp: f64,
    text: String,
    hum: i64,
    aqi: f64,
}

/// 中国天气网实况（带 UA/Referer；任一步失败返回 None → 调用方回退 Open-Meteo）
fn cma_current(id: &str) -> Option<CmaNow> {
    let url = CMA_SK.replace("{}", id);
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .build()
        .new_agent();
    let body = agent
        .get(&url)
        .header("User-Agent", CMA_UA)
        .header("Referer", CMA_REFERER)
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    let t = body.trim().trim_start_matches("var dataSK=").trim_end_matches(';').trim();
    let v: serde_json::Value = serde_json::from_str(t).ok()?;
    let temp = v
        .get("temp")
        .and_then(|x| x.as_str())
        .and_then(|x| x.parse::<f64>().ok())?;
    let text = v.get("weather").and_then(|x| x.as_str()).unwrap_or("").trim().to_owned();
    if text.is_empty() {
        return None;
    }
    let hum = v
        .get("SD")
        .and_then(|x| x.as_str())
        .and_then(|x| x.trim_end_matches('%').trim().parse::<i64>().ok())
        .unwrap_or(-1);
    let aqi = v
        .get("aqi")
        .and_then(|x| x.as_str())
        .and_then(|x| x.parse::<f64>().ok())
        .unwrap_or(-1.0);
    Some(CmaNow { temp, text, hum, aqi })
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn cache_path() -> PathBuf {
    crate::config::path().with_file_name("weather.json")
}

fn get_json(url: &str) -> Option<serde_json::Value> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .build()
        .new_agent();
    let text = agent.get(url).call().ok()?.body_mut().read_to_string().ok()?;
    serde_json::from_str(&text).ok()
}

fn f(v: &serde_json::Value, k: &str) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(f64::NAN)
}

/// 按公网 IP 猜城市。本机开着系统代理时拿到的是代理出口地（改成手动指定即可）。
fn ip_locate() -> Option<(String, f64, f64)> {
    if let Some(v) = get_json(IPWHO) {
        if v.get("success").and_then(|x| x.as_bool()).unwrap_or(false) {
            let city = v.get("city").and_then(|x| x.as_str()).unwrap_or("").to_owned();
            let lat = f(&v, "latitude");
            let lon = f(&v, "longitude");
            if !city.is_empty() && lat.is_finite() && lon.is_finite() {
                return Some((city, lat, lon));
            }
        }
    }
    // 兜底：ip-api（中文，免费版仅 HTTP）
    if let Some(v) = get_json(IPAPI) {
        if v.get("status").and_then(|x| x.as_str()) == Some("success") {
            let city = v.get("city").and_then(|x| x.as_str()).unwrap_or("").to_owned();
            let lat = f(&v, "lat");
            let lon = f(&v, "lon");
            if !city.is_empty() && lat.is_finite() && lon.is_finite() {
                return Some((city, lat, lon));
            }
        }
    }
    None
}

/// 城市名 → 候选坐标（主界面「搜索城市」用）
///
/// 主用 Photon（komoot，OSM 数据，免 key，中文地名全 —— 实测 Open-Meteo 自家的
/// 地理编码查「绍兴」只给四川一个小村，浙江绍兴根本没有，不能当主源）；
/// Photon 不通时回退 Open-Meteo 地理编码。
pub fn search(q: &str) -> Vec<Place> {
    let mut out = photon(q);
    if out.is_empty() {
        out = openmeteo_geo(q);
    }
    out
}

fn photon(q: &str) -> Vec<Place> {
    let url = format!("https://photon.komoot.io/api/?q={}&limit=10", urlencode(q));
    let Some(v) = get_json(&url) else { return Vec::new() };
    let Some(arr) = v.get("features").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<Place> = Vec::new();
    for f in arr {
        let Some(p) = f.get("properties") else { continue };
        let city = p.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_owned();
        if city.is_empty() {
            continue;
        }
        let region = p
            .get("state")
            .and_then(|x| x.as_str())
            .or_else(|| p.get("county").and_then(|x| x.as_str()))
            .unwrap_or("")
            .to_owned();
        let country = p.get("country").and_then(|x| x.as_str()).unwrap_or("");
        let coords = f
            .get("geometry")
            .and_then(|g| g.get("coordinates"))
            .and_then(|c| c.as_array());
        let lon = coords.and_then(|c| c.first()).and_then(|x| x.as_f64());
        let lat = coords.and_then(|c| c.get(1)).and_then(|x| x.as_f64());
        let (Some(lat), Some(lon)) = (lat, lon) else { continue };
        let mut label = city.clone();
        if !region.is_empty() && region != city {
            label.push_str(" · ");
            label.push_str(&region);
        }
        if !country.is_empty() && country != "中国" && country != "China" {
            label.push_str(" · ");
            label.push_str(country);
        }
        if out.iter().any(|o| o.label == label) {
            continue;   // OSM 里同名节点很多（车站/路口），按标签去重
        }
        out.push(Place { label, city, region, lat, lon });
        if out.len() >= 8 {
            break;
        }
    }
    out
}

fn openmeteo_geo(q: &str) -> Vec<Place> {
    let url = format!(
        "{}?name={}&count=8&language=zh&format=json",
        GEOCODE,
        urlencode(q)
    );
    let Some(v) = get_json(&url) else { return Vec::new() };
    let Some(arr) = v.get("results").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|r| {
            let city = r.get("name").and_then(|x| x.as_str())?.to_owned();
            let region = r
                .get("admin1")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_owned();
            let country = r.get("country").and_then(|x| x.as_str()).unwrap_or("");
            let lat = f(r, "latitude");
            let lon = f(r, "longitude");
            if !lat.is_finite() || !lon.is_finite() {
                return None;
            }
            let mut label = city.clone();
            if !region.is_empty() && region != city {
                label.push_str(" · ");
                label.push_str(&region);
            }
            if !country.is_empty() && country != "中国" {
                label.push_str(" · ");
                label.push_str(country);
            }
            Some(Place { label, city, region, lat, lon })
        })
        .collect()
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 取一轮数据：定位 → 中国天气网实况（拿不到回退 Open-Meteo 实况）→ 三日预报（Open-Meteo）
///
/// 两套数据的分工：实况温度/现象/湿度/AQI 用中国天气网（站点观测，跟手机同一套），
/// 三日高低温仍用 Open-Meteo（数值预报，免 key 且误差小）。任一源挂了都能单独兜底。
pub fn fetch(cfg: &crate::config::Config) -> Result<WeatherSnap, String> {
    let manual = matches!(
        (cfg.settings.weather_lat, cfg.settings.weather_lon),
        (Some(_), Some(_))
    );
    let (city, lat, lon, src) = if manual {
        (
            cfg.settings.weather_city.clone(),
            cfg.settings.weather_lat.unwrap(),
            cfg.settings.weather_lon.unwrap(),
            "manual".to_owned(),
        )
    } else {
        // 手动改过城市名但没坐标：先按名字查一次；否则按 IP 猜
        let named = if cfg.settings.weather_city.trim().is_empty() {
            None
        } else {
            search(&cfg.settings.weather_city).into_iter().next()
        };
        match named {
            Some(p) => (p.city, p.lat, p.lon, "manual".to_owned()),
            None => {
                let (c, la, lo) = ip_locate().ok_or("定位失败（IP 归属地不可达）")?;
                (c, la, lo, "ip".to_owned())
            }
        }
    };

    // ① 城市代码：选城市时存的优先，老 config 没存就按名字现查一次
    let wxid = cfg
        .settings
        .weather_wxid
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| wx_resolve(&city, ""));
    let cma = wxid.as_deref().and_then(cma_current);

    // ② 三日预报（Open-Meteo）。实况有中国天气网时预报挂了也不致命
    let f_url = format!(
        "{}?latitude={:.4}&longitude={:.4}&current=temperature_2m,relative_humidity_2m,weather_code\
         &daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=3",
        FORECAST, lat, lon
    );
    let v = get_json(&f_url);
    let cur = v.as_ref().and_then(|x| x.get("current"));
    let days = v
        .as_ref()
        .and_then(|x| x.get("daily"))
        .map(parse_days)
        .unwrap_or_default();

    // ③ 实况：中国天气网优先，Open-Meteo 兜底
    let mut provider = "Open-Meteo".to_owned();
    let (temp, code, text, humidity) = if let Some(c) = cma.as_ref() {
        provider = "中国天气网".to_owned();
        (c.temp, cn_to_wmo(&c.text), c.text.clone(), c.hum)
    } else if let Some(cur) = cur {
        let code = cur.get("weather_code").and_then(|x| x.as_i64()).unwrap_or(-1) as i32;
        (
            f(cur, "temperature_2m"),
            code,
            wmo_text(code).to_owned(),
            cur.get("relative_humidity_2m").and_then(|x| x.as_i64()).unwrap_or(-1),
        )
    } else {
        return Err("天气接口不可达".to_owned());
    };

    // ④ 空气质量：中国天气网直接给 AQI；没有才去 Open-Meteo 空气接口要 PM2.5
    let mut pm25 = -1.0;
    let mut air = String::new();
    if let Some(c) = cma.as_ref() {
        if c.aqi >= 0.0 {
            air = aqi_text(c.aqi).to_owned();
        }
    }
    if air.is_empty() {
        let a_url = format!(
            "{}?latitude={:.4}&longitude={:.4}&current=pm2_5&timezone=auto",
            AIR, lat, lon
        );
        if let Some(p) = get_json(&a_url)
            .and_then(|a| a.get("current").and_then(|c| c.get("pm2_5")).and_then(|x| x.as_f64()))
        {
            pm25 = p;
            air = air_text(p).to_owned();
        }
    }

    Ok(WeatherSnap {
        city,
        lat,
        lon,
        temp,
        code,
        humidity,
        pm25,
        air,
        text,
        days,
        at: now(),
        src,
        provider,
        wx_id: wxid,
    })
}

/// Open-Meteo `daily` → 三日（今天/明天/后天）
fn parse_days(d: &serde_json::Value) -> Vec<DayOut> {
    let codes = d.get("weather_code").and_then(|x| x.as_array());
    let his = d.get("temperature_2m_max").and_then(|x| x.as_array());
    let los = d.get("temperature_2m_min").and_then(|x| x.as_array());
    let lbl = ["今天", "明天", "后天"];
    let mut days = Vec::new();
    for i in 0..3 {
        let code = codes.and_then(|a| a.get(i)).and_then(|x| x.as_i64()).unwrap_or(-1) as i32;
        let hi = his.and_then(|a| a.get(i)).and_then(|x| x.as_f64()).unwrap_or(f64::NAN);
        let lo = los.and_then(|a| a.get(i)).and_then(|x| x.as_f64()).unwrap_or(f64::NAN);
        days.push(DayOut { label: lbl[i].to_owned(), code, hi, lo });
    }
    days
}

fn read_cache() -> Option<WeatherSnap> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(s: &WeatherSnap) {
    if let Ok(text) = serde_json::to_string(s) {
        let _ = std::fs::write(cache_path(), text);
    }
}

/// 拉一轮并广播；失败时保留旧值（卡片自己按 35 分钟判「数据源异常」）
pub fn refresh(shared: &SharedWeather, app: &AppHandle) -> Result<WeatherSnap, String> {
    let cfg = crate::config::Config::load();
    let s = fetch(&cfg)?;
    *shared.write().unwrap() = Some(s.clone());
    write_cache(&s);
    let _ = app.emit("weather", &s);
    Ok(s)
}

/// 开机：先贴上次落盘的值（卡片立刻有数），再起 30 分钟轮询线程
pub fn spawn(shared: SharedWeather, app: AppHandle) {
    if let Some(old) = read_cache() {
        *shared.write().unwrap() = Some(old);
    }
    thread::spawn(move || loop {
        let _ = refresh(&shared, &app);
        thread::sleep(PERIOD);
    });
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wx_resolve_known_cities() {
        // 精确名 + 省
        assert_eq!(wx_resolve("温州", "浙江省").as_deref(), Some("101210701"));
        assert_eq!(wx_resolve("绍兴", "浙江").as_deref(), Some("101210501"));
        // 带后缀 / 不给省
        assert_eq!(wx_resolve("温州市", "").as_deref(), Some("101210701"));
        assert_eq!(wx_resolve("绍兴", "").as_deref(), Some("101210501"));
        // Photon 会返回 POI：最长前缀兜底
        assert_eq!(wx_resolve("温州南", "浙江省").as_deref(), Some("101210701"));
        assert_eq!(wx_resolve("温州龙湾国际机场", "浙江省").as_deref(), Some("101210701"));
        // 重名城市靠省区分（朝阳：北京 / 辽宁）
        assert_eq!(wx_resolve("朝阳", "辽宁").as_deref(), Some("101071201"));
        assert_eq!(wx_resolve("朝阳", "北京").as_deref(), Some("101010300"));
        // 查不到必须是 None（调用方回退 Open-Meteo），不能瞎猜
        assert_eq!(wx_resolve("不存在的城市xyz", "火星省"), None);
    }

    #[test]
    fn cn_wmo_groups_match_card_icons() {
        assert_eq!(cn_to_wmo("晴"), 0);
        assert_eq!(cn_to_wmo("多云"), 2);
        assert_eq!(cn_to_wmo("阴"), 3);
        assert_eq!(cn_to_wmo("小雨"), 61);
        assert_eq!(cn_to_wmo("中雨"), 63);
        assert_eq!(cn_to_wmo("阵雨"), 80);
        assert_eq!(cn_to_wmo("雷阵雨"), 95); // 不能被「雨」分支吃掉
        assert_eq!(cn_to_wmo("雷暴冰雹"), 96);
        assert_eq!(cn_to_wmo("中雪"), 73);
        assert_eq!(cn_to_wmo("雾"), 45);
    }

    #[test]
    fn aqi_grades() {
        assert_eq!(aqi_text(30.0), "优");
        assert_eq!(aqi_text(60.0), "良");
        assert_eq!(aqi_text(160.0), "中度污染");
        assert_eq!(aqi_text(-1.0), "");
    }
}
