//! 建立工作階段：worktree、啟動 Agent、監看事件。

use super::*;

impl Daemon {
    // ───────────────────────── 建立 ─────────────────────────

    /// 建立工作階段（規格 7.1）。任何一步失敗都會回滾已完成的步驟（setup 除外）。
    pub fn create(self: &Arc<Self>, req: CreateSession) -> OpResult<SessionInfo> {
        let cfg = self.cfg();
        let settings = cfg.agents.get(&req.agent).cloned().ok_or_else(|| {
            OpError::new(
                codes::BAD_REQUEST,
                format!(
                    "未知的 Agent：{}（可用：{}）",
                    req.agent,
                    cfg.agents.keys().cloned().collect::<Vec<_>>().join("、")
                ),
            )
        })?;
        if let Some(msg) = platform::docker_unavailable(&settings.command) {
            return Err(OpError::new(codes::BAD_REQUEST, msg));
        }
        let dir = PathBuf::from(&req.repo);
        if !dir.is_dir() {
            return Err(OpError::new(
                codes::BAD_REQUEST,
                format!("目錄不存在：{}", req.repo),
            ));
        }
        let dir = agt_git::normalize(&dir);
        let id = uuid::Uuid::new_v4().to_string();
        let short_id = loop {
            let s = agt_git::short_id(random_u64());
            if !self.store().short_id_exists(&s)? {
                break s;
            }
        };
        let task = if req.task.trim().is_empty() {
            req.agent.clone()
        } else {
            req.task.trim().to_string()
        };

        let mut info = SessionInfo {
            id: id.clone(),
            short_id: short_id.clone(),
            agent: req.agent.clone(),
            repo_path: None,
            repo_name: None,
            task: task.clone(),
            worktree: None,
            branch: None,
            base_branch: None,
            cwd: dir.to_string_lossy().into_owned(),
            status: AgentStatus::Starting,
            exit_code: None,
            ahead: 0,
            behind: 0,
            dirty: false,
            created_at: agt_store::now(),
            shared: false,
        };
        let mut created_wt: Option<(PathBuf, PathBuf, String)> = None; // (repo, worktree, branch)
        let mut setup: Option<String> = None;

        if let Some(share) = &req.share {
            // 共用 worktree：只有明確加上 --share 才允許（規格 7.5）
            let other = self.get(share)?;
            let Some(wt) = other.worktree.clone() else {
                return Err(OpError::new(
                    codes::BAD_REQUEST,
                    format!("工作階段 {share} 沒有 worktree 可以共用"),
                ));
            };
            info.repo_path = other.repo_path.clone();
            info.repo_name = other.repo_name.clone();
            info.worktree = Some(wt.clone());
            info.branch = other.branch.clone();
            info.base_branch = other.base_branch.clone();
            info.cwd = wt;
            info.shared = true;
        } else {
            match agt_git::repo_info(&dir)? {
                None => {
                    // 不是 git repo：退化成一般分頁
                    info.repo_name = dir.file_name().map(|s| s.to_string_lossy().into_owned());
                    info.repo_path = Some(dir.to_string_lossy().into_owned());
                }
                Some(repo) => {
                    let root_s = repo.root.to_string_lossy().into_owned();
                    info.repo_path = Some(root_s.clone());
                    info.repo_name = Some(repo.name.clone());
                    let rcfg = cfg
                        .repo(&self.paths, &repo.root)
                        .map_err(|e| OpError::new(codes::BAD_REQUEST, e))?;
                    if settings.worktree {
                        let base = req
                            .base_branch
                            .clone()
                            .or(rcfg.base_branch.clone())
                            .or(repo.head_branch.clone())
                            .ok_or_else(|| {
                                OpError::new(
                                    codes::BAD_REQUEST,
                                    "目前是 detached HEAD，請用 --base 指定 base branch",
                                )
                            })?;
                        if !agt_git::ref_exists(&repo.root, &base) {
                            return Err(OpError::new(
                                codes::BAD_REQUEST,
                                format!("找不到 base branch：{base}"),
                            ));
                        }
                        // setup 來自 repo，第一次執行前要求使用者確認（規格 11.4）
                        if let Some(s) = rcfg.setup.clone().filter(|s| !s.trim().is_empty()) {
                            let trusted = self.store().is_setup_trusted(&root_s, &s)?;
                            if !trusted && !req.trust_setup {
                                return Err(OpError::new(codes::SETUP_UNTRUSTED, s));
                            }
                            if !trusted {
                                self.store().trust_setup(&root_s, &s)?;
                            }
                            setup = Some(s);
                        }
                        let branch = agt_git::branch_name(&req.agent, &task, &short_id);
                        let root = self.paths.expand(&cfg.general.worktree_root);
                        let wt = root
                            .join(agt_git::repo_dir_name(&repo.root))
                            .join(&short_id);
                        if platform::path_too_long(&wt) && !agt_git::longpaths_enabled(&repo.root) {
                            return Err(OpError::new(
                                codes::BAD_REQUEST,
                                format!(
                                    "worktree 路徑可能超過 Windows 的 260 字元限制：{}\n請執行 `git config core.longpaths true` 或在設定檔改用較短的 worktree_root",
                                    wt.display()
                                ),
                            ));
                        }
                        agt_git::add_worktree(&repo.root, &wt, &branch, &base)?;
                        let wt = agt_git::normalize(&wt);
                        created_wt = Some((repo.root.clone(), wt.clone(), branch.clone()));
                        if let Err(e) = agt_git::copy_files(&repo.root, &wt, &rcfg.copy_files) {
                            rollback(&created_wt);
                            return Err(e.into());
                        }
                        // Agent 寫的 commit / PR 說明檔不出現在 diff，也不會被 commit
                        if let Err(e) = agt_git::add_exclude(&wt, agt_git::NOTES_EXCLUDE) {
                            tracing::warn!("無法加入 info/exclude：{e}");
                        }
                        info.worktree = Some(wt.to_string_lossy().into_owned());
                        info.branch = Some(branch);
                        info.base_branch = Some(base);
                        info.cwd = wt.to_string_lossy().into_owned();
                    }
                }
            }
        }

        // 新分頁先執行更新指令（例如把 claude-docker 的 Claude Code 更新到最新版），再執行 repo 的 setup
        if settings.update_on_new
            && let Some(u) = platform::update_script(&settings.update)
        {
            setup = Some(match setup {
                Some(s) => format!("{u}\n{s}"),
                None => u,
            });
        }

        let conversation = new_conversation_id();
        let (session, events) = match self.start_session(
            &info,
            &conversation,
            false,
            false,
            setup.as_deref(),
            req.cols,
            req.rows,
        ) {
            Ok(s) => s,
            Err(e) => {
                rollback(&created_wt);
                return Err(e);
            }
        };

        let row = SessionRow {
            id: info.id.clone(),
            short_id: info.short_id.clone(),
            agent: info.agent.clone(),
            repo_path: info.repo_path.clone(),
            repo_name: info.repo_name.clone(),
            task: info.task.clone(),
            worktree_path: info.worktree.clone(),
            branch: info.branch.clone(),
            base_branch: info.base_branch.clone(),
            status: info.status.as_str().into(),
            pid: session.pid(),
            created_at: info.created_at,
            cwd: Some(info.cwd.clone()),
            shared: info.shared,
            conversation_id: Some(conversation.clone()),
            ..Default::default()
        };
        if let Err(e) = self.store().insert_session(&row) {
            session.kill();
            rollback(&created_wt);
            return Err(e.into());
        }
        self.record(
            &info.id,
            "status",
            json!({"to": "starting", "source": "daemon"}),
        );
        self.entries.lock().unwrap().insert(
            info.id.clone(),
            Entry {
                info: info.clone(),
                session: Some(session.clone()),
                conversation: Some(conversation),
                generation: 1,
                resume_attempt: None,
            },
        );
        self.watch(info.id.clone(), 1, session, events);
        self.publish(info.clone());
        self.touch();
        Ok(info)
    }

    pub(super) fn launch_ctx(
        &self,
        info: &SessionInfo,
        settings: &agt_agents::AgentSettings,
        conversation: &str,
        resume: bool,
        restarted: bool,
    ) -> LaunchCtx {
        // 只有設定用到 {gitdir}（要把主 repo 的 .git 掛進容器 / 沙箱）時，
        // 才把 worktree 的 .git 改成相對路徑並取得主 repo 的 .git 位置
        let wants_gitdir = settings
            .args
            .iter()
            .chain(&settings.restart_args)
            .chain(&settings.new_args)
            .chain(&settings.resume_args)
            .any(|a| a.contains("{gitdir}"));
        // 重新啟動時 worktree 的 .git 可能被上一次的 Agent 改過：先確認它指向主 repo 登記的 gitdir，
        // 否則 {gitdir} 會把 Agent 指定的任意目錄掛進新的沙箱。
        let trusted = info.repo_path.as_deref().is_some_and(|repo| {
            match agt_git::trusted_gitdir(Path::new(repo), Path::new(&info.cwd)) {
                Ok(_) => true,
                Err(e) => {
                    tracing::warn!("不掛載 {{gitdir}}：{e}");
                    false
                }
            }
        });
        let gitdir = if wants_gitdir && info.worktree.is_some() && trusted {
            agt_git::prepare_container_gitdir(Path::new(&info.cwd))
        } else {
            None
        };
        LaunchCtx {
            session_id: info.id.clone(),
            cwd: PathBuf::from(&info.cwd),
            agt_exe: self.agt_exe.clone(),
            socket: self.endpoint.clone(),
            conversation_id: conversation.to_string(),
            resume,
            restarted,
            gitdir,
        }
    }

    /// 關閉分頁時執行 Agent 設定的 `on_close`（例如刪除沙箱），在背景執行、不阻塞。
    pub(super) fn run_on_close(&self, id: &str) {
        let Some(info) = self.entries.lock().unwrap().get(id).map(|e| e.info.clone()) else {
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
                        if matches!(to, AgentStatus::WaitingInput | AgentStatus::Done)
                            && d.cfg().general.notify
                            && session.viewers() == 0
                        {
                            let what = if to == AgentStatus::WaitingInput {
                                "等待你的回覆"
                            } else {
                                "已完成"
                            };
                            let body = message.unwrap_or_else(|| format!("{} {what}", info.agent));
                            platform::notify(&format!("agt · {}", info.task), &body);
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
}
