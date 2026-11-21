//! 收尾：merge / pr / keep / discard，以及 diff。

use super::*;

/// 收尾檢查（`finish_check`）每組最多執行多久。
const CHECK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// 收尾檢查函式（見 [`agt_git::GroupCheck`]）。
type CheckFn = Box<dyn Fn(&str, &Path) -> Result<(), String>>;

/// 把收尾檢查的略過說明附在結果後面。
fn with_note(out: String, note: Option<String>) -> String {
    match note {
        Some(n) => format!("{out}\n\n{n}"),
        None => out,
    }
}

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
                    let (check, note) = self.group_check(&info, &repo)?;
                    let out = agt_git::merge_groups(
                        Path::new(&repo),
                        Path::new(&wt),
                        &branch,
                        &base,
                        &info.task,
                        s,
                        groups,
                        check.as_deref(),
                    );
                    self.record(&id, "git", json!({"action": "merge", "groups": true, "strategy": format!("{strategy:?}").to_lowercase(), "check": check.is_some(), "ok": out.is_ok()}));
                    let out = out?;
                    self.store().set_finish_action(&id, "merge")?;
                    return Ok(with_note(out, note));
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
                    let (check, note) = self.group_check(&info, &repo)?;
                    let out = agt_git::create_group_prs(
                        Path::new(&repo),
                        Path::new(&wt),
                        &branch,
                        &base,
                        &info.task,
                        groups,
                        check.as_deref(),
                    )?;
                    self.record(
                        &id,
                        "git",
                        json!({"action": "pr", "groups": true, "check": check.is_some(), "result": out}),
                    );
                    self.store().set_finish_action(&id, "pr")?;
                    return Ok(with_note(out, note));
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

    /// 分組收尾的檢查（使用者設定檔 `[repos."<路徑>"] finish_check`）。回傳檢查函式，
    /// 以及沒有執行檢查時要附在結果後面的說明。
    /// - Agent 有 `check_command`：照它執行（沙箱 Agent 在容器裡執行，`{dir}` 掛載這組的 checkout）
    /// - 沒有、而且是沙箱 Agent：略過。不在主機上執行沙箱 Agent 寫的程式碼（`build.rs`、測試等）
    /// - 沒有、不是沙箱 Agent：在主機上用 shell 執行（這種 Agent 本來就在主機上執行程式碼）
    pub(super) fn group_check(
        &self,
        info: &SessionInfo,
        repo: &str,
    ) -> OpResult<(Option<CheckFn>, Option<String>)> {
        let cfg = self.cfg();
        let script = cfg
            .repo(&self.paths, Path::new(repo))
            .map_err(|e| OpError::new(codes::BAD_REQUEST, e))?
            .finish_check
            .filter(|s| !s.trim().is_empty());
        let Some(script) = script else {
            return Ok((None, None));
        };
        let agent = cfg.agents.get(&info.agent).cloned().unwrap_or_default();
        let argv: Box<dyn Fn(&Path) -> Vec<String>> = if !agent.check_command.is_empty() {
            let cmd = agent.check_command.clone();
            let session = info.id.clone();
            let script = script.clone();
            Box::new(move |dir: &Path| {
                let dir = dir.to_string_lossy();
                // {check} 最後替換：它的內容不會再被當成變數處理
                cmd.iter()
                    .map(|a| {
                        a.replace("{dir}", &dir)
                            .replace("{session}", &session)
                            .replace("{check}", &script)
                    })
                    .collect()
            })
        } else if agent.sandboxed() {
            return Ok((
                None,
                Some(format!(
                    "⚠ 沒有執行收尾檢查（{script}）：{} 是沙箱 Agent，沒有設定 check_command；agt 不在主機上執行沙箱 Agent 寫的程式碼",
                    info.agent
                )),
            ));
        } else {
            let script = script.clone();
            Box::new(move |_: &Path| platform::shell_argv(&script))
        };
        let id = info.id.clone();
        let check = move |name: &str, dir: &Path| -> Result<(), String> {
            let cmd = argv(dir);
            tracing::info!(%id, group = name, ?cmd, "收尾檢查");
            let r = platform::run_check(&cmd, dir, CHECK_TIMEOUT);
            if let Err(e) = &r {
                tracing::warn!(%id, group = name, "收尾檢查沒有通過：{e}");
            }
            r
        };
        Ok((Some(Box::new(check)), None))
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

    pub fn finish_preview(&self, id: &str, action: &str) -> OpResult<String> {
        let info = self.get(id)?;
        let (wt, _, base, repo) = need_worktree(&info)?;
        let strategy = self.cfg().general.merge_strategy.clone();
        let auto_commit = self
            .cfg()
            .agents
            .get(&info.agent)
            .is_some_and(|a| a.auto_commit);
        let out = agt_git::finish_preview(
            Path::new(&repo),
            Path::new(&wt),
            &base,
            &info.task,
            action == "merge",
            &strategy,
            auto_commit,
        )?;
        // 有分組時才會執行收尾檢查
        if agt_git::load_groups(Path::new(&wt))?.is_empty() {
            return Ok(out);
        }
        let (check, note) = self.group_check(&info, &repo)?;
        let script = self
            .cfg()
            .repo(&self.paths, Path::new(&repo))
            .ok()
            .and_then(|r| r.finish_check)
            .unwrap_or_default();
        Ok(match (check, note) {
            (Some(_), _) => format!(
                "{out}\n收尾前會在每組 branch 執行檢查：{script}（任何一組失敗就不 push、不合併）"
            ),
            (None, Some(n)) => format!("{out}\n{n}"),
            (None, None) => out,
        })
    }

    pub fn diff(&self, id: &str) -> OpResult<String> {
        let info = self.get(id)?;
        let (wt, _, base, repo) = need_worktree(&info)?;
        Ok(agt_git::diff(Path::new(&repo), Path::new(&wt), &base)?)
    }
}
