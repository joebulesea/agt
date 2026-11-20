//! 收尾：merge / pr / keep / discard，以及 diff。

use super::*;

impl Daemon {
    /// 收尾動作（規格 7.3）。失敗時寫進 agtd.log 與該分頁的 events（kind = "git"、ok = false），
    /// 錯誤視窗關掉後也查得到原因；要求選擇策略 / 確認（NEED_STRATEGY、NEEDS_CONFIRM）不算失敗。
    pub fn finish(&self, id: &str, action: FinishAction, force: bool) -> OpResult<String> {
        let name = action.name();
        let r = self.finish_inner(id, action, force);
        if let Err(e) = &r
            && e.code != codes::NEED_STRATEGY
            && e.code != codes::NEEDS_CONFIRM
        {
            tracing::warn!("{id}：收尾 {name} 失敗（{}）：{}", e.code, e.message);
            if let Ok(full) = self.resolve(id) {
                self.record(
                    &full,
                    "git",
                    json!({"action": name, "ok": false, "code": e.code, "error": e.message}),
                );
            }
        }
        r
    }

    pub(super) fn finish_inner(
        &self,
        id: &str,
        action: FinishAction,
        force: bool,
    ) -> OpResult<String> {
        let id = self.resolve(id)?;
        let info = self.get(&id)?;
        match action {
            FinishAction::Keep => {
                self.stop_agent(&id);
                self.record(&id, "git", json!({"action": "keep"}));
                self.remove_entry(&id, Some("keep"))?;
                Ok(match &info.worktree {
                    Some(wt) => format!(
                        "已結束 Agent，保留 worktree {wt} 與 branch {}",
                        info.branch.unwrap_or_default()
                    ),
                    None => "已結束 Agent".into(),
                })
            }
            FinishAction::Discard => {
                self.check_discard(&info, force)?;
                self.stop_agent(&id);
                self.discard_worktree(&info)?;
                self.record(&id, "git", json!({"action": "discard"}));
                self.remove_entry(&id, Some("discard"))?;
                Ok(if info.shared {
                    "已關閉分頁；worktree 屬於其他分頁，已保留".into()
                } else {
                    "已移除 worktree 並刪除 branch".into()
                })
            }
            FinishAction::Merge { strategy } => {
                let (wt, branch, base, repo) = need_worktree(&info)?;
                self.auto_commit(&id, &info, &repo, &wt)?;
                let cfg_strategy = self.cfg().general.merge_strategy.clone();
                let strategy = match strategy {
                    Some(s) => s,
                    None if cfg_strategy != "ask" => cfg_strategy
                        .parse()
                        .map_err(|e: String| OpError::new(codes::BAD_REQUEST, e))?,
                    None => {
                        return Err(OpError::new(
                            codes::NEED_STRATEGY,
                            "請選擇 merge 策略：merge、squash 或 rebase",
                        ));
                    }
                };
                let extra = self.cfg().general.sensitive_paths.clone();
                confirm_sensitive(&repo, &wt, &base, &extra, "merge", force)?;
                let s = match strategy {
                    MergeStrategy::Merge => agt_git::Strategy::Merge,
                    MergeStrategy::Squash => agt_git::Strategy::Squash,
                    MergeStrategy::Rebase => agt_git::Strategy::Rebase,
                };
                // Agent 在 .agt/groups/ 描述了分組：每組各自一個 commit 依序合進 base
                let groups = agt_git::load_groups(Path::new(&wt))?;
                if !groups.is_empty() {
                    let out = agt_git::merge_groups(
                        Path::new(&repo),
                        Path::new(&wt),
                        &branch,
                        &base,
                        &info.task,
                        s,
                        groups,
                    );
                    self.record(&id, "git", json!({"action": "merge", "groups": true, "strategy": format!("{strategy:?}").to_lowercase(), "ok": out.is_ok()}));
                    let out = out?;
                    self.store().set_finish_action(&id, "merge")?;
                    return Ok(out);
                }
                let msg = agt_git::merge_into_base(
                    Path::new(&repo),
                    Path::new(&wt),
                    &branch,
                    &base,
                    s,
                    &info.task,
                );
                self.record(&id, "git", json!({"action": "merge", "strategy": format!("{strategy:?}").to_lowercase(), "ok": msg.is_ok()}));
                let msg = msg?;
                self.store().set_finish_action(&id, "merge")?;
                Ok(msg)
            }
            FinishAction::Pr => {
                let (wt, branch, base, repo) = need_worktree(&info)?;
                let subject = self.auto_commit(&id, &info, &repo, &wt)?;
                let extra = self.cfg().general.sensitive_paths.clone();
                confirm_sensitive(&repo, &wt, &base, &extra, "建立 PR", force)?;
                // Agent 在 .agt/groups/ 描述了分組：每組各自 branch / commit / PR
                let groups = agt_git::load_groups(Path::new(&wt))?;
                if !groups.is_empty() {
                    let out = agt_git::create_group_prs(
                        Path::new(&repo),
                        Path::new(&wt),
                        &branch,
                        &base,
                        &info.task,
                        groups,
                    )?;
                    self.record(
                        &id,
                        "git",
                        json!({"action": "pr", "groups": true, "result": out}),
                    );
                    self.store().set_finish_action(&id, "pr")?;
                    return Ok(out);
                }
                // PR 標題：這次 commit 的標題（Agent 寫的 COMMIT_MSG），沒有時用任務名稱
                let title = subject.unwrap_or_else(|| info.task.clone());
                let out =
                    agt_git::create_pr(Path::new(&repo), Path::new(&wt), &branch, &base, &title)?;
                self.record(&id, "git", json!({"action": "pr", "result": out}));
                self.store().set_finish_action(&id, "pr")?;
                Ok(out)
            }
        }
    }

    /// Agent 設定了 `auto_commit` 時，收尾前先 commit worktree 中的所有變更
    /// （Agent 寫了 `.agt/COMMIT_MSG` 時用它當 commit 訊息，否則用任務名稱）。
    /// 有 commit 時回傳 commit 訊息的標題。
    pub(super) fn auto_commit(
        &self,
        id: &str,
        info: &SessionInfo,
        repo: &str,
        wt: &str,
    ) -> OpResult<Option<String>> {
        let enabled = self
            .cfg()
            .agents
            .get(&info.agent)
            .is_some_and(|a| a.auto_commit);
        if !enabled {
            return Ok(None);
        }
        let msg = agt_git::commit_all(Path::new(repo), Path::new(wt), &info.task)?;
        if msg.is_some() {
            self.record(id, "git", json!({"action": "auto_commit"}));
        }
        Ok(msg.as_deref().and_then(agt_git::subject_line))
    }

    pub fn diff(&self, id: &str) -> OpResult<String> {
        let info = self.get(id)?;
        let (wt, _, base, repo) = need_worktree(&info)?;
        Ok(agt_git::diff(Path::new(&repo), Path::new(&wt), &base)?)
    }
}
