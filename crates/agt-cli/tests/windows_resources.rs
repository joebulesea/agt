//! Windows 版要有版本資訊與 manifest（build.rs 嵌入的）。
#![cfg(windows)]

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn exe_embeds_version_info_and_manifest() {
    let exe = std::fs::read(env!("CARGO_BIN_EXE_agt")).unwrap();
    // 版本資訊的字串是 UTF-16LE
    for s in ["VS_VERSION_INFO", "OriginalFilename", "agt.exe"] {
        let utf16: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert!(contains(&exe, &utf16), "沒有嵌入 {s}");
    }
    let manifest = include_bytes!("../agt.manifest");
    let level = br#"requestedExecutionLevel level="asInvoker""#;
    assert!(contains(manifest, level));
    assert!(contains(&exe, level), "沒有嵌入 manifest");
}
