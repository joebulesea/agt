//! 工作階段的操作：回報狀態、改名、重新啟動、關閉、收尾（merge / pr / discard）、diff。

use super::*;

impl Daemon {
    // ───────────────────────── 操作 ─────────────────────────

    /// hooks 回報狀態（`agt notify`）。
    pub fn notify(
        &self,
        id: &str,
        status: AgentStatus,
        message: Option<String>,
        tool: Option<String>,
    ) -> OpResult<()> {
        let id = self.resolve(id)?;
        let session = self.session(&id).ok_or_else(|| OpError::not_found(&id))?;
        if tool.is_some() || message.is_some() {
            self.record(
                &id,
                "notify",
                json!({"status": status.as_str(), "tool": tool, "message": message}),
            );
        }
        session.set_status(status, StatusSource::Hook, message);
        Ok(())
    }

    pub fn rename(&self, id: &str, task: &str) -> OpResult<SessionInfo> {
        let id = self.resolve(id)?;
        let task = task.trim();
        if task.is_empty() {
            return Err(OpError::new(codes::BAD_REQUEST, "任務名稱不能是空的"));
        }
        self.store().rename(&id, task)?;
        let info = self
            .update_info(&id, |i| i.task = task.into())
            .ok_or_else(|| OpError::not_found(&id))?;
        self.publish(info.clone());
        Ok(info)
    }

    /// Agent 結束時呼叫：剛以 `resume_args` 接續、很快就以錯誤結束 → true（應改開新對話）。
    pub(super) fn resume_failed(&self, id: &str, generation: u64, code: Option<i32>) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let Some(e) = entries.get_mut(id) else {
            return false;
        };
        match e.resume_attempt {
            Some((g, t)) if g == generation => {
                e.resume_attempt = None;
                t.elapsed() < RESUME_GRACE && code != Some(0)
            }
            _ => false,
        }
    }

    /// 在同一個 worktree 重新啟動 Agent。Agent 設定了 `resume_args` 時接續上次的對話；
    /// `fresh` 或是沒有可接續的對話時開新對話。
    pub fn restart(self: &Arc<Self>, id: &str, fresh: bool) -> OpResult<SessionInfo> {
        let _one_at_a_time = self.restart_lock.lock().unwrap_or_else(|e| e.into_inner());
        let id = self.resolve(id)?;
        let (info, old, conversation) = {
            let entries = self.entries.lock().unwrap();
            let e = entries.get(&id).ok_or_else(|| OpError::not_found(&id))?;
            (e.info.clone(), e.session.clone(), e.conversation.clone())
        };
        if let Some(msg) = self
            .cfg()
            .agents
            .get(&info.agent)
            .and_then(|a| platform::docker_unavailable(&a.command))
        {
            return Err(OpError::new(codes::BAD_REQUEST, msg));
        }
        let can_resume = self
            .cfg()
            .agents
            .get(&info.agent)
            .is_some_and(|a| !a.resume_args.is_empty());
        let (conversation, resume) = match conversation {
            Some(c) if can_resume && !fresh => (c, true),
            _ => (new_conversation_id(), false),
        };
        if old.as_ref().is_some_and(|s| s.is_running()) {
            return Err(OpError::new(codes::BAD_REQUEST, "Agent 仍在執行中"));
        }
        if !Path::new(&info.cwd).is_dir() {
            return Err(OpError::new(
                codes::BAD_REQUEST,
                format!("工作目錄已不存在：{}", info.cwd),
            ));
        }
        let (cols, rows) = old.map(|s| s.size()).unzip();
        // 先登記這次啟動，再啟動程序：程序可能立刻結束
        let generation = {
            let mut entries = self.entries.lock().unwrap();
            let e = entries
                .get_mut(&id)
                .ok_or_else(|| OpError::not_found(&id))?;
            e.generation += 1;
            e.resume_attempt = resume.then(|| (e.generation, Instant::now()));
            e.generation
        };
        let (session, events) =
            self.start_session(&info, &conversation, resume, true, None, cols, rows)?;
        let info = {
            let mut entries = self.entries.lock().unwrap();
            let e = entries
                .get_mut(&id)
                .ok_or_else(|| OpError::not_found(&id))?;
            e.session = Some(session.clone());
            e.info.status = AgentStatus::Starting;
            e.info.exit_code = None;
            e.conversation = Some(conversation.clone());
            e.info.clone()
        };
        let _ = self.store().update_process(&id, session.pid(), None);
        let _ = self.store().update_status(&id, "starting");
        if !resume {
            let _ = self.store().set_conversation(&id, &conversation);
        }
        self.record(
            &id,
            "status",
            json!({"to": "starting", "source": "restart", "resume": resume}),
        );
        self.watch(id, generation, session, events);
        self.publish(info.clone());
        Ok(info)
    }

    pub(super) fn remove_entry(&self, id: &str, finish_action: Option<&str>) -> OpResult<()> {
        self.store().end_session(id, finish_action)?;
        self.run_on_close(id);
        self.entries.lock().unwrap().remove(id);
        self.tamper_warned.lock().unwrap().remove(id);
        // 分頁關閉後紀錄檔就沒有人會看了（每個最多約 100 MB，且可能含有機密）。
        // Windows 上 Agent 剛被結束時檔案可能還開著，刪不掉就留著，只記錄。
        let logs = self.paths.logs();
        for name in [format!("{id}.log"), format!("{id}.log.1")] {
            match std::fs::remove_file(logs.join(&name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::debug!("無法刪除紀錄檔 {name}：{e}"),
            }
        }
        let _ = self
            .events
            .send(ServerMsg::SessionRemoved { id: id.to_string() });
        self.touch();
        Ok(())
    }

    pub(super) fn others_share(&self, id: &str, worktree: &str) -> bool {
        self.entries
            .lock()
            .unwrap()
            .values()
            .any(|e| e.info.id != id && e.info.worktree.as_deref() == Some(worktree))
    }

    /// discard 前的安全檢查（規格 7.4）。
    pub(super) fn check_discard(&self, info: &SessionInfo, force: bool) -> OpResult<()> {
        let (Some(wt), Some(base)) = (&info.worktree, &info.base_branch) else {
            return Ok(());
        };
        if self.others_share(&info.id, wt) {
            return Err(OpError::new(
                codes::BAD_REQUEST,
                "還有其他分頁共用這個 worktree，不能移除",
            ));
        }
        if force || !Path::new(wt).exists() {
            return Ok(());
        }
        let Some(repo) = &info.repo_path else {
            return Ok(());
        };
        let (repo, wt) = (Path::new(repo), Path::new(wt));
        // .git 被改動過：無法檢查內容，但仍要讓使用者能確認後丟棄
        if let Err(e) = agt_git::trusted_gitdir(repo, wt) {
            return Err(OpError::new(
                codes::NEEDS_CONFIRM,
                format!("{e}\n\n無法檢查是否有未 commit 的變更，確定要丟棄嗎？"),
            ));
        }
        let dirty = agt_git::is_dirty_wt(repo, wt, true)?;
        let unpushed = agt_git::has_unpushed(repo, wt, base)?;
        if dirty || unpushed {
            let mut why = vec![];
            if dirty {
                why.push("未 commit 的變更");
            }
            if unpushed {
                why.push("未 push / 未合併的 commit");
            }
            return Err(OpError::new(
                codes::NEEDS_CONFIRM,
                format!("worktree 有{}，確定要丟棄嗎？", why.join("與")),
            ));
        }
        Ok(())
    }

    pub(super) fn discard_worktree(&self, info: &SessionInfo) -> OpResult<()> {
        if info.shared {
            return Ok(()); // 共用者不負責移除
        }
        let (Some(wt), Some(repo)) = (&info.worktree, &info.repo_path) else {
            return Ok(());
        };
        agt_git::remove_worktree(Path::new(repo), Path::new(wt), true)?;
        if let Some(b) = &info.branch {
            agt_git::delete_branch(Path::new(repo), b)?;
        }
        self.store().clear_worktree(&info.id)?;
        Ok(())
    }

    pub(super) fn stop_agent(&self, id: &str) {
        if let Some(s) = self.session(id)
            && s.is_running()
        {
            s.kill();
        }
    }

    /// 關閉分頁。`cleanup` 時一併移除 worktree（需通過 discard 的安全檢查）。
    pub fn kill(&self, id: &str, cleanup: bool, force: bool) -> OpResult<()> {
        let id = self.resolve(id)?;
        let info = self.get(&id)?;
        if cleanup {
            self.check_discard(&info, force)?;
        }
        tracing::debug!(%id, "kill：結束 Agent");
        self.stop_agent(&id);
        tracing::debug!(%id, "kill：Agent 已送出結束訊號");
        if cleanup {
            self.discard_worktree(&info)?;
        }
        // 註記留下的副本：之後 `agt gc` 與開啟 TUI 時會提示清理（closed_worktree）
        let left = (!cleanup && !info.shared)
            .then_some(info.worktree.as_deref())
            .flatten();
        self.record(
            &id,
            "status",
            json!({"to": "closed", "cleanup": cleanup, "worktree_left": left}),
        );
        self.remove_entry(&id, if cleanup { Some("discard") } else { None })
    }

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
