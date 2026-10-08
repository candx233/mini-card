fn main() {
    // 前端资源变更也要触发重编译（卡片窗口的 HTML/JS 嵌在二进制里）
    println!("cargo:rerun-if-changed=../web");
    tauri_build::build()
}
