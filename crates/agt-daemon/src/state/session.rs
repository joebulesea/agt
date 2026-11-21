//! 分頁的執行：啟動、狀態監看、通知、重新命名與重新啟動。

use super::*;

impl Daemon {
    /// 關閉分頁時執行 Agent 設定的 `on_close`（例如刪除沙箱），在背景執行、不阻塞。
    pub(super) fn run_on_close(&self, id: &str) {
        let Some(info) = self
            .entries
            .lock_or_recover()
            .get(id)
            .map(|e| e.info.clone())
        else {
            return;
        };
        let cfg = self.cfg();
        let Some(settings) = cfg.agents.get(&info.agent) else {
            return;
        };
        if settings.on_close.is_empty() {
            return;
        }
        let ctx = LaunchCtx {
            session_id: info.id.clone(),
            cwd: PathBuf::from(&info.cwd),
            agt_exe: self.agt_exe.clone(),
            socket: self.endpoint.clone(),
            conversation_id: String::new(),
            resume: false,
            restarted: false,
            gitdir: None,
        };
        let cmd = agt_agents::on_close_command(settings, &ctx);
        tracing::debug!(%id, ?cmd, "執行 on_close");
        let _ = std::thread::Builder::new()
            .name("agt-on-close".into())
            .spawn(move || platform::run_cleanup(&cmd));
    }

    /// 安裝 hooks 並在 cwd 啟動 Agent。`resume` = 以 `resume_args` 接續 `conversation`；
    /// `restarted` = 這個分頁之前啟動過（`R` / `N` 重新啟動），有設定 `restart_args` 時用它取代 `args`。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_session(
        &self,
        info: &SessionInfo,
        conversation: &str,
        resume: bool,
        restarted: bool,
        setup: Option<&str>,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> OpResult<(Arc<Session>, broadcast::Receiver<SessionEvent>)> {
        let cfg = self.cfg();
        let settings = cfg.agents.get(&info.agent).cloned().ok_or_else(|| {
            OpError::new(codes::BAD_REQUEST, format!("未知的 Agent：{}", info.agent))
        })?;
        let adapter = agt_agents::build(&info.agent, &settings)
            .map_err(|e| OpError::new(codes::BAD_REQUEST, e))?;
        let ctx = self.launch_ctx(info, &settings, conversation, resume, restarted);
        adapter.install_hooks(&ctx)?;
        let mut spec = adapter.launch(&ctx);
        if let Some(s) = setup {
            spec = platform::wrap_with_setup(s, spec);
        }
        let log: Option<Box<dyn std::io::Write + Send>> = if cfg.general.log_output {
            match LogWriter::open(&self.paths.logs(), &info.id, LOG_MAX_BYTES) {
                Ok(w) => Some(Box::new(w)),
                Err(e) => {
                    tracing::warn!("無法開啟紀錄檔：{e}");
                    None
                }
            }
        } else {
            None
        };
        let started = Session::spawn(SessionOptions {
            id: info.id.clone(),
            cols: cols.unwrap_or(100),
            rows: rows.unwrap_or(30),
            scrollback: cfg.general.scrollback_lines,
            spec,
            adapter,
            log,
        })
        .map_err(|e| OpError::new(codes::BAD_REQUEST, e.to_string()))?;
        Ok(started)
    }

    /// 監聽工作階段事件：更新資料庫、推播 `StatusChanged`、發出系統通知。
    pub(super) fn watch(
        self: &Arc<Self>,
        id: String,
        generation: u64,
        session: Arc<Session>,
        mut rx: broadcast::Receiver<SessionEvent>,
    ) {
        let d = self.clone();
        tokio::spawn(async move {
            loop {
                let ev = match rx.recv().await {
                    Ok(ev) => ev,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                match ev {
                    SessionEvent::Status {
                        from,
                        to,
                        source,
                        message,
                    } => {
                        let Some(info) = d.update_if_current(&id, generation, |i| i.status = to)
                        else {
                            break; // 分頁已關閉，或已重新啟動（這是舊 Agent 的事件）
                        };
                        if let Err(e) = d.store().update_status(&id, to.as_str()) {
                            tracing::warn!("更新狀態失敗：{e}");
                        }
                        match to {
                            AgentStatus::Running | AgentStatus::Thinking => {
                                d.busy.lock_or_recover().enter(&id, Instant::now());
                            }
                            _ => d.busy.lock_or_recover().leave(&id),
                        }
                        d.record(
                            &id,
                            "status",
                            json!({
                                "from": from.as_str(), "to": to.as_str(),
                                "source": format!("{source:?}").to_lowercase(), "message": message,
                            }),
                        );
                        let _ = d.events.send(ServerMsg::StatusChanged {
                            id: id.clone(),
                            from,
                            to,
                        });
                        // 使用者目前正在看的分頁不發系統通知
                        if d.cfg().general.should_notify(to) && session.viewers() == 0 {
                            let what = match to {
                                AgentStatus::WaitingInput => "等待你的回覆",
                                AgentStatus::Error => "發生錯誤",
                                AgentStatus::Exited => "已結束",
                                _ => "已完成",
                            };
                            let body = message.unwrap_or_else(|| format!("{} {what}", info.agent));
                            platform::notify(
                                &format!("agt · {}", info.task),
                                &body,
                                Some(id.as_str()),
                            );
                        }
                    }
                    SessionEvent::Notify { title, body } => {
                        d.record(
                            &id,
                            "notify",
                            json!({"title": title, "body": body, "source": "osc"}),
                        );
                    }
                    SessionEvent::Title(_) => {}
                    SessionEvent::Clipboard(text) => {
                        // 上限 64 KiB：一般複製用不到更多，也避免異常輸出塞爆連線。
                        // 設定為 off 時直接丟棄；ask / on 由 TUI 處理（ask 先詢問使用者）。
                        if text.len() <= CLIPBOARD_MAX && d.cfg().general.osc52 != "off" {
                            let _ = d.events.send(ServerMsg::Clipboard {
                                id: id.clone(),
                                text,
                            });
                        }
                    }
                    SessionEvent::Exited { code } => {
                        let st = session.status();
                        if let Some(info) = d.update_if_current(&id, generation, |i| {
                            i.exit_code = code;
                            i.status = st;
                        }) {
                            let _ = d.store().update_process(&id, session.pid(), code);
                            let _ = d.store().update_status(&id, st.as_str());
                            d.publish(info);
                        }
                        d.touch();
                        if d.resume_failed(&id, generation, code) {
                            tracing::info!("{id}：接續對話失敗（exit {code:?}），改開新對話");
                            d.record(
                                &id,
                                "status",
                                json!({"to": "starting", "source": "resume_fallback"}),
                            );
                            let d = d.clone();
                            tokio::task::spawn_blocking(move || {
                                if let Err(e) = d.restart(&id, true) {
                                    tracing::warn!("{id}：改開新對話失敗：{}", e.message);
                                }
                            });
                        }
                        break;
                    }
                }
            }
        });
    }

    /// 分頁連續忙碌超過 `long_running_notify_secs` 時提醒一次（每 3 秒由背景工作呼叫）。
    /// 你正在看的分頁不通知。
    pub fn check_long_running(&self) {
        let cfg = self.cfg();
        let secs = cfg.general.long_running_notify_secs;
        if secs == 0 || !cfg.general.notify {
            return;
        }
        let due = self.busy.lock_or_recover().take_due(Instant::now(), secs);
        for id in due {
            let (info, viewers) = {
                let entries = self.entries.lock_or_recover();
                match entries.get(&id) {
                    Some(e) => (
                        e.info.clone(),
                        e.session.as_ref().map_or(0, |s| s.viewers()),
                    ),
                    None => continue,
                }
            };
            if viewers > 0 {
                continue;
            }
            platform::notify(
                &format!("agt · {}", info.task),
                &format!("{} 已連續執行超過 {}", info.agent, busy::format_secs(secs)),
                Some(id.as_str()),
            );
        }
    }

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

    /// 取代分頁的標籤（空清單 = 清除）。
    pub fn set_tags(&self, id: &str, tags: &[String]) -> OpResult<SessionInfo> {
        let id = self.resolve(id)?;
        let tags =
            agt_proto::tags::normalize(tags).map_err(|e| OpError::new(codes::BAD_REQUEST, e))?;
        self.store().set_tags(&id, &tags)?;
        let info = self
            .update_info(&id, |i| i.tags = tags.clone())
            .ok_or_else(|| OpError::not_found(&id))?;
        self.publish(info.clone());
        Ok(info)
    }

    /// Agent 結束時呼叫：剛以 `resume_args` 接續、很快就以錯誤結束 → true（應改開新對話）。
    pub(super) fn resume_failed(&self, id: &str, generation: u64, code: Option<i32>) -> bool {
        let mut entries = self.entries.lock_or_recover();
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
        let _one_at_a_time = self.restart_lock.lock_or_recover();
        let id = self.resolve(id)?;
        let (info, old, conversation) = {
            let entries = self.entries.lock_or_recover();
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
            let mut entries = self.entries.lock_or_recover();
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
            let mut entries = self.entries.lock_or_recover();
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
}
