//! `agt stats`：從資料庫的狀態事件統計每個分頁花在各種狀態的時間。
//!
//! 只看時間：目前沒有取得 token 或費用的來源，所以不統計那些。資料來自 daemon 記錄的 `status` 事件，
//! 關閉超過 30 天的分頁會被清除，所以最多只能回溯 30 天。

use agt_store::{SessionRow, Store};
use anyhow::Context;
use std::collections::BTreeMap;

/// 一個分頁在各狀態停留的秒數（鍵是 `AgentStatus::as_str()`，例如 `running`）。
///
/// 起點是建立時間（狀態 `starting`），終點是關閉時間，尚未關閉的算到 `now`。
/// `events` 是 `(時間, 種類, JSON 資料)`，只看 `status` 事件的 `to`；其他種類與壞掉的資料略過。
pub fn durations(
    created_at: i64,
    ended_at: Option<i64>,
    now: i64,
    events: &[(i64, String, Option<String>)],
) -> BTreeMap<String, i64> {
    let end = ended_at.unwrap_or(now).max(created_at);
    let mut out: BTreeMap<String, i64> = BTreeMap::new();
    let mut state = "starting".to_string();
    let mut since = created_at;
    let mut changes: Vec<(i64, String)> = events
        .iter()
        .filter(|(_, kind, _)| kind == "status")
        .filter_map(|(at, _, data)| {
            let v: serde_json::Value = serde_json::from_str(data.as_deref()?).ok()?;
            Some((*at, v.get("to")?.as_str()?.to_string()))
        })
        .collect();
    changes.sort_by_key(|(at, _)| *at);
    for (at, to) in changes {
        let at = at.clamp(since, end);
        *out.entry(state.clone()).or_default() += at - since;
        state = to;
        since = at;
    }
    *out.entry(state).or_default() += end - since;
    out.retain(|_, v| *v > 0);
    out
}

/// 「忙碌」＝執行中＋思考中。
pub fn busy_secs(by_status: &BTreeMap<String, i64>) -> i64 {
    ["running", "thinking"]
        .iter()
        .map(|k| by_status.get(*k).copied().unwrap_or(0))
        .sum()
}

pub fn waiting_secs(by_status: &BTreeMap<String, i64>) -> i64 {
    by_status.get("waiting").copied().unwrap_or(0)
}

/// `1h 05m`、`12m 30s`、`45s`。
pub fn fmt_secs(s: i64) -> String {
    let s = s.max(0);
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

pub struct SessionStats {
    pub row: SessionRow,
    pub by_status: BTreeMap<String, i64>,
    pub total: i64,
}

#[derive(Default, Debug, PartialEq)]
pub struct AgentTotals {
    pub sessions: usize,
    pub total: i64,
    pub busy: i64,
    pub waiting: i64,
}

pub fn by_agent(list: &[SessionStats]) -> BTreeMap<String, AgentTotals> {
    let mut m: BTreeMap<String, AgentTotals> = BTreeMap::new();
    for s in list {
        let t = m.entry(s.row.agent.clone()).or_default();
        t.sessions += 1;
        t.total += s.total;
        t.busy += busy_secs(&s.by_status);
        t.waiting += waiting_secs(&s.by_status);
    }
    m
}

pub fn to_json(days: u32, list: &[SessionStats]) -> serde_json::Value {
    let sessions: Vec<_> = list
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.row.id,
                "short_id": s.row.short_id,
                "agent": s.row.agent,
                "task": s.row.task,
                "created_at": s.row.created_at,
                "ended_at": s.row.ended_at,
                "total_secs": s.total,
                "busy_secs": busy_secs(&s.by_status),
                "waiting_secs": waiting_secs(&s.by_status),
                "by_status": s.by_status,
            })
        })
        .collect();
    let agents: serde_json::Map<String, serde_json::Value> = by_agent(list)
        .into_iter()
        .map(|(k, t)| {
            (
                k,
                serde_json::json!({
                    "sessions": t.sessions,
                    "total_secs": t.total,
                    "busy_secs": t.busy,
                    "waiting_secs": t.waiting,
                }),
            )
        })
        .collect();
    serde_json::json!({"days": days, "sessions": sessions, "by_agent": agents})
}

fn clip(s: &str, max: usize) -> String {
    let s = agt_proto::text::printable_line(s);
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

pub fn to_text(days: u32, list: &[SessionStats]) -> String {
    if list.is_empty() {
        return format!("最近 {days} 天沒有分頁紀錄\n");
    }
    let mut out = format!(
        "最近 {days} 天的分頁（忙碌 = 執行中＋思考中；沒有 token／費用資料）\n\n{:<8} {:<14} {:>9} {:>9} {:>9}  {}\n",
        "分頁", "Agent", "總時間", "忙碌", "等待輸入", "任務"
    );
    for s in list {
        out += &format!(
            "{:<8} {:<14} {:>9} {:>9} {:>9}  {}\n",
            s.row.short_id,
            clip(&s.row.agent, 14),
            fmt_secs(s.total),
            fmt_secs(busy_secs(&s.by_status)),
            fmt_secs(waiting_secs(&s.by_status)),
            clip(&s.row.task, 40),
        );
    }
    out += "\n依 Agent 合計\n";
    for (agent, t) in by_agent(list) {
        out += &format!(
            "  {:<14} {} 個分頁，總時間 {}，忙碌 {}，等待輸入 {}\n",
            clip(&agent, 14),
            t.sessions,
            fmt_secs(t.total),
            fmt_secs(t.busy),
            fmt_secs(t.waiting),
        );
    }
    out
}

/// 讀資料庫，回傳最近 `days` 天內建立的分頁統計。資料庫不存在時回傳空清單。
pub fn collect(days: u32) -> anyhow::Result<Vec<SessionStats>> {
    let db = agt_daemon::db_path();
    if !db.is_file() {
        return Ok(Vec::new());
    }
    let store = Store::open(&db).with_context(|| format!("無法開啟資料庫 {}", db.display()))?;
    let now = agt_store::now();
    let since = now - i64::from(days) * 86_400;
    let mut out = Vec::new();
    for row in store.all_sessions()? {
        if row.created_at < since {
            continue;
        }
        let events = store.events(&row.id)?;
        let by_status = durations(row.created_at, row.ended_at, now, &events);
        let total = by_status.values().sum();
        out.push(SessionStats {
            row,
            by_status,
            total,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(at: i64, to: &str) -> (i64, String, Option<String>) {
        (
            at,
            "status".into(),
            Some(serde_json::json!({"from": "x", "to": to}).to_string()),
        )
    }

    #[test]
    fn sums_time_per_status() {
        let ev = [st(110, "running"), st(160, "waiting"), st(200, "running")];
        let d = durations(100, Some(300), 9999, &ev);
        assert_eq!(d["starting"], 10);
        assert_eq!(d["running"], 50 + 100);
        assert_eq!(d["waiting"], 40);
        assert_eq!(d.values().sum::<i64>(), 200);
        assert_eq!(busy_secs(&d), 150);
        assert_eq!(waiting_secs(&d), 40);
    }

    #[test]
    fn open_sessions_count_until_now() {
        let d = durations(100, None, 400, &[st(150, "thinking")]);
        assert_eq!(d["starting"], 50);
        assert_eq!(d["thinking"], 250);
    }

    #[test]
    fn ignores_other_kinds_bad_data_and_out_of_range_times() {
        let ev = [
            (120, "notify".to_string(), Some("{}".to_string())),
            (130, "status".to_string(), Some("not json".to_string())),
            (140, "status".to_string(), None),
            (150, "status".to_string(), Some(r#"{"x":1}"#.to_string())),
            st(50, "running"),   // 比建立時間早：算在建立時間
            st(9000, "waiting"), // 比結束時間晚：算在結束時間
        ];
        let d = durations(100, Some(200), 1000, &ev);
        assert_eq!(d.values().sum::<i64>(), 100);
        assert_eq!(d["running"], 100);
        assert!(!d.contains_key("waiting"), "{d:?}");
    }

    #[test]
    fn unsorted_events_and_empty() {
        let ev = [st(200, "waiting"), st(150, "running")];
        let d = durations(100, Some(300), 0, &ev);
        assert_eq!((d["starting"], d["running"], d["waiting"]), (50, 50, 100));
        let d = durations(100, Some(100), 0, &[]);
        assert!(d.is_empty());
        // 結束時間比建立時間早（資料異常）：不產生負數
        assert!(durations(100, Some(50), 0, &[]).is_empty());
    }

    #[test]
    fn formats_durations() {
        assert_eq!(fmt_secs(45), "45s");
        assert_eq!(fmt_secs(750), "12m 30s");
        assert_eq!(fmt_secs(3900), "1h 05m");
        assert_eq!(fmt_secs(-5), "0s");
    }

    fn sess(agent: &str, short: &str, by: &[(&str, i64)]) -> SessionStats {
        let by_status: BTreeMap<String, i64> =
            by.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let total = by_status.values().sum();
        SessionStats {
            row: SessionRow {
                id: format!("id-{short}"),
                short_id: short.into(),
                agent: agent.into(),
                task: "任務\n含換行".into(),
                created_at: 1,
                ..Default::default()
            },
            by_status,
            total,
        }
    }

    #[test]
    fn aggregates_by_agent() {
        let list = [
            sess("claude", "a1", &[("running", 60), ("waiting", 30)]),
            sess("claude", "a2", &[("thinking", 40), ("done", 10)]),
            sess("codex", "b1", &[("running", 5)]),
        ];
        let m = by_agent(&list);
        assert_eq!(
            m["claude"],
            AgentTotals {
                sessions: 2,
                total: 140,
                busy: 100,
                waiting: 30
            }
        );
        assert_eq!(m["codex"].sessions, 1);
    }

    #[test]
    fn json_shape_and_text_output() {
        let list = [sess("claude", "a1", &[("running", 60), ("waiting", 30)])];
        let v = to_json(7, &list);
        assert_eq!(v["days"], 7);
        assert_eq!(v["sessions"][0]["short_id"], "a1");
        assert_eq!(v["sessions"][0]["busy_secs"], 60);
        assert_eq!(v["sessions"][0]["by_status"]["waiting"], 30);
        assert_eq!(v["by_agent"]["claude"]["total_secs"], 90);
        let text = to_text(7, &list);
        assert!(text.contains("a1") && text.contains("1m 30s"), "{text}");
        // 任務中的換行不會破壞表格
        assert_eq!(
            text.lines().filter(|l| l.starts_with("a1")).count(),
            1,
            "{text}"
        );
        assert!(to_text(7, &[]).contains("沒有分頁紀錄"));
    }
}
