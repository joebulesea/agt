//! 滑鼠、點擊範圍與定時工作。

use super::*;

impl App {
    pub(super) fn on_mouse(&mut self, m: MouseEvent) {
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
            let now = Instant::now();
            let double = crate::open::is_double_click(self.last_click, x, y, now);
            // 雙擊之後清除，第三下重新計算
            self.last_click = (!double).then_some((x, y, now));
            if matches!(c, Click::OpenDir) {
                if double {
                    self.open_active_dir();
                } else {
                    self.set_message("雙擊開啟工作目錄", false);
                }
                return;
            }
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
        if let Some(Popup::Text {
            lines,
            scroll,
            diff,
            ..
        }) = self.popup.as_mut()
        {
            // diff / 文字視窗：滾輪一次捲 3 行
            let max = crate::ui::text_rows(lines, *diff, self.screen_area.width)
                .len()
                .saturating_sub(1);
            match m.kind {
                MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => *scroll = (*scroll + 3).min(max),
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
                    Some(Popup::Finish { selected, .. })
                    | Some(Popup::Strategy { selected })
                    | Some(Popup::Close { selected, .. })
                    | Some(Popup::Switch { selected, .. }) => *selected = i,
                    // 目錄瀏覽器：點一下等於選到該項後按 Enter（進入目錄 / 使用這個目錄）
                    Some(Popup::New(NewForm {
                        browser: Some(b), ..
                    })) => b.selected = i,
                    _ => return,
                }
                self.on_popup_key(key(KeyCode::Enter));
            }
            Click::OpenDir => self.open_active_dir(),
            Click::Nothing => {}
        }
    }

    /// 目前分頁的工作目錄：有 worktree 時是 worktree（Agent 改檔案的地方），否則是執行目錄。
    pub fn active_dir(&self) -> Option<String> {
        let s = self.active_info()?;
        s.worktree
            .clone()
            .or_else(|| Some(s.cwd.clone()))
            .filter(|d| !d.is_empty())
    }

    /// 用系統的檔案管理員開啟目前分頁的工作目錄。
    pub(super) fn open_active_dir(&mut self) {
        let Some(dir) = self.active_dir() else {
            if self.active.is_some() {
                self.set_message("這個分頁沒有工作目錄", true);
            }
            return;
        };
        match crate::open::open_dir(&dir) {
            Ok(()) => self.set_message(format!("已開啟 {dir}"), false),
            Err(e) => self.set_message(e, true),
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
