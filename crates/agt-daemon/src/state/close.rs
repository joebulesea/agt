//! 關閉分頁：結束 Agent、移除 / 保留 worktree。

use super::*;

impl Daemon {
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
}
