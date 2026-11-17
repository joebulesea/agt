//! 用系統的檔案管理員開啟目錄（Windows 檔案總管、macOS Finder、Linux 的 xdg-open）。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 兩次點擊間隔在這個時間內、而且位置相同，就算雙擊。
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// 這次點擊和上一次是否構成雙擊。
pub fn is_double_click(prev: Option<(u16, u16, Instant)>, x: u16, y: u16, now: Instant) -> bool {
    prev.is_some_and(|(px, py, at)| {
        px == x && py == y && now.saturating_duration_since(at) <= DOUBLE_CLICK
    })
}

/// 開啟目錄。路徑必須是已存在的絕對路徑：
/// 相對路徑或以 `/`、`-` 開頭的字串可能被檔案總管 / xdg-open 當成參數解讀。
pub fn open_dir(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if path.is_empty() || !p.is_absolute() {
        return Err(format!("不是絕對路徑：{path}"));
    }
    if !p.is_dir() {
        return Err(format!("目錄不存在：{path}"));
    }
    let program = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // 直接傳參數，不經過 shell
    let mut child = Command::new(program)
        .arg(p)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("無法執行 {program}：{e}"))?;
    // 不等它結束（不卡住 UI）；在背景回收行程。
    // explorer.exe 成功時的 exit code 也是 1，所以不看 exit code。
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_click_needs_same_cell_and_short_gap() {
        let t = Instant::now();
        assert!(!is_double_click(None, 1, 1, t));
        assert!(is_double_click(
            Some((5, 0, t)),
            5,
            0,
            t + Duration::from_millis(200)
        ));
        assert!(!is_double_click(
            Some((5, 0, t)),
            6,
            0,
            t + Duration::from_millis(200)
        ));
        assert!(!is_double_click(
            Some((5, 0, t)),
            5,
            0,
            t + Duration::from_millis(900)
        ));
    }

    #[test]
    fn rejects_relative_and_missing_paths() {
        assert!(open_dir("").is_err());
        assert!(open_dir("relative/dir").is_err());
        assert!(open_dir("-x").is_err());
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("nope");
        let err = open_dir(&missing.to_string_lossy()).unwrap_err();
        assert!(err.contains("不存在"), "{err}");
    }
}
