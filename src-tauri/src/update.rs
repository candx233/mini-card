//! 自更新（绿色单 exe 形态）：查 Release → 取 latest.json → ed25519 验签 → 下载 zip →
//! sha256 校验 → 解压 → 改名让位 + 覆盖 exe → 启动新版 → 本进程退出。
//!
//! 信任根 = 本文件硬编码的 ed25519 公钥（私钥在打包机 %APPDATA%\mini-card\keys\，
//! 打包脚本 docs/verify/mc_pack_release.py）。签名口径：payload = "<version>|<sha256>"。
//! **先验签、再按清单里的 sha256 校验下载到的 zip**，两条都过才允许动 exe —— 只靠 HTTPS 不够：
//! 这一步替换的是可执行文件。
//!
//! 替换的时序（本机实测过）：Windows 允许**改名**正在运行的 exe、也允许把新 exe 写到原路径，
//! 但不允许删除还在跑的旧文件 → 旧 exe 改名成 mini-card.old*.exe，下次启动时清理。
//! 不需要临时脚本、不需要管理员权限。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

const RELEASES_API: &str = "https://api.github.com/repos/candx233/mini-card/releases/latest";
const REPO_API: &str = "https://api.github.com/repos/candx233/mini-card";
/// 与打包脚本里的私钥配对（ed25519，32 字节 base64）
const PUBKEY_B64: &str = "BrG25loJP5MVch7Y/KPYp1ADx8L5jGbvkQ+VsrVOwdY=";
const UA: &str = "mini-card";
/// 换新版的重启参数：带它启动时会在拿单实例锁上多等一会儿（老进程正在退出）
pub const ARG_AFTER_UPDATE: &str = "--after-update";

/// 更新源。debug 构建允许用环境变量指到本地假服务做端到端测试；release 里忽略环境变量。
#[cfg(debug_assertions)]
fn releases_api() -> String {
    std::env::var("MC_UPDATE_API").unwrap_or_else(|_| RELEASES_API.to_owned())
}
#[cfg(not(debug_assertions))]
fn releases_api() -> String {
    RELEASES_API.to_owned()
}

fn agent(timeout_secs: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
        .build()
        .new_agent()
}

fn str_field(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned()
}

pub fn ver_tuple(s: &str) -> Vec<u32> {
    s.trim_start_matches(['v', 'V'])
        .split(|c: char| !c.is_ascii_digit())
        .filter(|x| !x.is_empty())
        .filter_map(|x| x.parse::<u32>().ok())
        .collect()
}

/// 仓库是否已公开（区分「没发过 Release」和「还没公开」两种 404）
fn repo_public() -> bool {
    agent(10)
        .get(REPO_API)
        .header("User-Agent", UA)
        .call()
        .is_ok()
}

pub fn fetch_latest_release() -> Result<serde_json::Value, String> {
    match agent(15)
        .get(releases_api())
        .header("User-Agent", UA)
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(mut r) => {
            let text = r
                .body_mut()
                .read_to_string()
                .map_err(|e| format!("读取响应失败：{e}"))?;
            serde_json::from_str(&text).map_err(|e| format!("解析响应失败：{e}"))
        }
        Err(ureq::Error::StatusCode(404)) => {
            if repo_public() {
                Err("仓库已公开，但还没发布过 Release".to_owned())
            } else {
                Err("拿不到发布信息：仓库尚未公开（或不存在）".to_owned())
            }
        }
        Err(ureq::Error::StatusCode(code)) => Err(format!("GitHub 返回 {code}")),
        Err(e) => Err(format!("网络请求失败：{e}")),
    }
}

/// 查一次：有新版回 has_new=true（界面用）
pub fn check(current: &str) -> Result<serde_json::Value, String> {
    let v = fetch_latest_release()?;
    let tag = str_field(&v, "tag_name");
    let url = str_field(&v, "html_url");
    let notes = str_field(&v, "body").lines().next().unwrap_or("").to_owned();
    let assets = v.get("assets").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let zip_name = assets
        .iter()
        .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
        .find(|n| n.to_ascii_lowercase().ends_with(".zip"))
        .unwrap_or("")
        .to_owned();
    let zip_size = assets
        .iter()
        .find(|a| str_field(a, "name") == zip_name)
        .and_then(|a| a.get("size").and_then(|s| s.as_u64()))
        .unwrap_or(0);
    let has_manifest = assets
        .iter()
        .any(|a| str_field(a, "name") == "latest.json");
    let has_new = !tag.is_empty() && ver_tuple(&tag) > ver_tuple(current);
    Ok(serde_json::json!({
        "current": current,
        "latest": tag,
        "url": url,
        "notes": notes,
        "has_new": has_new,
        "zip_name": zip_name,
        "zip_size": zip_size,
        "can_update": has_new && has_manifest,
    }))
}

fn emit_progress(app: &AppHandle, phase: &str, text: &str, got: u64, total: u64) {
    let pct = if total > 0 { (got as f64) * 100.0 / (total as f64) } else { 0.0 };
    let _ = app.emit(
        "update_progress",
        serde_json::json!({
            "phase": phase, "text": text, "got": got, "total": total,
            "pct": (pct * 10.0).round() / 10.0,
        }),
    );
}

// ─── 校验 ───

#[derive(serde::Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    #[allow(dead_code)]
    tag: String,
    #[serde(default)]
    asset: String,
    #[serde(default)]
    size: u64,
    sha256: String,
    sig: String,
    #[serde(default)]
    pubkey: String,
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| format!("base64 解码失败：{e}"))
}

/// 验签：payload = "<version>|<sha256>"
fn verify_manifest(man: &Manifest) -> Result<(), String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    if !man.pubkey.is_empty() && man.pubkey != PUBKEY_B64 {
        return Err("更新清单用的公钥与内置公钥不一致".to_owned());
    }
    let pk = b64_decode(PUBKEY_B64)?;
    let pk: [u8; 32] = pk
        .as_slice()
        .try_into()
        .map_err(|_| "内置公钥长度不对".to_owned())?;
    let vk = VerifyingKey::from_bytes(&pk).map_err(|e| format!("内置公钥无效：{e}"))?;
    let sig_bytes = b64_decode(&man.sig)?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|e| format!("签名格式不对：{e}"))?;
    let payload = format!("{}|{}", man.version, man.sha256);
    vk.verify(payload.as_bytes(), &sig)
        .map_err(|_| "签名校验失败：更新包不是官方签名（可能被篡改）".to_owned())
}

fn sha256_file(p: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(p).map_err(|e| format!("打开下载文件失败：{e}"))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("读取失败：{e}"))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// 目标目录可写？（更新要往 exe 所在目录写文件；Program Files 之类会失败，提前给明确提示）
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".mc-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ─── 下载 / 解压 ───

fn http_get_string(url: &str) -> Result<String, String> {
    agent(20)
        .get(url)
        .header("User-Agent", UA)
        .call()
        .map_err(|e| format!("请求失败：{e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("读取响应失败：{e}"))
}

fn download(url: &str, dest: &Path, expect: u64, app: &AppHandle) -> Result<u64, String> {
    let mut resp = agent(120)
        .get(url)
        .header("User-Agent", UA)
        .call()
        .map_err(|e| format!("下载失败：{e}"))?;
    let total = resp.body().content_length().unwrap_or(expect);
    let mut reader = resp.body_mut().as_reader();
    let mut f = std::fs::File::create(dest).map_err(|e| format!("写临时文件失败：{e}"))?;
    let mut buf = vec![0u8; 128 * 1024];
    let mut got: u64 = 0;
    let mut last = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("下载中断：{e}"))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|e| format!("写盘失败：{e}"))?;
        got += n as u64;
        if got - last >= 512 * 1024 {
            last = got;
            emit_progress(app, "download", "正在下载更新包…", got, total);
        }
    }
    f.flush().ok();
    if expect > 0 && got != expect {
        return Err(format!("下载不完整：拿到 {got} 字节，清单写的是 {expect}"));
    }
    Ok(got)
}

/// 从 zip 里抽出 mini-card.exe 和 使用说明.txt（条目名形如 MiniCard-v0.1.0/xxx）
fn extract(zip_path: &Path, out_dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let f = std::fs::File::open(zip_path).map_err(|e| format!("打开 zip 失败：{e}"))?;
    let mut ar = zip::ZipArchive::new(f).map_err(|e| format!("解析 zip 失败：{e}"))?;
    let mut exe: Option<PathBuf> = None;
    let mut txt: Option<PathBuf> = None;
    for i in 0..ar.len() {
        let mut e = ar.by_index(i).map_err(|e| format!("读取 zip 条目失败：{e}"))?;
        if e.is_dir() {
            continue;
        }
        let name = e.name().to_owned();
        let base = name.rsplit('/').next().unwrap_or(&name).to_owned();
        let target = if base.eq_ignore_ascii_case("mini-card.exe") {
            Some(out_dir.join("mini-card.exe"))
        } else if base.to_ascii_lowercase().ends_with(".txt") {
            Some(out_dir.join(base))
        } else {
            None
        };
        let Some(tp) = target else { continue };
        let mut out = std::fs::File::create(&tp).map_err(|e| format!("解压写文件失败：{e}"))?;
        std::io::copy(&mut e, &mut out).map_err(|e| format!("解压失败：{e}"))?;
        if tp.file_name().map(|n| n.eq_ignore_ascii_case("mini-card.exe")).unwrap_or(false) {
            exe = Some(tp);
        } else {
            txt = Some(tp);
        }
    }
    let exe = exe.ok_or("更新包里没有 mini-card.exe")?;
    let txt = txt.unwrap_or_else(|| out_dir.join("使用说明.txt"));
    // 粗校验：PE 文件头
    let head = std::fs::read(&exe).map_err(|e| format!("读解压出来的 exe 失败：{e}"))?;
    if head.len() < 1024 || &head[..2] != b"MZ" {
        return Err("更新包里的 exe 不是可执行文件".to_owned());
    }
    if head.len() < 1024 * 1024 {
        return Err("更新包里的 exe 体积异常（小于 1MB）".to_owned());
    }
    Ok((exe, txt))
}

// ─── 替换 + 重启 ───

/// 把当前 exe 改名让位（运行中也允许改名）；返回备份路径
fn park_current_exe(exe: &Path) -> Result<PathBuf, String> {
    let dir = exe.parent().ok_or("拿不到程序目录")?;
    let mut last_err = String::new();
    for i in 0..8 {
        let name = if i == 0 {
            "mini-card.old.exe".to_owned()
        } else {
            format!("mini-card.old{i}.exe")
        };
        let p = dir.join(&name);
        if p.exists() {
            // 上一轮留下的、可能还被别的进程锁着 → 删不掉就换个名字
            let _ = std::fs::remove_file(&p);
            if p.exists() {
                continue;
            }
        }
        match std::fs::rename(exe, &p) {
            Ok(_) => return Ok(p),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("无法给当前程序让位（{last_err}）"))
}

fn swap_and_restart(exe: &Path, tmp: &Path) -> Result<(), String> {
    let dir = exe.parent().ok_or("拿不到程序目录")?.to_path_buf();
    let new_exe = tmp.join("mini-card.exe");
    let parked = park_current_exe(exe)?;
    // 复制新 exe 到原路径；失败要回滚
    if let Err(e) = std::fs::copy(&new_exe, exe) {
        let _ = std::fs::rename(&parked, exe);
        return Err(format!("写入新程序失败（已回滚）：{e}"));
    }
    // 说明书：目标目录里本来有才覆盖（用户没解压它就别硬塞一份）
    let dst_txt = dir.join("使用说明.txt");
    let src_txt = tmp.join("使用说明.txt");
    if dst_txt.exists() && src_txt.exists() {
        let _ = std::fs::copy(&src_txt, &dst_txt);
    }
    // 启动新版（带 --after-update：它在拿单实例锁时会多等一会儿，等本进程退出）
    let started = std::process::Command::new(exe)
        .arg(ARG_AFTER_UPDATE)
        .current_dir(&dir)
        .spawn();
    match started {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("更新已就位，但启动新版失败（手动双击即可）：{e}")),
    }
}

// ─── 主流程 ───

pub fn install(app: &AppHandle) -> Result<(), String> {
    let cur = app.package_info().version.to_string();
    emit_progress(app, "check", "正在查询新版本…", 0, 0);
    let rel = fetch_latest_release()?;
    let tag = str_field(&rel, "tag_name");
    let ver = tag.trim_start_matches(['v', 'V']).to_owned();
    if ver.is_empty() {
        return Err("拿不到最新版本号".to_owned());
    }
    if !(ver_tuple(&ver) > ver_tuple(&cur)) {
        return Err(format!("当前已是最新版本（v{cur}）"));
    }

    let assets = rel.get("assets").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let find = |name: &str| -> Option<String> {
        assets
            .iter()
            .find(|a| str_field(a, "name") == name)
            .map(|a| str_field(a, "browser_download_url"))
            .filter(|u| !u.is_empty())
    };
    let man_url = find("latest.json")
        .ok_or("这个版本没发布更新清单（latest.json），请到 Release 页手动下载")?;

    emit_progress(app, "manifest", "正在校验更新包签名…", 0, 0);
    let man_text = http_get_string(&man_url)?;
    let man: Manifest =
        serde_json::from_str(&man_text).map_err(|e| format!("更新清单解析失败：{e}"))?;
    if man.version != ver {
        return Err(format!("清单版本（{}）与 Release（{ver}）不一致", man.version));
    }
    verify_manifest(&man)?;

    let zip_name = if !man.asset.is_empty() {
        man.asset.clone()
    } else {
        assets
            .iter()
            .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
            .find(|n| n.to_ascii_lowercase().ends_with(".zip"))
            .ok_or("这个版本没发布 zip 包，请到 Release 页手动下载")?
            .to_owned()
    };
    let zip_url = find(&zip_name).ok_or_else(|| format!("Release 里找不到资产 {zip_name}"))?;

    let exe = std::env::current_exe().map_err(|e| format!("拿不到当前程序路径：{e}"))?;
    let dir = exe.parent().ok_or("拿不到程序目录")?.to_path_buf();
    if !dir_writable(&dir) {
        return Err(format!(
            "程序目录不可写（{}），请到 Release 页手动下载",
            dir.display()
        ));
    }

    let tmp = std::env::temp_dir().join("mini-card-update");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("建临时目录失败：{e}"))?;
    let zip_path = tmp.join("pkg.zip");

    emit_progress(app, "download", "正在下载更新包…", 0, man.size);
    let got = download(&zip_url, &zip_path, man.size, app)?;

    emit_progress(app, "verify", "正在校验下载的文件…", got, got.max(1));
    let sha = sha256_file(&zip_path)?;
    if sha.to_lowercase() != man.sha256.to_lowercase() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err("校验失败：下载到的文件与清单的 sha256 不一致".to_owned());
    }

    emit_progress(app, "extract", "正在解压…", 0, 0);
    let (_new_exe, _txt) = extract(&zip_path, &tmp)?;

    emit_progress(app, "swap", "正在替换并重启…", 0, 0);
    swap_and_restart(&exe, &tmp)?;
    let _ = std::fs::remove_file(&zip_path);
    Ok(())
}

/// 启动时清理上一轮更新留下的 mini-card.old*.exe（旧进程刚退出的那会儿还锁着，删不掉就留着）
pub fn cleanup_old_exes() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.starts_with("mini-card.old") && name.ends_with(".exe") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
