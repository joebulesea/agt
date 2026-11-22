//! 收尾開出的 PR 之後有沒有進 base（`agt landing`）。
//!
//! 收尾 pr 成功後，把每個 PR 記進資料庫（`landings`，不綁分頁）；daemon 定期用 `gh` 查還沒有結果的 PR：
//! 合併了就確認合併 commit 在 `<remote>/<base>` 裡，進錯 branch（例如堆疊 PR 只進了前一組）或找不到
//! 合併 commit 時發通知。只在主 repo 執行 `gh` 與 `git fetch`，不碰分頁的 worktree，所以沙箱分頁與
//! 已關閉的分頁都一樣處理。

use super::*;
use agt_git::{Ci, CiStatus, Landing, PrTarget};
use agt_proto::{CiState, LandingInfo, LandingState};
use agt_store::LandingRow;

/// 檢查失敗後第一次重試的間隔；之後每次加倍，最長 [`RETRY_MAX`]。
const RETRY_BASE: Duration = Duration::from_secs(60);
const RETRY_MAX: Duration = Duration::from_secs(600);
/// 追蹤紀錄保留多久（從建立算起）。
const KEEP_SECS: i64 = 60 * 24 * 3600;

/// 第 `fails` 次失敗後多久再試。
pub(super) fn retry_delay(fails: u32) -> Duration {
    RETRY_BASE
        .saturating_mul(1u32 << fails.min(4))
        .min(RETRY_MAX)
}

/// 把 git 層的結果換成要存的狀態與說明。
fn describe(l: &Landing, base: &str) -> (LandingState, String) {
    match l {
        Landing::Open => (LandingState::Open, String::new()),
        Landing::Landed => (LandingState::Landed, String::new()),
        Landing::Closed => (LandingState::Closed, String::new()),
        Landing::WrongBase(b) => (LandingState::WrongBase, format!("合進 {b}，不是 {base}")),
        Landing::Missing => (
            LandingState::Missing,
            format!("已合併，但合併 commit 不在 {base}"),
        ),
    }
}

pub(super) fn to_info(r: LandingRow) -> LandingInfo {
    LandingInfo {
        short_id: r.short_id,
        task: r.task,
        repo_path: r.repo_path,
        name: r.name,
        branch: r.branch,
        base: r.base,
        state: r.state.parse().unwrap_or(LandingState::Open),
        detail: r.detail,
        created_at: r.created_at,
        checked_at: r.checked_at,
        ci: r.ci.parse().unwrap_or_default(),
        ci_detail: r.ci_detail,
    }
}

fn ci_state(c: Ci) -> CiState {
    match c {
        Ci::None => CiState::None,
        Ci::Pending => CiState::Pending,
        Ci::Success => CiState::Success,
        Ci::Failure => CiState::Failure,
    }
}

impl Daemon {
    /// 收尾 pr 成功後記下要追蹤的 PR。寫入失敗只記 log，不影響收尾結果。
    pub(super) fn track_landings(&self, info: &SessionInfo, repo: &str, targets: &[PrTarget]) {
        for t in targets {
            let row = LandingRow {
                session_id: info.id.clone(),
                short_id: info.short_id.clone(),
                task: info.task.clone(),
                repo_path: repo.to_string(),
                name: t.name.clone(),
                branch: t.branch.clone(),
                base: t.base.clone(),
                created_at: agt_store::now(),
                ..Default::default()
            };
            if let Err(e) = self.store().upsert_landing(&row) {
                tracing::warn!("無法記錄要追蹤的 PR {}：{e}", t.branch);
            }
            self.landing_retry
                .lock_or_recover()
                .remove(&(repo.to_string(), t.branch.clone()));
        }
        self.refresh_landing_info(&[info.id.as_str()]);
    }

    /// 把資料庫裡這些分頁的 PR 結果帶進 `SessionInfo.landing`，有變化的推播給 UI（側欄顯示）。
    /// 分頁已關閉（不在 entries）時什麼都不做，之後用 `agt landing` 查。
    fn refresh_landing_info(&self, session_ids: &[&str]) {
        let Ok(rows) = self.store().landings(false) else {
            return;
        };
        for id in session_ids {
            let list: Vec<LandingInfo> = rows
                .iter()
                .filter(|r| r.session_id == *id)
                .cloned()
                .map(to_info)
                .collect();
            let changed = {
                let mut entries = self.entries.lock_or_recover();
                match entries.get_mut(*id) {
                    Some(e) if e.info.landing != list => {
                        e.info.landing = list;
                        Some(e.info.clone())
                    }
                    _ => None,
                }
            };
            if let Some(info) = changed {
                self.publish(info);
            }
        }
    }

    /// `agt landing`：列出追蹤中的 PR。`check` 為 true 時先立刻重新查一次還沒有結果的。
    pub fn list_landings(&self, check: bool) -> OpResult<Vec<LandingInfo>> {
        if check {
            self.poll_landing(true);
        }
        let rows = self
            .store()
            .landings(false)
            .map_err(|e| OpError::new(codes::INTERNAL, e.to_string()))?;
        Ok(rows.into_iter().map(to_info).collect())
    }

    /// 查還沒有結果的 PR。`force` 為 true 時忽略失敗後的重試間隔。
    pub fn poll_landing(&self, force: bool) {
        self.poll_landing_with(force, &agt_git::check_landing, &agt_git::check_ci);
    }

    /// [`Self::poll_landing`]，檢查函式可替換（測試用，不必真的呼叫 `gh`）。
    fn poll_landing_with(
        &self,
        force: bool,
        check: &dyn Fn(&Path, &PrTarget) -> agt_git::Result<Landing>,
        ci: &dyn Fn(&Path, &PrTarget) -> agt_git::Result<CiStatus>,
    ) {
        let pending = match self.store().landings(true) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("讀取追蹤中的 PR 失敗：{e}");
                return;
            }
        };
        let mut touched: Vec<String> = vec![];
        for r in pending {
            let key = (r.repo_path.clone(), r.branch.clone());
            if !force
                && let Some((at, _)) = self.landing_retry.lock_or_recover().get(&key)
                && *at > Instant::now()
            {
                continue;
            }
            let target = PrTarget {
                name: r.name.clone(),
                branch: r.branch.clone(),
                base: r.base.clone(),
            };
            match check(Path::new(&r.repo_path), &target) {
                Ok(l) => {
                    self.landing_retry.lock_or_recover().remove(&key);
                    let (state, detail) = describe(&l, &r.base);
                    if state == LandingState::Open {
                        // 還開著：順便查 CI。這個查詢失敗不影響上面「有沒有進 base」的結果，下次再試
                        if self.update_ci(&r, ci(Path::new(&r.repo_path), &target)) {
                            touched.push(r.session_id.clone());
                        }
                        continue;
                    }
                    if let Err(e) =
                        self.store()
                            .set_landing(&r.repo_path, &r.branch, state.as_str(), &detail)
                    {
                        tracing::warn!("無法寫入 PR {} 的結果：{e}", r.branch);
                        continue;
                    }
                    self.record_landing(&r, state, &detail);
                    touched.push(r.session_id.clone());
                }
                Err(e) => {
                    let fails = {
                        let mut m = self.landing_retry.lock_or_recover();
                        let fails = m.get(&key).map_or(0, |(_, n)| *n) + 1;
                        m.insert(key, (Instant::now() + retry_delay(fails - 1), fails));
                        fails
                    };
                    // gh 的輸出不可信：清成單行、限制長度
                    let text = agt_proto::text::printable_line(&e.to_string());
                    let detail: String =
                        format!("上次檢查失敗：{text}").chars().take(200).collect();
                    tracing::info!(branch = %r.branch, fails, "檢查 PR 是否進 {} 失敗：{text}", r.base);
                    let _ = self
                        .store()
                        .set_landing(&r.repo_path, &r.branch, "open", &detail);
                }
            }
        }
        let _ = self.store().prune_landings(agt_store::now() - KEEP_SECS);
        // 有結果的分頁：把最新的 PR 進度帶進側欄資料並推播
        touched.sort();
        touched.dedup();
        let ids: Vec<&str> = touched.iter().map(String::as_str).collect();
        self.refresh_landing_info(&ids);
    }

    /// 寫入 CI 狀態；有變化回傳 true。CI 變成失敗時發通知（只通知異常，通過與執行中不通知）。
    fn update_ci(&self, r: &LandingRow, res: agt_git::Result<CiStatus>) -> bool {
        let c = match res {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(branch = %r.branch, "查 PR 的 CI 失敗：{e}");
                return false;
            }
        };
        // 名稱來自 PR 的檢查（不可信）：agt-git 已清過，這裡再保險一次
        let detail = agt_proto::text::printable_line(&c.failed);
        let state = ci_state(c.ci);
        match self
            .store()
            .set_landing_ci(&r.repo_path, &r.branch, state.as_str(), &detail)
        {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                tracing::warn!("無法寫入 PR {} 的 CI 狀態：{e}", r.branch);
                return false;
            }
        }
        if state == CiState::Failure {
            let data =
                json!({"name": r.name, "branch": r.branch, "ci": "failure", "failed": detail});
            if self.entries.lock_or_recover().contains_key(&r.session_id) {
                self.record(&r.session_id, "landing", data);
            }
            let body = format!("[{}] {}（{}）", r.name, detail, r.branch);
            tracing::warn!(session = %r.short_id, "PR 的 CI 失敗：{body}");
            if self.cfg().general.notify {
                platform::notify(&format!("agt · {} 的 PR CI 失敗", r.task), &body, None);
            }
        }
        true
    }

    /// 寫進事件記錄；只有異常（沒進預期的 base）才發通知。
    fn record_landing(&self, r: &LandingRow, state: LandingState, detail: &str) {
        let data = json!({"name": r.name, "branch": r.branch, "base": r.base, "state": state.as_str(), "detail": detail});
        // 分頁可能已被清掉：只記還在的
        if self.entries.lock_or_recover().contains_key(&r.session_id) {
            self.record(&r.session_id, "landing", data);
        }
        if state.is_problem() {
            let body = format!("[{}] {}（{}）", r.name, detail, r.branch);
            tracing::warn!(session = %r.short_id, "PR 沒有進 {}：{body}", r.base);
            if self.cfg().general.notify {
                platform::notify(
                    &format!("agt · {} 的 PR 沒進 {}", r.task, r.base),
                    &body,
                    None,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Paths};
    use agt_store::SessionRow;
    use std::cell::RefCell;

    fn paths(dir: &Path) -> Paths {
        Paths {
            config_file: dir.join("config.toml"),
            data_dir: dir.join("data"),
            home: dir.to_path_buf(),
            logs_dir: None,
        }
    }

    fn daemon(dir: &Path) -> Arc<Daemon> {
        let mut cfg = Config::defaults();
        cfg.general.notify = false; // 測試不要真的跳系統通知
        Daemon::new(paths(dir), "test".into(), "agt".into(), cfg, None).unwrap()
    }

    /// 在 entries 放一個還開著的分頁（側欄會顯示它的 PR 進度）。
    fn add_session(d: &Daemon, id: &str) -> SessionInfo {
        let info = row_to_info(&SessionRow {
            id: id.into(),
            short_id: "aaaaaa".into(),
            task: "任務".into(),
            status: "running".into(),
            ..Default::default()
        });
        d.entries.lock_or_recover().insert(
            id.into(),
            Entry {
                info: info.clone(),
                session: None,
                conversation: None,
                generation: 1,
                resume_attempt: None,
            },
        );
        info
    }

    /// 不查 CI（回傳錯誤，等於 gh 沒有這項資料）。
    fn no_ci(_: &Path, _: &PrTarget) -> agt_git::Result<CiStatus> {
        Err(agt_git::GitError::Refused("沒有 CI".into()))
    }

    fn target(name: &str) -> PrTarget {
        PrTarget {
            name: name.into(),
            branch: format!("agt/t-{name}"),
            base: "main".into(),
        }
    }

    /// 組名 → 狀態（排序後，不依賴資料庫的列出順序）。
    fn states(d: &Daemon) -> std::collections::BTreeMap<String, LandingState> {
        d.list_landings(false)
            .unwrap()
            .into_iter()
            .map(|l| (l.name, l.state))
            .collect()
    }

    #[test]
    fn poll_records_results_updates_sidebar_info_and_stops_polling_finished_prs() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemon(dir.path());
        let info = add_session(&d, "s1");
        d.track_landings(
            &info,
            "/r",
            &[target("one"), target("two"), target("three")],
        );
        // 追蹤時就帶進側欄的資料（都還在等待）
        assert_eq!(d.get("s1").unwrap().landing.len(), 3);
        let mut rx = d.events.subscribe();

        let calls = RefCell::new(vec![]);
        let answer = |_: &Path, t: &PrTarget| {
            calls.borrow_mut().push(t.name.clone());
            Ok(match t.name.as_str() {
                "one" => Landing::Landed,
                "two" => Landing::WrongBase("agt/t-one".into()),
                _ => Landing::Open, // 還開著：下次還要再查
            })
        };
        d.poll_landing_with(false, &answer, &no_ci);
        let got = states(&d);
        assert_eq!(got["one"], LandingState::Landed);
        assert_eq!(got["two"], LandingState::WrongBase);
        assert_eq!(got["three"], LandingState::Open);
        // 側欄的資料跟著更新，並推播完整的 SessionUpdated
        let landing = d.get("s1").unwrap().landing;
        let two = landing.iter().find(|l| l.name == "two").unwrap();
        assert_eq!(two.state, LandingState::WrongBase);
        assert_eq!(two.detail, "合進 agt/t-one，不是 main");
        assert!(matches!(
            rx.try_recv(),
            Ok(ServerMsg::SessionUpdated { .. })
        ));
        // 有結果的不再查；還開著的繼續查
        calls.borrow_mut().clear();
        d.poll_landing_with(false, &answer, &no_ci);
        assert_eq!(*calls.borrow(), vec!["three".to_string()]);
    }

    #[test]
    fn open_prs_get_ci_status_in_sidebar_and_ci_errors_do_not_affect_landing() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemon(dir.path());
        let info = add_session(&d, "s1");
        d.track_landings(&info, "/r", &[target("one")]);
        let open = |_: &Path, _: &PrTarget| Ok(Landing::Open);
        let ci = |c: Ci, failed: &str| {
            let failed = failed.to_string();
            move |_: &Path, _: &PrTarget| {
                Ok(CiStatus {
                    ci: c,
                    failed: failed.clone(),
                })
            }
        };
        let sidebar = |d: &Daemon| d.get("s1").unwrap().landing.remove(0);
        let mut rx = d.events.subscribe();

        d.poll_landing_with(true, &open, &ci(Ci::Pending, ""));
        assert_eq!(sidebar(&d).ci, CiState::Pending);
        assert!(matches!(
            rx.try_recv(),
            Ok(ServerMsg::SessionUpdated { .. })
        ));
        // 沒有變化：不再推播
        d.poll_landing_with(true, &open, &ci(Ci::Pending, ""));
        assert!(rx.try_recv().is_err());
        // 失敗：帶出失敗的檢查名稱（控制字元會被清掉）
        d.poll_landing_with(true, &open, &ci(Ci::Failure, "clippy\u{1b}[31m, test"));
        let l = sidebar(&d);
        assert_eq!(l.ci, CiState::Failure);
        assert!(!l.ci_detail.contains('\u{1b}'), "{:?}", l.ci_detail);
        assert!(l.ci_detail.starts_with("clippy"), "{:?}", l.ci_detail);
        let label = d
            .get("s1")
            .unwrap()
            .landing_label(agt_store::now())
            .unwrap();
        assert!(label.text.starts_with("✗ CI 失敗："), "{}", label.text);
        // 查 CI 失敗（gh 不支援等）：維持上次的 CI 狀態，也不影響「有沒有進 base」
        d.poll_landing_with(true, &open, &no_ci);
        assert_eq!(sidebar(&d).ci, CiState::Failure);
        d.poll_landing_with(true, &|_: &Path, _: &PrTarget| Ok(Landing::Landed), &no_ci);
        assert_eq!(states(&d)["one"], LandingState::Landed);
        // 已進 base 的 PR 不再查 CI
        d.poll_landing_with(true, &open, &|_: &Path, _: &PrTarget| {
            panic!("已有結果，不該再查 CI")
        });
    }

    #[test]
    fn failed_checks_back_off_keep_waiting_and_recover() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemon(dir.path());
        let info = add_session(&d, "s1");
        d.track_landings(&info, "/r", &[target("one")]);
        let n = RefCell::new(0);
        let fail = |_: &Path, _: &PrTarget| {
            *n.borrow_mut() += 1;
            Err(agt_git::GitError::Refused("連不上\n網路\u{1b}[31m".into()))
        };
        d.poll_landing_with(false, &fail, &no_ci);
        assert_eq!(*n.borrow(), 1);
        // 失敗：維持等待，原因（單行、無控制字元）給 `agt landing` 看
        let l = d.list_landings(false).unwrap().remove(0);
        assert_eq!(l.state, LandingState::Open);
        assert!(l.detail.starts_with("上次檢查失敗："), "{}", l.detail);
        assert!(!l.detail.contains(['\n', '\u{1b}']), "{:?}", l.detail);
        // 還在退避：一般輪詢不查，`--check`（force）才查
        d.poll_landing_with(false, &fail, &no_ci);
        assert_eq!(*n.borrow(), 1);
        d.poll_landing_with(true, &fail, &no_ci);
        assert_eq!(*n.borrow(), 2);
        // 恢復後寫入結果並清掉退避
        d.poll_landing_with(true, &|_: &Path, _: &PrTarget| Ok(Landing::Landed), &no_ci);
        let l = d.list_landings(false).unwrap().remove(0);
        assert_eq!((l.state, l.detail.as_str()), (LandingState::Landed, ""));
        assert!(d.landing_retry.lock_or_recover().is_empty());
    }

    #[test]
    fn finished_tabs_keep_their_landings_and_restart_restores_the_sidebar() {
        let dir = tempfile::tempdir().unwrap();
        let info;
        {
            let d = daemon(dir.path());
            d.store()
                .insert_session(&SessionRow {
                    id: "s1".into(),
                    short_id: "aaaaaa".into(),
                    agent: "claude".into(),
                    task: "任務".into(),
                    status: "idle".into(),
                    created_at: agt_store::now(),
                    ..Default::default()
                })
                .unwrap();
            info = add_session(&d, "s1");
            d.track_landings(&info, "/r", &[target("one"), target("two")]);
            d.poll_landing_with(
                false,
                &|_: &Path, t: &PrTarget| {
                    Ok(if t.name == "one" {
                        Landing::Landed
                    } else {
                        Landing::Missing
                    })
                },
                &no_ci,
            );
        }
        // daemon 重啟：分頁從資料庫載入，側欄的 PR 進度跟著回來
        let d = daemon(dir.path());
        let got = d.get("s1").unwrap().landing;
        assert_eq!(got.len(), 2);
        assert_eq!(
            got.iter().find(|l| l.name == "two").unwrap().state,
            LandingState::Missing
        );
        // 分頁關閉（不在 entries）後，`agt landing` 仍查得到，也不會 panic
        d.entries.lock_or_recover().remove("s1");
        assert_eq!(states(&d).len(), 2);
        d.poll_landing_with(
            true,
            &|_: &Path, _: &PrTarget| panic!("已有結果，不該再查"),
            &no_ci,
        );
    }

    #[test]
    fn retry_delay_doubles_and_caps() {
        let secs = |n| retry_delay(n).as_secs();
        assert_eq!((secs(0), secs(1), secs(2), secs(3)), (60, 120, 240, 480));
        assert_eq!((secs(4), secs(40)), (600, 600));
    }

    #[test]
    fn only_wrong_base_and_missing_are_problems() {
        let (s, d) = describe(&Landing::WrongBase("agt/t-one".into()), "main");
        assert!(s.is_problem() && d == "合進 agt/t-one，不是 main");
        assert!(describe(&Landing::Missing, "main").0.is_problem());
        for l in [Landing::Open, Landing::Landed, Landing::Closed] {
            assert!(!describe(&l, "main").0.is_problem());
        }
    }
}
