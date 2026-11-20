//! 對 daemon 的非同步請求（建立、收尾、關閉、清理）。

use super::*;

impl App {
    // ───────────────────────── 非同步請求 ─────────────────────────

    pub(super) fn spawn<F>(&self, f: F)
    where
        F: std::future::Future<Output = AppEvent> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(f.await);
        });
    }

    pub(super) fn create(&mut self, req: CreateSession) {
        let c = self.client.clone();
        self.set_message(format!("建立 {} 分頁中…", req.agent), false);
        self.spawn(async move {
            let r = c
                .request(ClientMsg::CreateSession(req.clone()))
                .await
                .and_then(client::expect_session);
            match r {
                Ok(info) => AppEvent::Created(Box::new(info)),
                Err(e) => AppEvent::CreateFailed(Box::new((e, req))),
            }
        });
    }

    pub(super) fn finish(&mut self, action: FinishAction, force: bool) {
        let Some(id) = self.active.clone() else {
            return;
        };
        self.finish_id(id, action, force);
    }

    pub(super) fn finish_id(&mut self, id: String, action: FinishAction, force: bool) {
        let c = self.client.clone();
        self.set_message(format!("{} 執行中…", action.name()), false);
        self.spawn(async move {
            let r = c
                .request(ClientMsg::Finish {
                    id,
                    action: action.clone(),
                    force,
                })
                .await
                .map(|b| match b {
                    ReplyBody::Text(t) => t,
                    _ => String::new(),
                });
            AppEvent::Finished(action, r)
        });
    }

    pub(super) fn close(&mut self, id: String) {
        let left = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .filter(|s| !s.shared)
            .and_then(|s| s.worktree.clone());
        if let Some(wt) = left {
            self.set_message(
                format!(
                    "已保留 worktree 副本 {wt}；下次開啟 agt 時會提示清理，或執行 agt gc --prune"
                ),
                false,
            );
        }
        let c = self.client.clone();
        self.spawn(async move {
            AppEvent::Closed(
                c.request(ClientMsg::Kill {
                    id,
                    cleanup: false,
                    force: false,
                })
                .await
                .map(|_| ()),
            )
        });
    }

    /// 開啟 TUI 時檢查有沒有留下來的 worktree 副本，有的話詢問是否清理。
    pub fn check_leftovers(&mut self) {
        let c = self.client.clone();
        self.spawn(async move {
            match c.request(ClientMsg::Gc { prune: false }).await {
                Ok(ReplyBody::Gc(items)) => AppEvent::Leftovers(items),
                _ => AppEvent::Leftovers(vec![]),
            }
        });
    }

    pub(super) fn clean_leftovers(&mut self) {
        let c = self.client.clone();
        self.set_message("清理 worktree 副本中…", false);
        self.spawn(async move {
            if let Err(e) = c.request(ClientMsg::Gc { prune: true }).await {
                return AppEvent::Error(e.to_string());
            }
            match c.request(ClientMsg::Gc { prune: false }).await {
                Ok(ReplyBody::Gc(left)) if !left.is_empty() => AppEvent::Info(format!(
                    "已清理；{} 個副本有未 commit 的變更，已保留（執行 agt gc 查看）",
                    left.len()
                )),
                Ok(_) => AppEvent::Info("已清理留下的 worktree 副本".into()),
                Err(e) => AppEvent::Error(e.to_string()),
            }
        });
    }

    pub(super) fn simple(&mut self, msg: ClientMsg, ok: &'static str) {
        let c = self.client.clone();
        self.spawn(async move {
            match c.request(msg).await {
                Ok(_) => AppEvent::Info(ok.into()),
                Err(e) => AppEvent::Error(e.to_string()),
            }
        });
    }
}
