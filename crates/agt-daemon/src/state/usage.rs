//! 分頁的用量（context、訂閱額度）：讀取 Claude Code 的 status line 寫到 worktree 的 `.agt/usage.json`。

use super::*;
use agt_proto::usage::{USAGE_FILE, USAGE_MAX, USAGE_WARN_PERCENT, Usage, format_remaining};
use std::collections::HashSet;

/// 已經通知過的用量提醒。
#[derive(Debug, Default)]
pub struct AlertState {
    /// context 已達門檻並通知過的分頁；降回門檻以下（例如 `/compact` 之後）時移除，之後可再通知。
    context: HashSet<String>,
    /// 已通知過的額度時間窗：(`5h` / `7d`, 重置時間)。額度是整個帳號共用的，
    /// 同一個時間窗不管從幾個分頁回報都只通知一次。
    quota: HashSet<(&'static str, i64)>,
}

/// 一則要發出的用量提醒。
#[derive(Debug, Clone, PartialEq)]
pub enum Alert {
    Context {
        percent: f64,
    },
    Quota {
        window: &'static str,
        percent: f64,
        resets_at: i64,
    },
}

/// 依新的用量決定要發哪些提醒，並更新紀錄。`watched`：使用者正在看這個分頁
/// （狀態列已經顯示紅色，不另外通知 context；額度是全帳號的，照樣通知）。
pub fn due_alerts(
    state: &mut AlertState,
    id: &str,
    usage: &Usage,
    watched: bool,
    now: i64,
) -> Vec<Alert> {
    let mut out = vec![];
    match usage.context_percent {
        Some(p) if p >= USAGE_WARN_PERCENT => {
            if !watched && state.context.insert(id.to_string()) {
                out.push(Alert::Context { percent: p });
            }
        }
        // 降回門檻以下：下次再超過時重新通知。沒有資料（剛 /compact）時不動
        Some(_) => {
            state.context.remove(id);
        }
        None => {}
    }
    state.quota.retain(|(_, resets_at)| *resets_at > now);
    for (window, q) in [("5h", usage.five_hour), ("7d", usage.seven_day)] {
        let Some(q) = q else { continue };
        if q.used_percent >= USAGE_WARN_PERCENT
            && q.resets_at > now
            && state.quota.insert((window, q.resets_at))
        {
            out.push(Alert::Quota {
                window,
                percent: q.used_percent,
                resets_at: q.resets_at,
            });
        }
    }
    out
}

impl Daemon {
    /// 讀取各分頁的 `.agt/usage.json`，數值有變化時推播 `SessionUpdated`。
    /// 只有檔案修改時間變了才讀；檔案是 Agent 那一側寫的，讀取時不跟隨連結、限制大小（見 `agt_proto::usage`）。
    pub fn poll_usage(&self) {
        let targets: Vec<(String, String)> = self
            .entries
            .lock_or_recover()
            .values()
            .filter(|e| !e.info.cwd.is_empty())
            .map(|e| (e.info.id.clone(), e.info.cwd.clone()))
            .collect();
        self.usage_seen
            .lock_or_recover()
            .retain(|id, _| targets.iter().any(|(t, _)| t == id));
        for (id, cwd) in targets {
            let Some(usage) = self.read_usage(&id, Path::new(&cwd)) else {
                continue;
            };
            let (changed, info, watched) = {
                let mut entries = self.entries.lock_or_recover();
                let Some(e) = entries.get_mut(&id) else {
                    continue;
                };
                let changed = !e
                    .info
                    .usage
                    .as_ref()
                    .is_some_and(|old| old.same_display(&usage));
                e.info.usage = Some(usage.clone());
                let watched = e.session.as_ref().is_some_and(|s| s.viewers() > 0);
                (changed, e.info.clone(), watched)
            };
            self.alert_usage(&info, &usage, watched);
            if changed {
                self.publish(info);
            }
        }
        // 已關閉的分頁不再保留 context 提醒紀錄
        let ids: Vec<String> = self.entries.lock_or_recover().keys().cloned().collect();
        self.usage_alerts
            .lock_or_recover()
            .context
            .retain(|id| ids.contains(id));
    }

    /// 用量達到 80% 時發系統通知（`notify = false` 時不發），並記在分頁的事件記錄。
    fn alert_usage(&self, info: &SessionInfo, usage: &Usage, watched: bool) {
        let now = agt_store::now();
        let alerts = due_alerts(
            &mut self.usage_alerts.lock_or_recover(),
            &info.id,
            usage,
            watched,
            now,
        );
        let notify = self.cfg().general.notify;
        for a in alerts {
            let (title, body, data) = match &a {
                Alert::Context { percent } => (
                    format!("agt · {}", info.task),
                    format!(
                        "{} 的 context 已用 {}%，快滿時 Claude Code 會自動壓縮對話（可以先 /compact 或收尾）",
                        info.agent,
                        percent.round()
                    ),
                    json!({"kind": "context", "percent": percent}),
                ),
                Alert::Quota {
                    window,
                    percent,
                    resets_at,
                } => {
                    let name = if *window == "5h" {
                        "5 小時"
                    } else {
                        "每週"
                    };
                    let rem = format_remaining(*resets_at, now).unwrap_or_default();
                    (
                        "agt · Claude 訂閱額度".to_string(),
                        format!("{name}額度已用 {}%，{rem} 後重置", percent.round()),
                        json!({"kind": "quota", "window": window, "percent": percent, "resets_at": resets_at}),
                    )
                }
            };
            tracing::info!(id = %info.id, "用量提醒：{body}");
            self.record(&info.id, "usage_alert", data);
            if notify {
                platform::notify(&title, &body, Some(info.id.as_str()));
            }
        }
    }

    /// 修改時間和上次不同時讀取並解析；沒有檔案、沒有變化或格式不對時回傳 `None`。
    fn read_usage(&self, id: &str, cwd: &Path) -> Option<Usage> {
        let path = cwd.join(agt_git::NOTES_DIR).join(USAGE_FILE);
        let m = std::fs::symlink_metadata(&path).ok()?;
        if m.file_type().is_symlink() || !m.is_file() || m.len() > USAGE_MAX {
            return None;
        }
        let mtime = m.modified().ok()?;
        {
            let mut seen = self.usage_seen.lock_or_recover();
            if seen.get(id) == Some(&mtime) {
                return None;
            }
            seen.insert(id.to_string(), mtime);
        }
        // read_note：不跟隨連結、上限 64KB、必須是 UTF-8
        let text = agt_git::read_note(cwd, USAGE_FILE).ok()??;
        agt_proto::usage::parse(&text, agt_store::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agt_proto::usage::Quota;

    fn usage(ctx: Option<f64>, five: Option<(f64, i64)>, week: Option<(f64, i64)>) -> Usage {
        let q = |v: Option<(f64, i64)>| {
            v.map(|(used_percent, resets_at)| Quota {
                used_percent,
                resets_at,
            })
        };
        Usage {
            context_percent: ctx,
            five_hour: q(five),
            seven_day: q(week),
            ..Default::default()
        }
    }

    #[test]
    fn context_alert_once_per_tab_and_rearms_below_threshold() {
        let mut s = AlertState::default();
        assert!(due_alerts(&mut s, "a", &usage(Some(79.9), None, None), false, 0).is_empty());
        assert_eq!(
            due_alerts(&mut s, "a", &usage(Some(80.0), None, None), false, 0),
            [Alert::Context { percent: 80.0 }]
        );
        // 同一個分頁不重複；別的分頁各自通知
        assert!(due_alerts(&mut s, "a", &usage(Some(95.0), None, None), false, 0).is_empty());
        assert_eq!(
            due_alerts(&mut s, "b", &usage(Some(81.0), None, None), false, 0).len(),
            1
        );
        // 沒有資料（剛 /compact）：不重新開啟
        assert!(due_alerts(&mut s, "a", &usage(None, None, None), false, 0).is_empty());
        assert!(due_alerts(&mut s, "a", &usage(Some(90.0), None, None), false, 0).is_empty());
        // 降回門檻以下後再超過：再通知一次
        due_alerts(&mut s, "a", &usage(Some(20.0), None, None), false, 0);
        assert_eq!(
            due_alerts(&mut s, "a", &usage(Some(85.0), None, None), false, 0).len(),
            1
        );
        // 正在看這個分頁：不通知 context（之後切走仍會通知）
        due_alerts(&mut s, "c", &usage(Some(90.0), None, None), true, 0);
        assert_eq!(
            due_alerts(&mut s, "c", &usage(Some(90.0), None, None), false, 0).len(),
            1
        );
    }

    #[test]
    fn quota_alert_once_per_window_across_tabs() {
        let mut s = AlertState::default();
        let five = Some((82.0, 1000));
        assert_eq!(
            due_alerts(
                &mut s,
                "a",
                &usage(None, five, Some((50.0, 9000))),
                false,
                0
            ),
            [Alert::Quota {
                window: "5h",
                percent: 82.0,
                resets_at: 1000
            }]
        );
        // 別的分頁回報同一個時間窗：不重複；正在看也照樣通知（額度是全帳號的）
        assert!(
            due_alerts(&mut s, "b", &usage(None, Some((90.0, 1000)), None), true, 0).is_empty()
        );
        // 每週額度到了門檻
        assert_eq!(
            due_alerts(&mut s, "b", &usage(None, None, Some((80.0, 9000))), true, 0).len(),
            1
        );
        // 5 小時額度重置後的新時間窗：再通知
        assert_eq!(
            due_alerts(
                &mut s,
                "a",
                &usage(None, Some((85.0, 2000)), None),
                false,
                1001
            )
            .len(),
            1
        );
        // 已經過了重置時間的資料不通知
        assert!(
            due_alerts(
                &mut s,
                "a",
                &usage(None, Some((99.0, 500)), None),
                false,
                1001
            )
            .is_empty()
        );
    }

    #[test]
    fn poll_usage_reads_changes_and_publishes_only_visible_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_file: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            home: dir.path().to_path_buf(),
            logs_dir: None,
        };
        let d = Daemon::new(paths, "test".into(), "agt".into(), Config::defaults(), None).unwrap();
        let wt = dir.path().join("wt");
        let notes = wt.join(agt_git::NOTES_DIR);
        std::fs::create_dir_all(&notes).unwrap();
        let mut info = row_to_info(&SessionRow {
            id: "s1".into(),
            short_id: "aaaaaa".into(),
            status: "running".into(),
            ..Default::default()
        });
        info.cwd = wt.to_string_lossy().into_owned();
        d.entries.lock_or_recover().insert(
            "s1".into(),
            Entry {
                info,
                session: None,
                conversation: None,
                generation: 1,
                resume_attempt: None,
            },
        );
        let mut rx = d.events.subscribe();
        let write = |pct: f64| {
            std::fs::write(
                notes.join(USAGE_FILE),
                format!(r#"{{"context_window": {{"used_percentage": {pct}}}, "rate_limits": {{"five_hour": {{"used_percentage": 50, "resets_at": 9999999999}}}}}}"#),
            )
            .unwrap();
        };
        // 還沒有檔案：沒有用量
        d.poll_usage();
        assert!(d.get("s1").unwrap().usage.is_none());

        write(40.0);
        d.poll_usage();
        let u = d.get("s1").unwrap().usage.unwrap();
        assert_eq!(u.context_percent, Some(40.0));
        assert_eq!(u.five_hour.unwrap().used_percent, 50.0);
        assert!(matches!(
            rx.try_recv(),
            Ok(ServerMsg::SessionUpdated { .. })
        ));

        // 修改時間沒變：不重讀
        d.poll_usage();
        assert!(rx.try_recv().is_err());

        // 內容變了但顯示相同（40 → 40.2）：更新但不推播。修改時間的精度可能是秒，先改成不同的時間
        write(40.2);
        let f = std::fs::File::options()
            .write(true)
            .open(notes.join(USAGE_FILE))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        d.poll_usage();
        assert_eq!(
            d.get("s1").unwrap().usage.unwrap().context_percent,
            Some(40.2)
        );
        assert!(rx.try_recv().is_err());

        // 格式不對：保留上一次的值
        std::fs::write(notes.join(USAGE_FILE), "garbage").unwrap();
        let f = std::fs::File::options()
            .write(true)
            .open(notes.join(USAGE_FILE))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(10))
            .unwrap();
        drop(f);
        d.poll_usage();
        assert_eq!(
            d.get("s1").unwrap().usage.unwrap().context_percent,
            Some(40.2)
        );

        // 分頁關閉後不再保留紀錄
        d.entries.lock_or_recover().clear();
        d.poll_usage();
        assert!(d.usage_seen.lock_or_recover().is_empty());
    }
}
