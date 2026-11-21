//! 「連續忙碌」的計時：分頁持續處於執行中／思考中超過設定的時間，就提醒一次。

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct BusyTracker {
    /// 分頁 id → (開始忙碌的時間, 是否已提醒)
    map: HashMap<String, (Instant, bool)>,
}

impl BusyTracker {
    /// 分頁進入忙碌狀態。已經在忙碌時（執行中 ↔ 思考中）保留原本的開始時間。
    pub fn enter(&mut self, id: &str, now: Instant) {
        self.map.entry(id.to_string()).or_insert((now, false));
    }

    /// 分頁離開忙碌狀態（等待輸入、完成、結束…）或被關閉：之後再忙碌會重新計時、重新提醒。
    pub fn leave(&mut self, id: &str) {
        self.map.remove(id);
    }

    /// 取出已超過 `secs` 秒、還沒提醒過的分頁並標記為已提醒；`secs == 0` 代表關閉。
    pub fn take_due(&mut self, now: Instant, secs: u64) -> Vec<String> {
        if secs == 0 {
            return Vec::new();
        }
        let limit = Duration::from_secs(secs);
        let mut due: Vec<String> = Vec::new();
        for (id, (since, notified)) in self.map.iter_mut() {
            if !*notified && now.duration_since(*since) >= limit {
                *notified = true;
                due.push(id.clone());
            }
        }
        due.sort();
        due
    }
}

/// 提醒訊息中的時間長度，例如 `90 秒`、`5 分鐘`、`1 小時 30 分鐘`。
pub fn format_secs(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs} 秒");
    }
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    match (h, m) {
        (0, m) => format!("{m} 分鐘"),
        (h, 0) => format!("{h} 小時"),
        (h, m) => format!("{h} 小時 {m} 分鐘"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn notifies_once_after_the_threshold() {
        let t0 = Instant::now();
        let mut b = BusyTracker::default();
        b.enter("a", t0);
        assert!(b.take_due(at(t0, 299), 300).is_empty());
        assert_eq!(b.take_due(at(t0, 300), 300), ["a"]);
        // 同一段忙碌期間不再重複提醒
        assert!(b.take_due(at(t0, 900), 300).is_empty());
    }

    #[test]
    fn switching_between_running_and_thinking_keeps_the_clock() {
        let t0 = Instant::now();
        let mut b = BusyTracker::default();
        b.enter("a", t0);
        b.enter("a", at(t0, 200)); // Running → Thinking
        assert_eq!(b.take_due(at(t0, 300), 300), ["a"]);
    }

    #[test]
    fn leaving_resets_and_allows_a_new_reminder() {
        let t0 = Instant::now();
        let mut b = BusyTracker::default();
        b.enter("a", t0);
        assert_eq!(b.take_due(at(t0, 300), 300), ["a"]);
        b.leave("a"); // 等待輸入
        b.enter("a", at(t0, 400)); // 又開始忙碌
        assert!(b.take_due(at(t0, 600), 300).is_empty());
        assert_eq!(b.take_due(at(t0, 700), 300), ["a"]);
    }

    #[test]
    fn zero_disables_and_sessions_are_independent() {
        let t0 = Instant::now();
        let mut b = BusyTracker::default();
        b.enter("a", t0);
        b.enter("b", at(t0, 100));
        assert!(b.take_due(at(t0, 10_000), 0).is_empty());
        assert_eq!(b.take_due(at(t0, 300), 300), ["a"]);
        assert_eq!(b.take_due(at(t0, 400), 300), ["b"]);
        b.leave("unknown"); // 不存在也不會出錯
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_secs(45), "45 秒");
        assert_eq!(format_secs(60), "1 分鐘");
        assert_eq!(format_secs(300), "5 分鐘");
        assert_eq!(format_secs(3600), "1 小時");
        assert_eq!(format_secs(5400), "1 小時 30 分鐘");
    }
}
