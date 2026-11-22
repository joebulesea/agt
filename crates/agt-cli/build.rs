//! 把建置編號併進 `agt --version`：測試版（nightly）的 workflow 會設定環境變數 `AGT_BUILD`
//! （格式 `nightly.<run>.<sha>`），正式版與本機編譯沒有，只顯示版號。

fn main() {
    println!("cargo:rerun-if-env-changed=AGT_BUILD");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let long = match std::env::var("AGT_BUILD") {
        Ok(b) if !b.is_empty() => format!("{version} ({b})"),
        _ => version,
    };
    println!("cargo:rustc-env=AGT_LONG_VERSION={long}");
}
