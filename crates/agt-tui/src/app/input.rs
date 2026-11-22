//! 鍵盤、貼上、前綴鍵指令與捲動模式。

use super::*;

impl App {
    // ───────────────────────── 輸入 ─────────────────────────

    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Paste(s) => self.on_paste(&s),
            Event::Resize(..) => self.dirty = true,
            _ => {}
        }
    }

    pub(super) fn input(&self, bytes: &[u8]) {
        if let Some(id) = &self.active {
            let _ = self.client.input(id, bytes);
        }
    }

    pub(super) fn on_paste(&mut self, s: &str) {
        if let Some(p) = self.popup.as_mut() {
            if let Some(field) = text_field(p) {
                field.push_str(&s.replace(['\r', '\n'], " "));
                self.dirty = true;
            }
            return;
        }
        // 一律用 bracketed paste 包起來：即使 daemon 傳來的畫面快照還沒帶到
        // BRACKETED_PASTE 旗標，也不會把貼上內容的換行當成 Enter 而誤送出。
        self.input(&paste_bytes(s));
    }

    pub(super) fn on_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        if self.popup.is_some() {
            self.on_popup_key(k);
            return;
        }
        if self.prefix_armed {
            self.prefix_armed = false;
            self.on_command(k);
            return;
        }
        if self.prefix.matches(&k) {
            self.prefix_armed = true;
            return;
        }
        if self.scroll.is_some() {
            self.on_scroll_key(k);
            return;
        }
        let app_cursor = self
            .screen
            .as_ref()
            .is_some_and(|s| s.has_mode(modes::APP_CURSOR));
        if let Some(b) = encode_key(&k, app_cursor) {
            self.input(&b);
        }
    }

    /// 把目前畫面上最下面（最新）的網址複製到剪貼簿（OSC 52），例如登入網址。
    /// TUI 開啟了滑鼠模式、長網址又被折成多列，直接用滑鼠選取很難完整複製。
    pub(super) fn copy_url(&mut self) {
        match self.screen.as_ref().and_then(|s| s.urls().pop()) {
            Some(url) => {
                self.set_message(format!("已複製網址：{url}"), false);
                self.clipboard_out = Some(url);
            }
            None => self.set_message("目前畫面上沒有網址", true),
        }
    }

    pub(super) fn on_command(&mut self, k: KeyEvent) {
        if self.prefix.matches(&k) {
            self.input(&[self.prefix.byte()]);
            return;
        }
        match k.code {
            KeyCode::Char('n') => self.open_new_form(),
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char(c @ '1'..='9') => {
                let i = c as usize - '1' as usize;
                let id = self.ordered().get(i).map(|s| s.id.clone());
                if id.is_some() {
                    self.select(id);
                }
            }
            KeyCode::Char('w') => self.next_waiting(),
            KeyCode::Char(' ') | KeyCode::Char('p') => {
                self.popup = Some(Popup::Switch {
                    input: String::new(),
                    selected: 0,
                });
            }
            KeyCode::Char('d') => {
                if let Some(id) = self.active.clone() {
                    let c = self.client.clone();
                    self.spawn(async move {
                        AppEvent::Diff(c.request(ClientMsg::Diff { id }).await.map(|b| match b {
                            ReplyBody::Text(t) => t,
                            _ => String::new(),
                        }))
                    });
                }
            }
            KeyCode::Char('m') => {
                if self.active_info().is_some_and(|s| s.has_worktree()) {
                    self.popup = Some(Popup::Finish {
                        selected: 0,
                        preview: [vec!["讀取中…".into()], vec!["讀取中…".into()]],
                    });
                    for action in ["merge", "pr"] {
                        if let Some(id) = self.active.clone() {
                            let c = self.client.clone();
                            self.spawn(async move {
                                let r = c
                                    .request(ClientMsg::FinishPreview {
                                        id: id.clone(),
                                        action: action.into(),
                                    })
                                    .await
                                    .map(|b| match b {
                                        ReplyBody::Text(t) => t,
                                        _ => String::new(),
                                    });
                                AppEvent::FinishPreview(id, action, r)
                            });
                        }
                    }
                } else if self.active.is_some() {
                    self.set_message("這個分頁沒有 worktree", true);
                }
            }
            KeyCode::Char('s') => match self.active_info() {
                Some(s) if s.has_worktree() => {
                    let id = s.id.clone();
                    let c = self.client.clone();
                    self.set_message("同步中…", false);
                    self.spawn(async move {
                        AppEvent::Synced(c.request(ClientMsg::Sync { id }).await.map(|b| match b {
                            ReplyBody::Text(t) => t,
                            _ => String::new(),
                        }))
                    });
                }
                Some(_) => self.set_message("這個分頁沒有 worktree", true),
                None => {}
            },
            KeyCode::Char('r') => {
                if let Some(s) = self.active_info() {
                    self.popup = Some(Popup::Rename {
                        input: s.task.clone(),
                    });
                }
            }
            KeyCode::Char('t') => {
                if let Some(s) = self.active_info() {
                    self.popup = Some(Popup::Tags {
                        input: s.tags.join(" "),
                    });
                }
            }
            KeyCode::Char('R') => {
                if let Some(id) = self.active.clone() {
                    self.simple(
                        ClientMsg::Restart { id, fresh: false },
                        "已重新啟動 Agent（接續上次的對話）",
                    );
                }
            }
            KeyCode::Char('N') => {
                if let Some(id) = self.active.clone() {
                    self.simple(
                        ClientMsg::Restart { id, fresh: true },
                        "已重新啟動 Agent（新對話）",
                    );
                }
            }
            KeyCode::Char('[') => {
                if self.active.is_some() {
                    self.scroll = Some(Scroll {
                        searching: None,
                        last_query: String::new(),
                    });
                }
            }
            KeyCode::Char('x') => {
                if let Some(s) = self.active_info() {
                    let id = s.id.clone();
                    let busy = (s.status.is_busy() || s.status == AgentStatus::WaitingInput)
                        .then(|| s.agent.clone());
                    if s.worktree.is_some() && !s.shared {
                        self.popup = Some(Popup::Close {
                            id,
                            busy,
                            selected: 0,
                        });
                    } else if let Some(agent) = busy {
                        self.popup = Some(Popup::Confirm {
                            text: format!("{agent} 仍在執行中，確定要關閉這個分頁嗎？(y/n)"),
                            action: Confirm::Close { id },
                        });
                    } else {
                        self.close(id);
                    }
                }
            }
            KeyCode::Char('u') => self.copy_url(),
            KeyCode::Char('o') => self.open_active_dir(),
            KeyCode::Char('b') => self.sidebar_toggled = !self.sidebar_toggled,
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    pub(super) fn open_new_form(&mut self) {
        let mut agents = self.ui.agents.clone();
        if agents.is_empty() {
            agents = UiConfig::default().agents;
        }
        let (repo, agent_name) = match self.active_info() {
            Some(s) => (
                s.repo_path.clone().unwrap_or_else(|| self.cwd.clone()),
                s.agent.clone(),
            ),
            None => (self.cwd.clone(), "claude".into()),
        };
        let agent = agents.iter().position(|a| *a == agent_name).unwrap_or(0);
        self.popup = Some(Popup::New(NewForm {
            agents,
            agent,
            repo,
            task: String::new(),
            base: String::new(),
            field: 2,
            browser: None,
            templates: self.ui.templates.clone(),
            template: None,
        }));
    }

    pub(super) fn scroll_by(&mut self, delta: i32) {
        if let Some(id) = self.active.clone() {
            let _ = self.client.send(ClientMsg::Scroll { id, delta });
        }
    }

    pub(super) fn on_scroll_key(&mut self, k: KeyEvent) {
        let page = self.main_area.height.saturating_sub(1).max(1) as i32;
        let sc = self.scroll.as_mut().unwrap();
        if let Some(q) = sc.searching.as_mut() {
            match k.code {
                KeyCode::Enter => {
                    let q = std::mem::take(q);
                    sc.searching = None;
                    sc.last_query = q.clone();
                    self.search(q);
                }
                KeyCode::Esc => sc.searching = None,
                KeyCode::Backspace => {
                    q.pop();
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => q.push(c),
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(-1),
            KeyCode::PageUp | KeyCode::Char('u') => self.scroll_by(page),
            KeyCode::PageDown | KeyCode::Char('d') => self.scroll_by(-page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll_by(i32::MAX / 2),
            KeyCode::End | KeyCode::Char('G') => self.scroll_by(i32::MIN),
            KeyCode::Char('/') => sc.searching = Some(String::new()),
            KeyCode::Char('n') => {
                let q = sc.last_query.clone();
                if !q.is_empty() {
                    self.search(q);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.scroll = None;
                self.scroll_by(i32::MIN);
            }
            _ => {}
        }
    }

    pub(super) fn search(&mut self, query: String) {
        let Some(id) = self.active.clone() else {
            return;
        };
        let c = self.client.clone();
        self.spawn(async move {
            match c.request(ClientMsg::Search { id, query }).await {
                Ok(ReplyBody::Found(f)) => AppEvent::Searched(f),
                Ok(_) => AppEvent::Searched(false),
                Err(e) => AppEvent::Error(e.to_string()),
            }
        });
    }
}
