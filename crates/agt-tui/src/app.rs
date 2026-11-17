//! TUI 狀態與事件處理。指令可以透過前綴鍵或滑鼠點擊按鈕觸發，其餘按鍵原樣送給 Agent（規格 8）。

use crate::keys::{Prefix, encode_key, encode_mouse};
use agt_proto::client::{Client, ClientError, Incoming};
use agt_proto::screen::modes;
use agt_proto::*;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// 非同步請求的結果，送回主迴圈處理。
pub enum AppEvent {
    Created(Box<SessionInfo>),
    CreateFailed(Box<(ClientError, CreateSession)>),
    Finished(FinishAction, Result<String, ClientError>),
    Diff(Result<String, ClientError>),
    Searched(bool),
    Closed(Result<(), ClientError>),
    Error(String),
    Info(String),
}

pub enum Confirm {
    Close {
        id: String,
    },
    /// 收尾動作需要確認（丟棄未 commit 的變更、合併會自動執行的設定檔）：確認後加上 force 重送。
    Finish {
        id: String,
        action: FinishAction,
    },
    TrustSetup(CreateSession),
}

pub struct NewForm {
    pub agents: Vec<String>,
    pub agent: usize,
    pub repo: String,
    pub task: String,
    pub base: String,
    pub field: usize,
    /// 開啟中的目錄瀏覽器（選擇 Repo 用）。
    pub browser: Option<crate::dirs::DirBrowser>,
}

impl NewForm {
    /// 開啟目錄瀏覽器，從目前 Repo 欄位的路徑開始。
    pub fn open_browser(&mut self, cwd: &str) {
        self.field = 1;
        self.browser = Some(crate::dirs::DirBrowser::open(&expand_home(&self.repo), cwd));
    }

    /// 目錄瀏覽器的按鍵。
    fn browser_key(&mut self, k: &KeyEvent) {
        use crate::dirs::Pick;
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        let chosen = match k.code {
            KeyCode::Esc => {
                self.browser = None;
                return;
            }
            KeyCode::Up => {
                b.move_by(-1);
                None
            }
            KeyCode::Down => {
                b.move_by(1);
                None
            }
            KeyCode::PageUp => {
                b.move_by(-10);
                None
            }
            KeyCode::PageDown => {
                b.move_by(10);
                None
            }
            KeyCode::Home => {
                b.selected = 0;
                None
            }
            KeyCode::End => {
                b.move_by(i32::MAX / 2);
                None
            }
            KeyCode::Enter | KeyCode::Right => match b.enter() {
                Pick::Chosen(p) => Some(p),
                Pick::Browsing => None,
            },
            // Tab：直接選定反白的目錄，不必先進入
            KeyCode::Tab => b.choose_selected(),
            KeyCode::Left => {
                b.up();
                None
            }
            KeyCode::Backspace => {
                if b.filter.is_empty() {
                    b.up();
                } else {
                    let mut f = b.filter.clone();
                    f.pop();
                    b.set_filter(f);
                }
                None
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut f = b.filter.clone();
                f.push(c);
                b.set_filter(f);
                None
            }
            _ => None,
        };
        if let Some(p) = chosen {
            self.repo = p.to_string_lossy().into_owned();
            self.browser = None;
            // 選好 repo 後接著輸入任務名稱
            self.field = 2;
        }
    }
}

pub enum Popup {
    Help,
    New(NewForm),
    Finish {
        selected: usize,
    },
    Strategy {
        selected: usize,
    },
    Confirm {
        text: String,
        action: Confirm,
    },
    Text {
        title: String,
        lines: Vec<String>,
        scroll: usize,
        diff: bool,
    },
    Rename {
        input: String,
    },
}

pub const FINISH_ITEMS: [(&str, &str); 4] = [
    ("merge", "合併進 base branch"),
    ("pr", "push 並建立 PR"),
    ("keep", "結束 Agent，保留 worktree 與 branch"),
    ("discard", "移除 worktree 並刪除 branch"),
];
pub const STRATEGIES: [MergeStrategy; 3] = [
    MergeStrategy::Merge,
    MergeStrategy::Squash,
    MergeStrategy::Rebase,
];

pub struct Scroll {
    pub searching: Option<String>,
    pub last_query: String,
}

/// 左側欄的一列（滑鼠點擊用）。
pub struct SidebarHit {
    pub y: u16,
    pub id: String,
}

/// 可以用滑鼠點擊的按鈕要做的事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Click {
    /// 等同按前綴鍵後再按這個字元。
    Command(char),
    /// 對目前的彈出視窗送出這個按鍵。
    Key(KeyCode),
    /// 新增分頁表單：聚焦第 N 個欄位。
    Field(usize),
    /// 新增分頁表單：切換 Agent（-1 上一個、1 下一個）。
    Agent(i8),
    /// 選單：選擇第 N 項並執行。
    Item(usize),
    /// 新增分頁表單：開啟目錄瀏覽器選擇 Repo。
    Browse,
    /// 吃掉點擊，不做任何事（彈出視窗本身的範圍）。
    Nothing,
}

/// 畫面上一個可點擊的範圍；後加入的優先（彈出視窗蓋在最上面）。
pub struct ClickHit {
    pub rect: Rect,
    pub click: Click,
}

pub struct App {
    pub client: Client,
    pub ui: UiConfig,
    pub prefix: Prefix,
    pub config_error: Option<String>,
    pub sessions: Vec<SessionInfo>,
    pub active: Option<String>,
    pub screen: Option<Screen>,
    pub prefix_armed: bool,
    pub sidebar_toggled: bool,
    pub popup: Option<Popup>,
    pub scroll: Option<Scroll>,
    pub message: Option<(String, bool, Instant)>,
    pub main_area: Rect,
    pub sidebar_area: Rect,
    pub sidebar_hits: Vec<SidebarHit>,
    pub click_hits: Vec<ClickHit>,
    /// 等待寫給外層終端機的剪貼簿內容（OSC 52），由主迴圈送出。
    pub clipboard_out: Option<String>,
    pub sent_size: Option<(String, u16, u16)>,
    pub quit: bool,
    pub dirty: bool,
    pub blink_on: bool,
    pub cwd: String,
    pub tx: mpsc::UnboundedSender<AppEvent>,
}

impl App {
    pub fn new(
        client: Client,
        ui: UiConfig,
        config_error: Option<String>,
        cwd: String,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> App {
        App {
            client,
            prefix: Prefix::parse(&ui.prefix_key),
            ui,
            config_error,
            sessions: vec![],
            active: None,
            screen: None,
            prefix_armed: false,
            sidebar_toggled: false,
            popup: None,
            scroll: None,
            message: None,
            main_area: Rect::default(),
            sidebar_area: Rect::default(),
            sidebar_hits: vec![],
            click_hits: vec![],
            clipboard_out: None,
            sent_size: None,
            quit: false,
            dirty: true,
            blink_on: true,
            cwd,
            tx,
        }
    }

    // ───────────────────────── 分頁 ─────────────────────────

    /// 依 repo 分組後的顯示順序（同一組維持建立順序）。
    pub fn ordered(&self) -> Vec<&SessionInfo> {
        let mut groups: Vec<(String, Vec<&SessionInfo>)> = vec![];
        for s in &self.sessions {
            let key = s.repo_path.clone().unwrap_or_default();
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => v.push(s),
                None => groups.push((key, vec![s])),
            }
        }
        groups.into_iter().flat_map(|(_, v)| v).collect()
    }

    pub fn active_info(&self) -> Option<&SessionInfo> {
        let id = self.active.as_ref()?;
        self.sessions.iter().find(|s| &s.id == id)
    }

    pub fn set_message(&mut self, m: impl Into<String>, error: bool) {
        self.message = Some((m.into(), error, Instant::now()));
        self.dirty = true;
    }

    pub fn upsert(&mut self, s: SessionInfo) {
        match self.sessions.iter_mut().find(|x| x.id == s.id) {
            Some(slot) => *slot = s,
            None => {
                self.sessions.push(s);
                self.sessions.sort_by_key(|s| s.created_at);
            }
        }
        self.dirty = true;
    }

    /// 切換到某個分頁：Detach 舊的、送出大小、Attach 新的。
    pub fn select(&mut self, id: Option<String>) {
        if self.active == id {
            return;
        }
        if let Some(old) = self.active.take() {
            let _ = self.client.send(ClientMsg::Detach { id: old });
        }
        self.screen = None;
        self.scroll = None;
        self.active = id.clone();
        self.sent_size = None;
        if let Some(id) = id {
            self.send_resize();
            let _ = self.client.send(ClientMsg::Attach { id });
        }
        self.dirty = true;
    }

    pub fn send_resize(&mut self) {
        let Some(id) = self.active.clone() else {
            return;
        };
        let (cols, rows) = (self.main_area.width.max(2), self.main_area.height.max(1));
        if self.sent_size.as_ref() == Some(&(id.clone(), cols, rows)) {
            return;
        }
        if self.main_area.width == 0 {
            return;
        }
        self.sent_size = Some((id.clone(), cols, rows));
        let _ = self.client.send(ClientMsg::Resize { id, cols, rows });
    }

    fn move_selection(&mut self, delta: i32) {
        let ids: Vec<String> = self.ordered().iter().map(|s| s.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let cur = self
            .active
            .as_ref()
            .and_then(|a| ids.iter().position(|x| x == a))
            .unwrap_or(0) as i32;
        let n = ids.len() as i32;
        let next = ((cur + delta) % n + n) % n;
        self.select(Some(ids[next as usize].clone()));
    }

    fn next_waiting(&mut self) {
        let ids: Vec<(String, AgentStatus)> = self
            .ordered()
            .iter()
            .map(|s| (s.id.clone(), s.status))
            .collect();
        let cur = self
            .active
            .as_ref()
            .and_then(|a| ids.iter().position(|(x, _)| x == a))
            .unwrap_or(0);
        for i in 1..=ids.len() {
            let (id, st) = &ids[(cur + i) % ids.len()];
            if *st == AgentStatus::WaitingInput {
                let id = id.clone();
                self.select(Some(id));
                return;
            }
        }
        self.set_message("沒有等待輸入的分頁", false);
    }

    // ───────────────────────── daemon 事件 ─────────────────────────

    pub fn on_incoming(&mut self, inc: Incoming) {
        match inc {
            Incoming::Screen(u) => {
                if Some(u.id()) != self.active.as_deref() {
                    return;
                }
                match u {
                    ScreenUpdate::Snapshot { screen, .. } => self.screen = Some(screen),
                    ScreenUpdate::Diff { diff, .. } => {
                        if let Some(s) = self.screen.as_mut() {
                            s.apply(&diff);
                        }
                    }
                }
                self.dirty = true;
            }
            Incoming::Msg(m) => self.on_msg(*m),
            Incoming::Closed => {
                self.quit = true;
                self.set_message("與 daemon 的連線已中斷", true);
            }
        }
    }

    fn on_msg(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::SessionUpdated { session } => self.upsert(session),
            ServerMsg::SessionRemoved { id } => {
                let order: Vec<String> = self.ordered().iter().map(|s| s.id.clone()).collect();
                let pos = order.iter().position(|x| *x == id);
                self.sessions.retain(|s| s.id != id);
                if self.active.as_deref() == Some(&id) {
                    self.active = None;
                    self.screen = None;
                    let order: Vec<String> = self.ordered().iter().map(|s| s.id.clone()).collect();
                    let next =
                        pos.and_then(|p| order.get(p.min(order.len().saturating_sub(1))).cloned());
                    self.select(next);
                }
            }
            ServerMsg::StatusChanged { id, to, .. } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.status = to;
                }
            }
            ServerMsg::WorktreeChanged {
                id,
                ahead,
                behind,
                dirty,
            } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.ahead = ahead;
                    s.behind = behind;
                    s.dirty = dirty;
                }
            }
            ServerMsg::ConfigStatus { error, ui } => {
                if let Some(ui) = ui {
                    self.prefix = Prefix::parse(&ui.prefix_key);
                    self.ui = ui;
                    self.sent_size = None;
                }
                self.config_error = error;
            }
            ServerMsg::Clipboard { id, text } => {
                // 只接受目前分頁的複製要求，背景分頁不能偷偷改剪貼簿
                if self.active.as_deref() == Some(id.as_str()) {
                    let n = text.chars().count();
                    self.clipboard_out = Some(text);
                    self.set_message(format!("已複製到剪貼簿（{n} 字）"), false);
                }
            }
            ServerMsg::DaemonStopping => {
                self.quit = true;
            }
            ServerMsg::Error { message, .. } => self.set_message(message, true),
            ServerMsg::Reply { .. } => {}
        }
        self.dirty = true;
    }

    pub fn on_app_event(&mut self, ev: AppEvent) {
        self.dirty = true;
        match ev {
            AppEvent::Created(info) => {
                let info = *info;
                self.message = None;
                let id = info.id.clone();
                self.upsert(info);
                self.select(Some(id));
            }
            AppEvent::CreateFailed(b) => {
                let (e, req) = *b;
                self.message = None;
                if e.code() == Some(codes::SETUP_UNTRUSTED) {
                    let cmd = match &e {
                        ClientError::Remote { message, .. } => message.clone(),
                        _ => String::new(),
                    };
                    self.popup = Some(Popup::Confirm {
                        text: format!(
                            "這個 repo 設定了 setup 指令，第一次執行前需要確認：\n\n  {cmd}\n\n要執行嗎？(y/n)"
                        ),
                        action: Confirm::TrustSetup(req),
                    });
                } else {
                    self.show_text("無法建立分頁", &e.to_string(), false);
                }
            }
            AppEvent::Finished(action, Ok(text)) => {
                if !text.is_empty() && !matches!(action, FinishAction::Keep) {
                    self.show_text(&format!("{} 完成", action.name()), &text, false);
                } else {
                    self.set_message(text, false);
                }
            }
            AppEvent::Finished(action, Err(e)) => match (e.code(), &action) {
                (Some(codes::NEEDS_CONFIRM), _) => {
                    if let Some(id) = self.active.clone() {
                        self.popup = Some(Popup::Confirm {
                            text: format!("{e}\n\n(y/n)"),
                            action: Confirm::Finish {
                                id,
                                action: action.clone(),
                            },
                        });
                    }
                }
                (Some(codes::NEED_STRATEGY), _) => {
                    self.popup = Some(Popup::Strategy { selected: 0 })
                }
                _ => self.show_text(&format!("{} 失敗", action.name()), &e.to_string(), false),
            },
            AppEvent::Diff(Ok(text)) => self.show_text("diff", &text, true),
            AppEvent::Diff(Err(e)) => self.set_message(e.to_string(), true),
            AppEvent::Searched(found) => {
                if !found {
                    self.set_message("找不到符合的內容", true);
                }
            }
            AppEvent::Closed(Ok(())) => {}
            AppEvent::Closed(Err(e)) => self.set_message(e.to_string(), true),
            AppEvent::Error(m) => self.set_message(m, true),
            AppEvent::Info(m) => self.set_message(m, false),
        }
    }

    fn show_text(&mut self, title: &str, text: &str, diff: bool) {
        self.popup = Some(Popup::Text {
            title: title.into(),
            lines: text.lines().map(|l| l.replace('\t', "    ")).collect(),
            scroll: 0,
            diff,
        });
    }

    // ───────────────────────── 非同步請求 ─────────────────────────

    fn spawn<F>(&self, f: F)
    where
        F: std::future::Future<Output = AppEvent> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(f.await);
        });
    }

    fn create(&mut self, req: CreateSession) {
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

    fn finish(&mut self, action: FinishAction, force: bool) {
        let Some(id) = self.active.clone() else {
            return;
        };
        self.finish_id(id, action, force);
    }

    fn finish_id(&mut self, id: String, action: FinishAction, force: bool) {
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

    fn close(&mut self, id: String) {
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

    fn simple(&mut self, msg: ClientMsg, ok: &'static str) {
        let c = self.client.clone();
        self.spawn(async move {
            match c.request(msg).await {
                Ok(_) => AppEvent::Info(ok.into()),
                Err(e) => AppEvent::Error(e.to_string()),
            }
        });
    }

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

    fn input(&self, bytes: &[u8]) {
        if let Some(id) = &self.active {
            let _ = self.client.input(id, bytes);
        }
    }

    fn on_paste(&mut self, s: &str) {
        if let Some(p) = self.popup.as_mut() {
            if let Some(field) = text_field(p) {
                field.push_str(&s.replace(['\r', '\n'], " "));
                self.dirty = true;
            }
            return;
        }
        let bracketed = self
            .screen
            .as_ref()
            .is_some_and(|s| s.has_mode(modes::BRACKETED_PASTE));
        if bracketed {
            let mut b = b"\x1b[200~".to_vec();
            b.extend_from_slice(s.as_bytes());
            b.extend_from_slice(b"\x1b[201~");
            self.input(&b);
        } else {
            self.input(s.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
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
    fn copy_url(&mut self) {
        match self.screen.as_ref().and_then(|s| s.urls().pop()) {
            Some(url) => {
                self.set_message(format!("已複製網址：{url}"), false);
                self.clipboard_out = Some(url);
            }
            None => self.set_message("目前畫面上沒有網址", true),
        }
    }

    fn on_command(&mut self, k: KeyEvent) {
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
                    self.popup = Some(Popup::Finish { selected: 0 });
                } else if self.active.is_some() {
                    self.set_message("這個分頁沒有 worktree", true);
                }
            }
            KeyCode::Char('r') => {
                if let Some(s) = self.active_info() {
                    self.popup = Some(Popup::Rename {
                        input: s.task.clone(),
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
                    if s.status.is_busy() || s.status == AgentStatus::WaitingInput {
                        self.popup = Some(Popup::Confirm {
                            text: format!(
                                "{} 仍在執行中，確定要關閉這個分頁嗎？\nworktree 會保留。(y/n)",
                                s.agent
                            ),
                            action: Confirm::Close { id },
                        });
                    } else {
                        self.close(id);
                    }
                }
            }
            KeyCode::Char('u') => self.copy_url(),
            KeyCode::Char('b') => self.sidebar_toggled = !self.sidebar_toggled,
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    fn open_new_form(&mut self) {
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
        }));
    }

    fn scroll_by(&mut self, delta: i32) {
        if let Some(id) = self.active.clone() {
            let _ = self.client.send(ClientMsg::Scroll { id, delta });
        }
    }

    fn on_scroll_key(&mut self, k: KeyEvent) {
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

    fn search(&mut self, query: String) {
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

    fn on_popup_key(&mut self, k: KeyEvent) {
        let Some(mut popup) = self.popup.take() else {
            return;
        };
        let esc = k.code == KeyCode::Esc;
        let keep = match &mut popup {
            Popup::Help => {
                !(esc
                    || matches!(
                        k.code,
                        KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter
                    ))
            }
            Popup::Text { lines, scroll, .. } => {
                let page = self.main_area.height.saturating_sub(4).max(1) as usize;
                let max = lines.len().saturating_sub(1);
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => false,
                    KeyCode::Down | KeyCode::Char('j') => {
                        *scroll = (*scroll + 1).min(max);
                        true
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *scroll = scroll.saturating_sub(1);
                        true
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        *scroll = (*scroll + page).min(max);
                        true
                    }
                    KeyCode::PageUp => {
                        *scroll = scroll.saturating_sub(page);
                        true
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        *scroll = 0;
                        true
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        *scroll = max;
                        true
                    }
                    _ => true,
                }
            }
            Popup::Rename { input } => match k.code {
                KeyCode::Esc => false,
                KeyCode::Enter => {
                    if let Some(id) = self.active.clone() {
                        let task = input.clone();
                        self.simple(ClientMsg::Rename { id, task }, "已重新命名");
                    }
                    false
                }
                _ => {
                    edit_text(input, &k);
                    true
                }
            },
            Popup::Finish { selected } => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => false,
                KeyCode::Up | KeyCode::Char('k') => {
                    *selected = (*selected + FINISH_ITEMS.len() - 1) % FINISH_ITEMS.len();
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1) % FINISH_ITEMS.len();
                    true
                }
                KeyCode::Enter => {
                    let sel = *selected;
                    self.choose_finish(sel);
                    return;
                }
                KeyCode::Char(c) => {
                    if let Some(i) = FINISH_ITEMS.iter().position(|(n, _)| n.starts_with(c)) {
                        self.choose_finish(i);
                        return;
                    }
                    true
                }
                _ => true,
            },
            Popup::Strategy { selected } => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => false,
                KeyCode::Up | KeyCode::Char('k') => {
                    *selected = (*selected + STRATEGIES.len() - 1) % STRATEGIES.len();
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1) % STRATEGIES.len();
                    true
                }
                KeyCode::Enter => {
                    let s = STRATEGIES[*selected];
                    self.finish(FinishAction::Merge { strategy: Some(s) }, false);
                    false
                }
                _ => true,
            },
            Popup::Confirm { .. } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Popup::Confirm { action, .. } = popup {
                        match action {
                            Confirm::Close { id } => self.close(id),
                            Confirm::Finish { id, action } => self.finish_id(id, action, true),
                            Confirm::TrustSetup(mut req) => {
                                req.trust_setup = true;
                                self.create(req);
                            }
                        }
                    }
                    return;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                    false
                }
                _ => true,
            },
            Popup::New(form) if form.browser.is_some() => {
                form.browser_key(&k);
                true
            }
            Popup::New(form) => match k.code {
                KeyCode::Esc => false,
                // Ctrl+O 或 F2：開啟目錄瀏覽器選擇 Repo
                KeyCode::Char('o') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    form.open_browser(&self.cwd);
                    true
                }
                KeyCode::F(2) => {
                    form.open_browser(&self.cwd);
                    true
                }
                KeyCode::Tab | KeyCode::Down => {
                    form.field = (form.field + 1) % 4;
                    true
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.field = (form.field + 3) % 4;
                    true
                }
                KeyCode::Left if form.field == 0 => {
                    form.agent = (form.agent + form.agents.len() - 1) % form.agents.len().max(1);
                    true
                }
                KeyCode::Right | KeyCode::Char(' ') if form.field == 0 => {
                    form.agent = (form.agent + 1) % form.agents.len().max(1);
                    true
                }
                KeyCode::Enter => {
                    let req = CreateSession {
                        agent: form
                            .agents
                            .get(form.agent)
                            .cloned()
                            .unwrap_or_else(|| "shell".into()),
                        repo: expand_home(form.repo.trim()),
                        task: form.task.trim().to_string(),
                        base_branch: Some(form.base.trim().to_string()).filter(|s| !s.is_empty()),
                        share: None,
                        trust_setup: false,
                        cols: Some(self.main_area.width.max(20)),
                        rows: Some(self.main_area.height.max(5)),
                    };
                    self.create(req);
                    false
                }
                _ => {
                    match form.field {
                        1 => edit_text(&mut form.repo, &k),
                        2 => edit_text(&mut form.task, &k),
                        3 => edit_text(&mut form.base, &k),
                        _ => {}
                    }
                    true
                }
            },
        };
        if keep {
            self.popup = Some(popup);
        }
    }

    fn choose_finish(&mut self, i: usize) {
        match FINISH_ITEMS[i].0 {
            "merge" => {
                if self.ui.merge_strategy == "ask" {
                    self.popup = Some(Popup::Strategy { selected: 0 });
                } else {
                    self.finish(FinishAction::Merge { strategy: None }, false);
                }
            }
            "pr" => self.finish(FinishAction::Pr, false),
            "keep" => self.finish(FinishAction::Keep, false),
            _ => self.finish(FinishAction::Discard, false),
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let in_rect = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && let Some(c) = self
                .click_hits
                .iter()
                .rev()
                .find(|h| in_rect(h.rect))
                .map(|h| h.click.clone())
        {
            self.dirty = true;
            self.on_click(c);
            return;
        }
        if let Some(Popup::New(NewForm {
            browser: Some(b), ..
        })) = self.popup.as_mut()
        {
            // 目錄瀏覽器：滾輪移動
            match m.kind {
                MouseEventKind::ScrollUp => b.move_by(-3),
                MouseEventKind::ScrollDown => b.move_by(3),
                _ => return,
            }
            self.dirty = true;
            return;
        }
        if self.popup.is_some() {
            return;
        }
        if in_rect(self.sidebar_area) {
            if let MouseEventKind::Down(_) = m.kind {
                let hit = self
                    .sidebar_hits
                    .iter()
                    .rfind(|h| h.y <= y)
                    .map(|h| h.id.clone());
                if let Some(id) = hit {
                    self.select(Some(id));
                }
            }
            return;
        }
        if !in_rect(self.main_area) {
            return;
        }
        let Some(screen) = self.screen.as_ref() else {
            return;
        };
        let (rx, ry) = (x - self.main_area.x, y - self.main_area.y);
        if screen.has_mode(modes::MOUSE) {
            // Agent 開啟滑鼠模式時原樣轉送
            if let Some(b) = encode_mouse(
                &m,
                rx,
                ry,
                screen.has_mode(modes::SGR_MOUSE),
                screen.has_mode(modes::MOUSE_MOTION),
                screen.has_mode(modes::MOUSE_DRAG),
            ) {
                self.input(&b);
            }
            return;
        }
        match m.kind {
            MouseEventKind::ScrollUp if !screen.has_mode(modes::ALT_SCREEN) => {
                self.scroll_by(3);
                if self.scroll.is_none() {
                    self.scroll = Some(Scroll {
                        searching: None,
                        last_query: String::new(),
                    });
                }
            }
            MouseEventKind::ScrollDown if !screen.has_mode(modes::ALT_SCREEN) => {
                let near_bottom = screen.display_offset <= 3;
                self.scroll_by(-3);
                if near_bottom {
                    self.scroll = None;
                }
            }
            MouseEventKind::ScrollUp => self.input(b"\x1b[A"),
            MouseEventKind::ScrollDown => self.input(b"\x1b[B"),
            _ => {}
        }
    }

    /// 滑鼠點擊按鈕：轉成和鍵盤操作相同的處理，行為保持一致。
    pub fn on_click(&mut self, c: Click) {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        match c {
            Click::Command(ch) => {
                self.prefix_armed = false;
                if self.popup.is_none() {
                    self.scroll = None;
                    self.on_command(key(KeyCode::Char(ch)));
                }
            }
            Click::Key(code) => {
                if self.popup.is_some() {
                    self.on_popup_key(key(code));
                }
            }
            Click::Field(i) => {
                if let Some(Popup::New(f)) = self.popup.as_mut() {
                    f.field = i;
                }
            }
            Click::Agent(d) => {
                if let Some(Popup::New(f)) = self.popup.as_mut() {
                    let n = f.agents.len().max(1);
                    f.field = 0;
                    f.agent = if d < 0 {
                        (f.agent + n - 1) % n
                    } else {
                        (f.agent + 1) % n
                    };
                }
            }
            Click::Browse => {
                let cwd = self.cwd.clone();
                if let Some(Popup::New(f)) = self.popup.as_mut() {
                    f.open_browser(&cwd);
                }
            }
            Click::Item(i) => {
                match self.popup.as_mut() {
                    Some(Popup::Finish { selected }) | Some(Popup::Strategy { selected }) => {
                        *selected = i
                    }
                    // 目錄瀏覽器：點一下等於選到該項後按 Enter（進入目錄 / 使用這個目錄）
                    Some(Popup::New(NewForm {
                        browser: Some(b), ..
                    })) => b.selected = i,
                    _ => return,
                }
                self.on_popup_key(key(KeyCode::Enter));
            }
            Click::Nothing => {}
        }
    }

    pub fn tick(&mut self) {
        if let Some((_, _, at)) = &self.message
            && at.elapsed() > Duration::from_secs(6)
        {
            self.message = None;
            self.dirty = true;
        }
        if self
            .sessions
            .iter()
            .any(|s| s.status == AgentStatus::WaitingInput)
        {
            self.blink_on = !self.blink_on;
            self.dirty = true;
        } else {
            self.blink_on = true;
        }
    }
}

fn text_field(p: &mut Popup) -> Option<&mut String> {
    match p {
        Popup::Rename { input } => Some(input),
        Popup::New(f) => match f.field {
            1 => Some(&mut f.repo),
            2 => Some(&mut f.task),
            3 => Some(&mut f.base),
            _ => None,
        },
        _ => None,
    }
}

fn edit_text(s: &mut String, k: &KeyEvent) {
    match k.code {
        KeyCode::Backspace => {
            s.pop();
        }
        KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => s.clear(),
        KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => s.push(c),
        _ => {}
    }
}

fn expand_home(p: &str) -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    if p == "~" {
        home
    } else if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        std::path::Path::new(&home)
            .join(rest)
            .to_string_lossy()
            .into_owned()
    } else {
        let path = std::path::Path::new(p);
        if path.is_relative() {
            std::env::current_dir()
                .map(|d| d.join(path).to_string_lossy().into_owned())
                .unwrap_or_else(|_| p.into())
        } else {
            p.into()
        }
    }
}
