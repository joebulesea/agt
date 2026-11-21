//! Agent 回報的用量（context 用量、訂閱額度），來源是 Claude Code 的 status line 資料。
//!
//! Claude Code 每次收到回覆等時機會執行 `statusLine` 指令，並從 stdin 傳入 JSON
//! （<https://code.claude.com/docs/en/statusline>）。agt 讓這個指令把 JSON 寫到 worktree 的
//! `.agt/usage.json`，daemon 讀取後以 [`parse`] 取出需要的欄位。
//!
//! 檔案是 Agent 那一側（可能是沙箱裡）寫的，視為不可信：只取數字欄位並限制範圍，
//! 字串去掉控制字元並限制長度，格式不對的欄位當成沒有。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `.agt/` 底下 status line 資料的檔名。
pub const USAGE_FILE: &str = "usage.json";
/// 讀取上限。
pub const USAGE_MAX: u64 = 64 * 1024;
/// 提醒門檻（百分比）：context 用量、5 小時與每週額度達到時顯示警告色並通知一次。
pub const USAGE_WARN_PERCENT: f64 = 80.0;

/// 分頁的用量。所有欄位都可能沒有（剛開始對話、`/compact` 之後、不是訂閱方案等）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Usage {
    /// 模型顯示名稱（例如 `Opus`）。
    #[serde(default)]
    pub model: Option<String>,
    /// context window 已用的百分比（0–100）。
    #[serde(default)]
    pub context_percent: Option<f64>,
    /// context 裡的 token 數（輸入，含快取）。
    #[serde(default)]
    pub context_tokens: Option<u64>,
    /// context window 大小（token）。
    #[serde(default)]
    pub context_size: Option<u64>,
    /// 訂閱方案的 5 小時額度。
    #[serde(default)]
    pub five_hour: Option<Quota>,
    /// 訂閱方案的每週額度。
    #[serde(default)]
    pub seven_day: Option<Quota>,
    /// daemon 讀到這份資料的時間（Unix 秒），用來挑出最新的額度資料。
    #[serde(default)]
    pub updated_at: i64,
}

/// 訂閱額度的一個時間窗。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    /// 已用的百分比（0–100）。
    pub used_percent: f64,
    /// 重置時間（Unix 秒）。
    pub resets_at: i64,
}

impl Usage {
    /// 是否什麼資料都沒有。
    pub fn is_empty(&self) -> bool {
        self.context_percent.is_none() && self.five_hour.is_none() && self.seven_day.is_none()
    }

    /// 顯示用的比較：百分比取整數後是否相同（避免小數變動造成大量推播）。
    pub fn same_display(&self, other: &Usage) -> bool {
        let r = |v: Option<f64>| v.map(|p| p.round() as i64);
        let q = |v: &Option<Quota>| v.map(|q| (q.used_percent.round() as i64, q.resets_at));
        self.model == other.model
            && r(self.context_percent) == r(other.context_percent)
            && self.context_size == other.context_size
            && q(&self.five_hour) == q(&other.five_hour)
            && q(&self.seven_day) == q(&other.seven_day)
    }
}

fn percent(v: &Value) -> Option<f64> {
    let p = v.as_f64()?;
    p.is_finite().then(|| p.clamp(0.0, 100.0))
}

fn tokens(v: &Value) -> Option<u64> {
    // 上限 1 億：遠大於任何 context window，只是防止離譜的值
    v.as_u64().filter(|n| *n <= 100_000_000)
}

fn quota(v: &Value) -> Option<Quota> {
    let used_percent = percent(&v["used_percentage"])?;
    let resets_at = v["resets_at"].as_i64().filter(|t| *t > 0)?;
    Some(Quota {
        used_percent,
        resets_at,
    })
}

/// 解析 Claude Code 的 status line JSON。不是 JSON 物件時回傳 `None`。
/// `now` 是目前時間（Unix 秒），寫進 `updated_at`。
pub fn parse(text: &str, now: i64) -> Option<Usage> {
    let v: Value = serde_json::from_str(text).ok()?;
    if !v.is_object() {
        return None;
    }
    let cw = &v["context_window"];
    let input = &cw["current_usage"];
    // used_percentage 只算輸入（含快取讀寫）；沒有時自己由 current_usage 算
    let context_tokens = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .map(|k| tokens(&input[*k]))
    .try_fold(0u64, |acc, n| n.map(|n| acc + n))
    .filter(|n| *n > 0);
    let context_size = tokens(&cw["context_window_size"]).filter(|n| *n > 0);
    let context_percent = percent(&cw["used_percentage"]).or_else(|| {
        let (t, s) = (context_tokens?, context_size?);
        Some((t as f64 / s as f64 * 100.0).clamp(0.0, 100.0))
    });
    let model = v["model"]["display_name"]
        .as_str()
        .map(crate::text::printable_line)
        .map(|s| s.trim().chars().take(40).collect::<String>())
        .filter(|s| !s.is_empty());
    let rl = &v["rate_limits"];
    Some(Usage {
        model,
        context_percent,
        context_tokens,
        context_size,
        five_hour: quota(&rl["five_hour"]),
        seven_day: quota(&rl["seven_day"]),
        updated_at: now,
    })
}

/// 一行摘要「模型 · ctx 45% · 5h 23% · 7d 41%」（沒有的部分略過），顯示在 Claude Code 的 status line。
pub fn summary(u: &Usage) -> String {
    let mut out: Vec<String> = vec![];
    if let Some(m) = &u.model {
        out.push(m.clone());
    }
    if let Some(p) = u.context_percent {
        out.push(format!("ctx {}%", p.round()));
    }
    for (name, q) in [("5h", u.five_hour), ("7d", u.seven_day)] {
        if let Some(q) = q {
            out.push(format!("{name} {}%", q.used_percent.round()));
        }
    }
    out.join(" · ")
}

/// 距離 `resets_at` 的時間，例如 `35m`、`2h15m`、`3d4h`。已經過了回傳 `None`。
pub fn format_remaining(resets_at: i64, now: i64) -> Option<String> {
    let s = resets_at - now;
    if s <= 0 {
        return None;
    }
    let (d, h, m) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60);
    Some(if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{}m", m.max(1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "model": {"id": "claude-opus-5-5", "display_name": "Opus"},
      "context_window": {
        "total_input_tokens": 15500, "context_window_size": 200000,
        "used_percentage": 8, "remaining_percentage": 92,
        "current_usage": {"input_tokens": 8500, "output_tokens": 1200,
          "cache_creation_input_tokens": 5000, "cache_read_input_tokens": 2000}
      },
      "rate_limits": {
        "five_hour": {"used_percentage": 23.5, "resets_at": 1738425600},
        "seven_day": {"used_percentage": 41.2, "resets_at": 1738857600}
      }
    }"#;

    #[test]
    fn parses_context_and_rate_limits() {
        let u = parse(SAMPLE, 100).unwrap();
        assert_eq!(u.model.as_deref(), Some("Opus"));
        assert_eq!(u.context_percent, Some(8.0));
        assert_eq!(u.context_tokens, Some(15500));
        assert_eq!(u.context_size, Some(200000));
        assert_eq!(
            u.five_hour,
            Some(Quota {
                used_percent: 23.5,
                resets_at: 1738425600
            })
        );
        assert_eq!(u.seven_day.unwrap().used_percent, 41.2);
        assert_eq!(u.updated_at, 100);
    }

    #[test]
    fn missing_fields_are_none() {
        // 剛開始對話：used_percentage 是 null、沒有 current_usage；API key 使用者沒有 rate_limits
        let u = parse(
            r#"{"context_window": {"used_percentage": null, "current_usage": null, "context_window_size": 200000}}"#,
            0,
        )
        .unwrap();
        assert!(u.is_empty());
        assert_eq!(u.context_size, Some(200000));
        // 沒有 used_percentage 時由 current_usage 計算
        let u = parse(
            r#"{"context_window": {"context_window_size": 1000, "current_usage": {"input_tokens": 100, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 150}}}"#,
            0,
        )
        .unwrap();
        assert_eq!(u.context_percent, Some(25.0));
        // 只有其中一個時間窗
        let u = parse(
            r#"{"rate_limits": {"seven_day": {"used_percentage": 5, "resets_at": 9}}}"#,
            0,
        )
        .unwrap();
        assert!(u.five_hour.is_none() && u.seven_day.is_some());
    }

    #[test]
    fn untrusted_values_are_clamped_or_dropped() {
        let u = parse(
            r#"{
              "model": {"display_name": "\u001b[31mEvil\u0007 name that is far too long to show in a status bar"},
              "context_window": {"used_percentage": 1e9, "context_window_size": -5},
              "rate_limits": {
                "five_hour": {"used_percentage": -3, "resets_at": 10},
                "seven_day": {"used_percentage": "50", "resets_at": 10}
              }
            }"#,
            0,
        )
        .unwrap();
        let m = u.model.unwrap();
        assert!(!m.contains('\u{1b}') && !m.contains('\u{7}'), "{m:?}");
        assert!(m.chars().count() <= 40);
        assert_eq!(u.context_percent, Some(100.0));
        assert_eq!(u.context_size, None);
        assert_eq!(u.five_hour.unwrap().used_percent, 0.0);
        assert!(u.seven_day.is_none()); // 字串不是數字
        assert!(parse("not json", 0).is_none());
        assert!(parse("[1,2]", 0).is_none());
    }

    #[test]
    fn same_display_ignores_small_changes_and_time() {
        let a = parse(SAMPLE, 1).unwrap();
        let mut b = parse(SAMPLE, 2).unwrap();
        b.context_percent = Some(8.3);
        assert!(a.same_display(&b));
        b.context_percent = Some(9.0);
        assert!(!a.same_display(&b));
    }

    #[test]
    fn summary_line() {
        assert_eq!(
            summary(&parse(SAMPLE, 0).unwrap()),
            "Opus · ctx 8% · 5h 24% · 7d 41%"
        );
        assert_eq!(summary(&Usage::default()), "");
    }

    #[test]
    fn remaining_time_format() {
        assert_eq!(format_remaining(100, 100), None);
        assert_eq!(format_remaining(130, 100), Some("1m".into()));
        assert_eq!(format_remaining(100 + 35 * 60, 100), Some("35m".into()));
        assert_eq!(
            format_remaining(100 + 2 * 3600 + 5 * 60, 100),
            Some("2h05m".into())
        );
        assert_eq!(
            format_remaining(100 + 3 * 86400 + 4 * 3600, 100),
            Some("3d4h".into())
        );
    }
}
