//! 設定檔 `[colors]` 的顏色寫法與可覆寫的名稱（daemon 驗證、TUI 套用共用）。

use crate::screen::Color;

/// 可以覆寫的顏色名稱，對應 TUI 主題的欄位。
pub const COLOR_KEYS: [&str; 22] = [
    "cyan",
    "yellow",
    "green",
    "red",
    "blue",
    "magenta",
    "gray",
    "dark_gray",
    "text",
    "on_accent",
    "sel_bg",
    "bar_bg",
    "tab_on_bg",
    "tab_off_fg",
    "tab_off_bg",
    // 個別狀態的顏色；沒設定時沿用上面對應的角色色（running = green…）
    "status_running",
    "status_thinking",
    "status_waiting",
    "status_done",
    "status_error",
    "status_starting",
    "status_idle",
];

/// 解析顏色：`#rrggbb`、`0`–`255`（256 色索引）、ANSI 名稱
/// （black red green yellow blue magenta cyan gray darkgray white，大小寫不拘），
/// 或 `default`（終端機預設色）。
pub fn parse_color(s: &str) -> Result<Color, String> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix('#') {
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let v = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
            return Ok(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
        return Err(format!("{s:?} 不是 #rrggbb"));
    }
    if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
        return t
            .parse::<u8>()
            .map(Color::Idx)
            .map_err(|_| format!("{s:?} 超出 0–255"));
    }
    let idx = match t.to_ascii_lowercase().replace(['_', '-'], "").as_str() {
        "default" | "reset" => return Ok(Color::Default),
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "gray" | "grey" => 7,
        "darkgray" | "darkgrey" => 8,
        "white" => 15,
        _ => {
            return Err(format!(
                "{s:?} 不是顏色（可用 #rrggbb、0–255、black/red/green/yellow/blue/magenta/cyan/gray/darkgray/white、default）"
            ));
        }
    };
    Ok(Color::Idx(idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_forms() {
        assert_eq!(parse_color("#ff8000"), Ok(Color::Rgb(255, 128, 0)));
        assert_eq!(parse_color(" 208 "), Ok(Color::Idx(208)));
        assert_eq!(parse_color("Red"), Ok(Color::Idx(1)));
        assert_eq!(parse_color("dark_gray"), Ok(Color::Idx(8)));
        assert_eq!(parse_color("default"), Ok(Color::Default));
    }

    #[test]
    fn rejects_bad_values() {
        for s in ["", "#fff", "#gggggg", "256", "-1", "purple", "#ff80001"] {
            assert!(parse_color(s).is_err(), "{s:?}");
        }
    }
}
