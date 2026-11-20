//! worktree 與資料庫的比對與清理（gc）、定期輪詢 git 狀態。

use super::*;

impl Daemon {
    /// 孤兒偵測（規格 7.5）：比對 `git worktree list` 與資料庫。
    pub fn gc(&self, prune: bool) -> OpResult<Vec<GcItem>> {
        let rows = self.store().sessions_with_worktree()?;
        let mut repos: Vec<String> = rows.iter().filter_map(|r| r.repo_path.clone()).collect();
        repos.sort();
        repos.dedup();
        let known: Vec<&SessionRow> = rows.iter().collect();
        let mut items = Vec::new();
        for repo in &repos {
            let repo_p = Path::new(repo);
            if !repo_p.is_dir() {
                continue;
            }
            let list = match agt_git::list_worktrees(repo_p) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("無法列出 {repo} 的 worktree：{e}");
                    continue;
                }
            };
            for wt in &list {
                let Some(branch) = &wt.branch else { continue };
                if !branch.starts_with(agt_git::BRANCH_PREFIX) {
                    continue;
                }
                let tracked = known.iter().any(|r| {
                    r.worktree_path
                        .as_deref()
                        .is_some_and(|p| agt_git::same_path(Path::new(p), &wt.path))
                });
                if !tracked {
                    items.push(GcItem {
                        kind: "orphan_worktree".into(),
                        repo: repo.clone(),
                        path: wt.path.to_string_lossy().into_owned(),
                        branch: Some(branch.clone()),
                        session: None,
                    });
                }
            }
            for r in known
                .iter()
                .filter(|r| r.repo_path.as_deref() == Some(repo.as_str()))
            {
                let Some(p) = &r.worktree_path else { continue };
                let in_git = list
                    .iter()
                    .any(|w| agt_git::same_path(&w.path, Path::new(p)));
                if !in_git || !Path::new(p).exists() {
                    items.push(GcItem {
                        kind: "missing_worktree".into(),
                        repo: repo.clone(),
                        path: p.clone(),
                        branch: r.branch.clone(),
                        session: Some(r.short_id.clone()),
                    });
                } else if r.ended_at.is_some() && !r.shared && !in_use(&known, r, p) {
                    // 分頁已關閉，但 worktree 副本還留在磁碟上（關閉時選了保留）
                    items.push(GcItem {
                        kind: "closed_worktree".into(),
                        repo: repo.clone(),
                        path: p.clone(),
                        branch: r.branch.clone(),
                        session: Some(r.short_id.clone()),
                    });
                }
            }
        }
        if prune {
            for it in &items {
                let repo = Path::new(&it.repo);
                match it.kind.as_str() {
                    // 只移除乾淨的 worktree；有變更時 git 會拒絕，保留給使用者決定。branch 保留。
                    "orphan_worktree" => {
                        if let Err(e) = agt_git::remove_worktree(repo, Path::new(&it.path), false) {
                            tracing::warn!("無法移除 {}：{e}", it.path);
                        }
                    }
                    "closed_worktree" => self.prune_closed_worktree(it)?,
                    _ => {
                        let _ = agt_git::prune(repo);
                        if let Some(sid) = &it.session {
                            let row = self.store().get_session(sid)?;
                            if let Some(row) = row {
                                self.store().clear_worktree(&row.id)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(items)
    }

    /// 移除已關閉分頁留下的 worktree 副本。只移除乾淨的副本（有變更時 git 會拒絕，保留給使用者決定）；
    /// branch 只在沒有 base 以外、也沒有 push 的 commit 時刪除，否則保留。
    pub(super) fn prune_closed_worktree(&self, it: &GcItem) -> OpResult<()> {
        let (repo, wt) = (Path::new(&it.repo), Path::new(&it.path));
        let Some(row) = it
            .session
            .as_deref()
            .map(|sid| self.store().get_session(sid))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        // .git 被改動過就不在裡面執行 git，留給使用者處理
        if let Err(e) = agt_git::trusted_gitdir(repo, wt) {
            tracing::warn!("不清理 {}：{e}", it.path);
            return Ok(());
        }
        let keep_branch = match &row.base_branch {
            Some(base) => agt_git::has_unpushed(repo, wt, base).unwrap_or(true),
            None => true,
        };
        if let Err(e) = agt_git::remove_worktree(repo, wt, false) {
            tracing::warn!("無法移除 {}：{e}", it.path);
            return Ok(());
        }
        if !keep_branch
            && let Some(b) = &row.branch
            && let Err(e) = agt_git::delete_branch(repo, b)
        {
            tracing::warn!("無法刪除 branch {b}：{e}");
        }
        self.store().clear_worktree(&row.id)?;
        Ok(())
    }

    /// 定期更新 git 資訊，有變化時推播 `WorktreeChanged`。
    pub fn poll_git(&self) {
        let targets: Vec<(String, String, String, String)> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter_map(|e| {
                Some((
                    e.info.id.clone(),
                    e.info.repo_path.clone()?,
                    e.info.worktree.clone()?,
                    e.info.base_branch.clone()?,
                ))
            })
            .collect();
        for (id, repo, wt, base) in targets {
            if !Path::new(&wt).is_dir() {
                continue;
            }
            let st = match agt_git::status(Path::new(&repo), Path::new(&wt), &base) {
                Ok(st) => st,
                Err(e) => {
                    // 沙箱裡的 Agent 改了 .git：不執行 git，並提醒使用者一次
                    if agt_git::trusted_gitdir(Path::new(&repo), Path::new(&wt)).is_err()
                        && self.tamper_warned.lock().unwrap().insert(id.clone())
                    {
                        tracing::warn!("{id}：{e}");
                        let _ = self.events.send(ServerMsg::Error {
                            code: codes::GIT.into(),
                            message: e.to_string(),
                        });
                    }
                    continue;
                }
            };
            let changed = {
                let mut entries = self.entries.lock().unwrap();
                match entries.get_mut(&id) {
                    Some(e)
                        if (e.info.ahead, e.info.behind, e.info.dirty)
                            != (st.ahead, st.behind, st.dirty) =>
                    {
                        e.info.ahead = st.ahead;
                        e.info.behind = st.behind;
                        e.info.dirty = st.dirty;
                        true
                    }
                    _ => false,
                }
            };
            if changed {
                let _ = self.events.send(ServerMsg::WorktreeChanged {
                    id,
                    ahead: st.ahead,
                    behind: st.behind,
                    dirty: st.dirty,
                });
            }
        }
    }
}
