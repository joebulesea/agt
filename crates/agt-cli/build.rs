//! 1. 把建置編號併進 `agt --version`：測試版（nightly）的 workflow 會設定環境變數 `AGT_BUILD`
//!    （格式 `nightly.<run>.<sha>`），正式版與本機編譯沒有，只顯示版號。
//! 2. Windows：把版本資訊與 manifest 嵌進 agt.exe（檔案內容的「詳細資料」看得到；
//!    沒有這些資源、又沒有簽章的執行檔比較容易被防毒的機器學習判定誤判）。

fn main() {
    println!("cargo:rerun-if-env-changed=AGT_BUILD");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let build = std::env::var("AGT_BUILD").ok().filter(|b| !b.is_empty());
    let long = match &build {
        Some(b) => format!("{version} ({b})"),
        None => version,
    };
    println!("cargo:rustc-env=AGT_LONG_VERSION={long}");

    // 看目標平台，不看編譯的平台：在 Linux 交叉編譯 Windows 版時也要嵌入
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources(&long, build.as_deref());
    }
}

/// 失敗就讓建置失敗：悄悄略過的話，會發布出沒有版本資訊的執行檔。
fn windows_resources(long: &str, build: Option<&str>) {
    println!("cargo:rerun-if-changed=agt.manifest");
    let mut res = winresource::WindowsResource::new();
    // 版號的第四段是 nightly 的 run 編號（正式版與本機編譯是 0）；只有 16 位元，放不下就不放
    let run = build
        .and_then(|b| b.strip_prefix("nightly.")?.split('.').next()?.parse().ok())
        .unwrap_or(0u16);
    let part = |name: &str| -> u64 {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let number = part("CARGO_PKG_VERSION_MAJOR") << 48
        | part("CARGO_PKG_VERSION_MINOR") << 32
        | part("CARGO_PKG_VERSION_PATCH") << 16
        | u64::from(run);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, number)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, number)
        .set("FileVersion", long)
        .set("ProductVersion", long)
        .set("ProductName", "agt")
        .set("FileDescription", "agt - AI agent terminal multiplexer")
        .set("InternalName", "agt")
        .set("OriginalFilename", "agt.exe")
        .set_language(0x0409)
        .set_manifest_file("agt.manifest");
    if let Err(e) = res.compile_for("agt") {
        panic!(
            "無法把版本資訊嵌進 agt.exe：{e}（MSVC 需要 Windows SDK 的 rc.exe，mingw 需要 windres）"
        );
    }
}
