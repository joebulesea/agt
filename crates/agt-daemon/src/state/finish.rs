//! 收尾：merge / pr / keep / discard，以及 diff。

use super::*;

/// 收尾檢查（`finish_check`）每組最多執行多久。
const CHECK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// 收尾檢查函式（見 [`agt_git::GroupCheck`]）。
type CheckFn = Box<dyn Fn(&str, &Path) -> Result<(), String>>;

/// 由檢查目錄組出要執行的指令。
type ArgvFn = Box<dyn Fn(&Path) -> Vec<String>>;

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
                    let (out, targets) = agt_git::create_group_prs(
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
                    self.track_landings(&info, &repo, &targets);
                    return Ok(with_note(out, note));
                }
                // PR 標題：這次 commit 的標題（Agent 寫的 COMMIT_MSG），沒有時用任務名稱
                let title = subject.unwrap_or_else(|| info.task.clone());
                let (out, target) =
                    agt_git::create_pr(Path::new(&repo), Path::new(&wt), &branch, &base, &title)?;
                self.record(&id, "git", json!({"action": "pr", "result": out}));
                self.store().set_finish_action(&id, "pr")?;
                self.track_landings(&info, &repo, target.as_slice());
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
        let script = self
            .cfg()
            .repo(&self.paths, Path::new(repo))
            .map_err(|e| OpError::new(codes::BAD_REQUEST, e))?
            .finish_check
            .filter(|s| !s.trim().is_empty());
        let Some(script) = script else {
            return Ok((None, None));
        };
        self.check_runner(info, script)
    }

    /// 依 Agent 的執行方式（`check_command`／沙箱／主機 shell）組出執行 `script` 的檢查函式；
    /// 規則同 [`Self::group_check`]。收尾檢查與自動檢查（`auto_check`）共用。
    pub(super) fn check_runner(
        &self,
        info: &SessionInfo,
        script: String,
    ) -> OpResult<(Option<CheckFn>, Option<String>)> {
        let cfg = self.cfg();
        let agent = cfg.agents.get(&info.agent).cloned().unwrap_or_default();
        let argv: ArgvFn = if !agent.check_command.is_empty() {
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

    /// Agent 完成後（狀態變成 Done）自動執行 `[repos] auto_check`：在分頁的 worktree 裡跑，
    /// 結果寫進 `SessionInfo.check`（側欄顯示）、分頁的 events（kind = "check"），並發系統通知。
    /// 沒有設定、共用 worktree、沒有任何變更、或已經在執行時什麼都不做。
    pub(super) fn spawn_auto_check(self: &Arc<Self>, info: &SessionInfo, generation: u64) {
        let (Some(wt), Some(repo), Some(base)) = (
            info.worktree.clone(),
            info.repo_path.clone(),
            info.base_branch.clone(),
        ) else {
            return;
        };
        if info.shared {
            return;
        }
        let script = match self.cfg().repo(&self.paths, Path::new(&repo)) {
            Ok(r) => r.auto_check.filter(|s| !s.trim().is_empty()),
            Err(_) => None,
        };
        let Some(script) = script else {
            return;
        };
        if !self.auto_checking.lock_or_recover().insert(info.id.clone()) {
            return;
        }
        let running = CheckInfo {
            state: CheckState::Running,
            detail: String::new(),
        };
        match self.update_if_current(&info.id, generation, |i| i.check = Some(running)) {
            Some(i) => self.publish(i),
            None => {
                self.auto_checking.lock_or_recover().remove(&info.id);
                return;
            }
        }
        let d = self.clone();
        let info = info.clone();
        tokio::task::spawn_blocking(move || {
            let outcome = d.run_auto_check(&info, &repo, &wt, &base, script);
            d.auto_checking.lock_or_recover().remove(&info.id);
            let result = match outcome {
                Some(r) => r,
                None => {
                    // 沒有執行（沒有變更、或沙箱 Agent 沒有 check_command）：清掉「執行中」
                    if let Some(i) = d.update_if_current(&info.id, generation, |i| i.check = None) {
                        d.publish(i);
                    }
                    return;
                }
            };
            let (ok, text) = match &result {
                Ok(()) => (true, String::new()),
                Err(e) => (false, e.clone()),
            };
            let detail = agt_proto::text::printable_line(
                text.lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or(""),
            )
            .trim()
            .chars()
            .take(120)
            .collect::<String>();
            let state = if ok {
                CheckState::Passed
            } else {
                CheckState::Failed
            };
            let done = CheckInfo {
                state,
                detail: detail.clone(),
            };
            // 檢查期間 Agent 又開始工作：結果已過期，不顯示（事件仍照常記錄）
            let updated = d.update_if_current(&info.id, generation, |i| {
                if !matches!(
                    i.status,
                    AgentStatus::Running | AgentStatus::Thinking | AgentStatus::Starting
                ) {
                    i.check = Some(done);
                }
            });
            if let Some(i) = updated {
                d.publish(i);
            }
            let output: String = agt_proto::text::printable(&text)
                .chars()
                .take(8192)
                .collect();
            d.record(
                &info.id,
                "check",
                json!({"ok": ok, "detail": detail, "output": output}),
            );
            let viewers = d.session(&info.id).map_or(0, |s| s.viewers());
            if d.cfg().general.notify && viewers == 0 {
                let body = if ok {
                    "測試通過".to_string()
                } else {
                    format!("測試失敗：{detail}")
                };
                platform::notify(
                    &format!("agt · {}", info.task),
                    &body,
                    Some(info.id.as_str()),
                );
            }
        });
    }

    /// 在 worktree 執行自動檢查。沒有變更或不能執行時回傳 None（並記錄原因）。
    fn run_auto_check(
        &self,
        info: &SessionInfo,
        repo: &str,
        wt: &str,
        base: &str,
        script: String,
    ) -> Option<Result<(), String>> {
        let changed = agt_git::status(Path::new(repo), Path::new(wt), base)
            .map(|s| s.dirty || s.ahead > 0)
            .unwrap_or(true);
        if !changed {
            return None;
        }
        match self.check_runner(info, script) {
            Ok((Some(check), _)) => Some(check("auto", Path::new(wt))),
            Ok((None, note)) => {
                self.record(&info.id, "check", json!({"skipped": true, "note": note}));
                None
            }
            Err(e) => Some(Err(e.message)),
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

    /// 把 base 的最新 commit 合併進分頁的 branch。Agent（含共用這個 worktree 的其他分頁）
    /// 正在工作時拒絕：合併會在它腳下改檔案。
    pub fn sync_base(&self, id: &str) -> OpResult<String> {
        let id = self.resolve(id)?;
        let info = self.get(&id)?;
        let (wt, _, base, repo) = need_worktree(&info)?;
        let busy =
            self.entries.lock_or_recover().values().any(|e| {
                e.info.status.is_busy() && e.info.worktree.as_deref() == Some(wt.as_str())
            });
        if busy {
            return Err(OpError::new(
                codes::BAD_REQUEST,
                "Agent 正在工作中，等它停下來（等待輸入或完成）再同步",
            ));
        }
        let r = agt_git::sync_with_base(Path::new(&repo), Path::new(&wt), &base);
        match &r {
            Ok(msg) => self.record(
                &id,
                "git",
                json!({"action": "sync", "ok": true, "result": msg}),
            ),
            Err(e) => self.record(
                &id,
                "git",
                json!({"action": "sync", "ok": false, "error": e.to_string()}),
            ),
        }
        Ok(r?)
    }
}
