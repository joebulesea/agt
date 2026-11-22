//! 收尾開出的 PR 之後有沒有進 base（`agt landing`）。
//!
//! 收尾 pr 成功後，把每個 PR 記進資料庫（`landings`，不綁分頁）；daemon 定期用 `gh` 查還沒有結果的 PR：
//! 合併了就確認合併 commit 在 `<remote>/<base>` 裡，進錯 branch（例如堆疊 PR 只進了前一組）或找不到
//! 合併 commit 時發通知。只在主 repo 執行 `gh` 與 `git fetch`，不碰分頁的 worktree，所以沙箱分頁與
//! 已關閉的分頁都一樣處理。

use super::*;
use agt_git::{Landing, PrTarget};
use agt_proto::{LandingInfo, LandingState};
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
            match agt_git::check_landing(Path::new(&r.repo_path), &target) {
                Ok(l) => {
                    self.landing_retry.lock_or_recover().remove(&key);
                    let (state, detail) = describe(&l, &r.base);
                    if state == LandingState::Open {
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
