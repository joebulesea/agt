//! 顯示不可信文字前的過濾。
//!
//! diff 的檔案內容、Agent 回報的通知與要複製的文字都可能來自沙箱裡的 Agent。直接印到終端機時，
//! 其中的控制序列會被終端機執行（例如 OSC 52 改寫剪貼簿、改標題、移動游標覆蓋畫面）。

/// 把控制字元換成可見的符號；保留換行與 Tab。
///
/// - C0 控制字元（含 ESC、CR、BEL）→ 對應的 Unicode「控制圖形」（例如 ESC → `␛`）
/// - DEL 與 C1 控制字元（U+0080–U+009F，其中 U+009B 等同 `ESC [`）→ `�`
pub fn printable(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' | '\t' => out.push(c),
            '\u{0}'..='\u{1f}' => out.push(char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}')),
            '\u{7f}' | '\u{80}'..='\u{9f}' => out.push('\u{fffd}'),
            _ => out.push(c),
        }
    }
    out
}

/// 單行版本：換行與 Tab 也換掉，用於標題、通知等只能有一行的地方。
pub fn printable_line(s: &str) -> String {
    printable(s).replace(['\n', '\t'], " ")
}

/// 是否含有換行與 Tab 以外的控制字元。
pub fn has_controls(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{80}'..='\u{9f}') && c != '\n' && c != '\t'
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_are_neutralized() {
        let evil = "a\x1b]52;c;ZXZpbA==\x07b\r\nc\u{9b}2Jd\te";
        let p = printable(evil);
        assert!(!has_controls(&p), "{p:?}");
        assert_eq!(p, "a␛]52;c;ZXZpbA==␇b␍\nc�2Jd\te");
        assert_eq!(printable_line("x\ny\tz"), "x y z");
        assert!(has_controls("\x1b[2J"));
        assert!(!has_controls("中文\n\tok"));
    }
}
