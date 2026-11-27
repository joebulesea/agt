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

mod events;
mod input;
mod mouse;
mod popup;
mod requests;

/// 非同步請求的結果，送回主迴圈處理。
pub enum AppEvent {
    Created(Box<SessionInfo>),
    CreateFailed(Box<(ClientError, CreateSession)>),
    Finished(FinishAction, Result<String, ClientError>),
    Diff(Result<String, ClientError>),
    /// 同步 base 的結果（成功的說明，或失敗原因）。
    Synced(Result<String, ClientError>),
    /// 收尾視窗的預覽（分頁 id, 內容）。
    FinishPreview(String, &'static str, Result<String, ClientError>),
    Searched(bool),
    Closed(Result<(), ClientError>),
    /// 開啟 TUI 時找到的、留下來的 worktree 副本（`agt gc`）。
    Leftovers(Vec<GcItem>),
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
    /// 分頁以 OSC 52 要求寫入剪貼簿（設定 osc52 = "ask"）。
    Clipboard(String),
    /// 清理已關閉分頁留下的 worktree 副本（`agt gc --prune`）。
    CleanLeftovers,
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
    /// 設定檔中的範本（依名稱排序）與目前選的那一個（`None` = 不套用）。
    pub templates: Vec<agt_proto::TemplateInfo>,
    pub template: Option<usize>,
}

impl NewForm {
    /// 切到下一個範本（最後一個之後回到「不套用」），並把範本的 Agent、任務、base 填進表單。
    /// 回到「不套用」時不清除已填的欄位。
    pub fn next_template(&mut self) {
        if self.templates.is_empty() {
            return;
        }
        self.template = match self.template {
            None => Some(0),
            Some(i) if i + 1 < self.templates.len() => Some(i + 1),
            Some(_) => None,
        };
        let Some(t) = self.template.and_then(|i| self.templates.get(i)) else {
            return;
        };
        if let Some(p) = self.agents.iter().position(|a| *a == t.agent) {
            self.agent = p;
        }
        self.task = t.task.clone();
        if let Some(b) = &t.base {
            self.base = b.clone();
        }
    }

    /// 目前選的範本名稱。
    pub fn template_name(&self) -> Option<&str> {
        self.template
            .and_then(|i| self.templates.get(i))
            .map(|t| t.name.as_str())
    }

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
        /// 預計產生的 commit / PR 摘要，[merge, pr]；載入中或失敗時是一行說明。
        preview: [Vec<String>; 2],
    },
    /// 關閉有 worktree 的分頁：選擇刪除或保留副本。
    Close {
        id: String,
        /// Agent 還在執行時是 Agent 的名稱，選單上會提醒。
        busy: Option<String>,
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
        /// diff 依 `### ` 標題切成的分頁（標題, 內容）；空表示不分頁。
        tabs: Vec<(String, Vec<String>)>,
        tab: usize,
    },
    Rename {
        input: String,
    },
    /// 編輯標籤：以空白或逗號分隔。
    Tags {
        input: String,
    },
    /// 快速切換器：輸入關鍵字篩選分頁，Enter 跳過去。
    Switch {
        input: String,
        selected: usize,
    },
}

pub const FINISH_ITEMS: [(&str, &str); 4] = [
    ("merge", "合併進 base branch"),
    ("pr", "push 並建立 PR"),
    ("keep", "結束 Agent，保留 worktree 與 branch"),
    ("discard", "移除 worktree 並刪除 branch"),
];
pub const CLOSE_ITEMS: [(&str, &str); 2] = [
    ("delete", "關閉並刪除 worktree 副本與 branch"),
    ("keep", "關閉，保留副本（之後會提示清理）"),
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
    /// 標題列的工作目錄：雙擊用檔案管理員開啟。
    OpenDir,
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
    /// 整個畫面（文字視窗依它的寬度折行）。
    pub screen_area: Rect,
    pub sidebar_area: Rect,
    pub sidebar_hits: Vec<SidebarHit>,
    pub click_hits: Vec<ClickHit>,
    /// 上一次左鍵點擊可點擊範圍的位置與時間（判斷雙擊用）。
    pub last_click: Option<(u16, u16, Instant)>,
    /// 等待寫給外層終端機的剪貼簿內容（OSC 52），由主迴圈送出。
    pub clipboard_out: Option<String>,
    /// 使用者在確認框按過 a（本次一律允許）的分頁；只在這次 TUI 執行期間有效。
    pub clipboard_trusted: std::collections::HashSet<String>,
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
        crate::theme::set(&ui.theme, &ui.colors);
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
            screen_area: Rect::default(),
            sidebar_area: Rect::default(),
            sidebar_hits: vec![],
            click_hits: vec![],
            last_click: None,
            clipboard_out: None,
            clipboard_trusted: Default::default(),
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

    /// 快速切換器的候選：每個以空白分隔的關鍵字都要出現在
    /// Agent、任務、repo、branch、短 id、標籤或狀態其中之一（不分大小寫），順序同側欄。
    pub fn switch_matches(&self, query: &str) -> Vec<&SessionInfo> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.ordered()
            .into_iter()
            .filter(|s| {
                let hay = format!(
                    "{} {} {} {} {} {} {}",
                    s.agent,
                    s.task,
                    s.repo_name.as_deref().unwrap_or(""),
                    s.branch.as_deref().unwrap_or(""),
                    s.short_id,
                    s.tags.join(" "),
                    s.status.as_str()
                )
                .to_lowercase();
                terms.iter().all(|t| hay.contains(t.as_str()))
            })
            .collect()
    }

    pub fn active_info(&self) -> Option<&SessionInfo> {
        let id = self.active.as_ref()?;
        self.sessions.iter().find(|s| &s.id == id)
    }

    pub fn set_message(&mut self, m: impl Into<String>, error: bool) {
        // 訊息可能帶有 daemon 轉來的文字（git 錯誤、Agent 的內容）
        let m = agt_proto::text::printable_line(&m.into());
        self.message = Some((m, error, Instant::now()));
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
}

fn text_field(p: &mut Popup) -> Option<&mut String> {
    match p {
        Popup::Rename { input } | Popup::Tags { input } | Popup::Switch { input, .. } => {
            Some(input)
        }
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

pub(crate) fn expand_home(p: &str) -> String {
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

/// 留下來的 worktree 副本的確認訊息。
fn leftovers_text(items: &[GcItem]) -> String {
    let mut t = format!("找到 {} 個留下來的 worktree 副本：\n", items.len());
    for it in items.iter().take(8) {
        let what = match it.kind.as_str() {
            "closed_worktree" => "已關閉的分頁",
            "orphan_worktree" => "不在資料庫中",
            _ => "資料夾已不存在",
        };
        t.push_str(&format!("\n  {}（{what}）", it.path));
    }
    if items.len() > 8 {
        t.push_str(&format!("\n  …還有 {} 個", items.len() - 8));
    }
    t.push_str(
        "\n\n要刪除這些副本嗎？有未 commit 變更的副本會保留，已合併的 branch 一併刪除。(y/n)",
    );
    t
}

/// 把貼上的文字包成 bracketed paste 序列。內容中若夾帶結束標記會先移除，
/// 避免貼上的文字提早結束貼上、讓後面的換行被當成按鍵執行。
fn paste_bytes(s: &str) -> Vec<u8> {
    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    let clean = s.replace("\x1b[201~", "");
    let mut b = START.to_vec();
    b.extend_from_slice(clean.as_bytes());
    b.extend_from_slice(END);
    b
}

#[cfg(test)]
mod paste_tests {
    use super::paste_bytes;

    #[test]
    fn multiline_paste_is_wrapped_not_executed() {
        let b = paste_bytes("a\nb\r\nc");
        assert_eq!(b, b"\x1b[200~a\nb\r\nc\x1b[201~");
    }

    #[test]
    fn embedded_end_marker_is_stripped() {
        let b = paste_bytes("x\x1b[201~\nrm -rf /");
        assert_eq!(b, b"\x1b[200~x\nrm -rf /\x1b[201~");
        assert_eq!(b.windows(6).filter(|w| *w == b"\x1b[201~").count(), 1);
    }

    #[test]
    fn empty_paste_still_wrapped() {
        assert_eq!(paste_bytes(""), b"\x1b[200~\x1b[201~");
    }
}

#[cfg(test)]
mod tests;
