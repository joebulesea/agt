//! 分頁標籤：寫法與正規化（daemon、CLI、TUI 共用）。

/// 一個分頁最多幾個標籤。
pub const MAX_TAGS: usize = 8;
/// 單一標籤的最大字元數。
pub const MAX_TAG_CHARS: usize = 24;

/// 把使用者輸入（以逗號或空白分隔，可帶開頭的 `#`）切成標籤清單，不做驗證。
pub fn split_input(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c == '，' || c.is_whitespace())
        .map(|t| t.trim().trim_start_matches('#').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// 驗證並正規化：去掉開頭的 `#`、略過空的、依大小寫不敏感去重（保留第一次出現的寫法與順序）。
/// 標籤不能含空白、逗號或控制字元，最長 [`MAX_TAG_CHARS`] 字元，最多 [`MAX_TAGS`] 個。
pub fn normalize(tags: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().trim_start_matches('#');
        if t.is_empty() {
            continue;
        }
        if t.chars().count() > MAX_TAG_CHARS {
            return Err(format!("標籤太長（最多 {MAX_TAG_CHARS} 個字元）：{t}"));
        }
        if t.chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == ',' || c == '，')
        {
            return Err(format!("標籤不能含空白、逗號或控制字元：{t:?}"));
        }
        if !out.iter().any(|o| o.to_lowercase() == t.to_lowercase()) {
            out.push(t.to_string());
        }
    }
    if out.len() > MAX_TAGS {
        return Err(format!("標籤最多 {MAX_TAGS} 個"));
    }
    Ok(out)
}

/// 資料庫的存放格式：以逗號連接（標籤本身不會含逗號）。
pub fn join(tags: &[String]) -> Option<String> {
    if tags.is_empty() {
        None
    } else {
        Some(tags.join(","))
    }
}

pub fn parse_stored(s: Option<&str>) -> Vec<String> {
    s.map(|s| {
        s.split(',')
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn splits_user_input() {
        assert_eq!(split_input("a, #b  c，d"), ["a", "b", "c", "d"]);
        assert!(split_input("  ,, # ").is_empty());
    }

    #[test]
    fn normalizes_dedupes_and_keeps_order() {
        assert_eq!(
            normalize(&v(&["#bug", "Bug", "ui", " "])).unwrap(),
            ["bug", "ui"]
        );
        // 大小寫不敏感（含非 ASCII），保留第一次出現的寫法
        assert_eq!(
            normalize(&v(&["中文", "ÄB", "äb"])).unwrap(),
            ["中文", "ÄB"]
        );
    }

    #[test]
    fn rejects_bad_tags() {
        assert!(normalize(&v(&["a b"])).unwrap_err().contains("空白"));
        assert!(normalize(&v(&["a,b"])).is_err());
        assert!(normalize(&v(&["a\u{1b}b"])).is_err());
        assert!(
            normalize(&v(&[&"x".repeat(25)]))
                .unwrap_err()
                .contains("太長")
        );
        assert!(normalize(&v(&[&"x".repeat(24)])).is_ok());
        let many: Vec<String> = (0..9).map(|i| format!("t{i}")).collect();
        assert!(normalize(&many).unwrap_err().contains("最多"));
        assert!(normalize(&many[..8]).is_ok());
    }

    #[test]
    fn storage_round_trip() {
        assert_eq!(join(&[]), None);
        assert_eq!(join(&v(&["a", "b"])).as_deref(), Some("a,b"));
        assert_eq!(parse_stored(Some("a,b")), ["a", "b"]);
        assert!(parse_stored(None).is_empty());
        assert!(parse_stored(Some("")).is_empty());
    }
}
