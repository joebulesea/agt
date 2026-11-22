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

/// 側欄預覽的提問最多保留的字數。
pub const PROMPT_MAX_CHARS: usize = 200;

fn is_border(c: char) -> bool {
    ('\u{2500}'..='\u{257f}').contains(&c) // 框線字元
}

/// 清成單行：去掉控制字元、兩端的框線與空白，連續空白合併成一個。
fn clean_line(s: &str) -> String {
    let p = printable_line(s);
    let t = p.trim_matches(|c: char| c.is_whitespace() || is_border(c));
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 等待輸入時，Agent 在問什麼：優先用 hook 帶來的訊息；沒有時從畫面最後幾行挑最後一個
/// 含 `?` 或 `(y/n)` 的行（選單的問題通常在選項上面；`? for shortcuts` 這類以 `?` 開頭的提示列不算），都沒有就用最後一個不以 `?` 開頭的非空白行。
/// 結果已清除控制字元、限制在 [`PROMPT_MAX_CHARS`] 字以內；找不到內容時回傳 None。
pub fn waiting_prompt(message: Option<&str>, screen: &[String]) -> Option<String> {
    let cut = |s: String| s.chars().take(PROMPT_MAX_CHARS).collect::<String>();
    if let Some(m) = message.map(clean_line).filter(|m| !m.is_empty()) {
        return Some(cut(m));
    }
    let lines: Vec<String> = screen
        .iter()
        .map(|l| clean_line(l))
        .filter(|l| !l.is_empty())
        .collect();
    let pick = lines
        .iter()
        .rev()
        .find(|l| !l.starts_with('?') && (l.contains('?') || l.contains("(y/n)")))
        .or_else(|| lines.iter().rev().find(|l| !l.starts_with('?')))
        .or(lines.last())?;
    Some(cut(pick.clone()))
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

    #[test]
    fn waiting_prompt_prefers_message_then_question_line() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // hook 訊息優先，並清掉控制字元
        assert_eq!(
            waiting_prompt(Some("Claude needs\x1b[2J your permission"), &v(&["x?"])).as_deref(),
            Some("Claude needs␛[2J your permission")
        );
        // 沒有訊息：選單上方的問題
        let menu = v(&[
            "╭──────────────╮",
            "│ Do you want to run this command?   │",
            "│ ❯ 1. Yes                           │",
            "│   3. No, and tell Copilot (Esc)    │",
            "╰──────────────╯",
            "",
        ]);
        assert_eq!(
            waiting_prompt(None, &menu).as_deref(),
            Some("Do you want to run this command?")
        );
        // 沒有問號：最後一個非空白行
        assert_eq!(
            waiting_prompt(Some("  "), &v(&["a", "  ", "Press Enter", ""])).as_deref(),
            Some("Press Enter")
        );
        // Claude Code 輸入框下方的「? for shortcuts」不是提問
        assert_eq!(
            waiting_prompt(None, &v(&["Done. Anything else", "? for shortcuts"])).as_deref(),
            Some("Done. Anything else")
        );
        assert_eq!(
            waiting_prompt(None, &v(&["Apply the patch?", "? for shortcuts"])).as_deref(),
            Some("Apply the patch?")
        );
        // 空白畫面
        assert_eq!(waiting_prompt(None, &v(&["", "───"])), None);
        // 長度限制（以字計）
        let long = "問".repeat(500);
        assert_eq!(
            waiting_prompt(Some(&long), &[]).unwrap().chars().count(),
            PROMPT_MAX_CHARS
        );
    }
}
