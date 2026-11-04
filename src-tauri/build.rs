use std::env;
use std::fs;
use std::path::PathBuf;

/// 发布包只有一个 `mini-card.exe`：把原生绘制进程和字体嵌进主 exe，
/// 首次运行解到 `%APPDATA%\mini-card\native\<版本>\`。
///
/// 为什么不用「zip 里放两个 exe」：0.1.1 的旧更新器只会抽 `mini-card.exe`，
/// 0.1.1 → 0.1.2 的自动更新会留下一个没有 `minicard-bin.exe` 的目录 → 九张卡全没。
/// 嵌进主 exe 后，旧更新器照常只换主 exe，新主 exe 自己把绘制进程落盘。
///
/// debug 构建不嵌（保持改 native 后 `cargo build` 立即生效）；
/// release 构建必须先把 `native/minicard-bin` 编出来，否则直接 panic。
fn main() {
    println!("cargo:rerun-if-changed=../web");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let native_dir = out_dir.join("native");
    fs::create_dir_all(&native_dir).ok();

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().expect("src-tauri 的父目录").to_path_buf();
    let release = env::var("PROFILE").unwrap_or_default() == "release";

    let bin = root.join("native/minicard-bin/target/release/minicard-bin.exe");
    let fonts = [
        ("FONT_REGULAR", root.join("native/assets/fonts/MiSans-Regular.ttf")),
        ("FONT_MEDIUM", root.join("native/assets/fonts/MiSans-Medium.ttf")),
        ("FONT_SEMIBOLD", root.join("native/assets/fonts/MiSans-Semibold.ttf")),
    ];

    let mut generated = String::new();

    if release {
        if !bin.exists() {
            panic!(
                "release 构建缺少原生绘制进程：{}\n先构建 native/minicard-bin：\n  cd native/minicard-bin && cargo build --release",
                bin.display()
            );
        }
        let missing: Vec<String> = fonts
            .iter()
            .filter(|(_, p)| !p.exists())
            .map(|(_, p)| p.display().to_string())
            .collect();
        if !missing.is_empty() {
            panic!(
                "release 构建缺少字体文件：{}\n先跑 native/assets/fonts/build_misans_ttf.py",
                missing.join(", ")
            );
        }

        let entries = std::iter::once(("NATIVE_BIN", bin))
            .chain(fonts.iter().map(|(n, p)| (*n, p.clone())));
        for (name, src) in entries {
            println!("cargo:rerun-if-changed={}", src.display());
            let dst = native_dir.join(src.file_name().expect("文件名"));
            fs::copy(&src, &dst).unwrap_or_else(|e| {
                panic!("复制 {} 到 OUT_DIR 失败：{e}", src.display());
            });
            let file = dst.file_name().unwrap().to_string_lossy();
            generated.push_str(&format!(
                "pub const {name}: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/native/{file}\"));\n"
            ));
        }
    } else {
        generated.push_str("pub const NATIVE_BIN: &[u8] = &[];\n");
        for (name, _) in &fonts {
            generated.push_str(&format!("pub const {name}: &[u8] = &[];\n"));
        }
    }

    fs::write(native_dir.join("assets.rs"), generated).expect("写 native/assets.rs");

    tauri_build::build()
}
