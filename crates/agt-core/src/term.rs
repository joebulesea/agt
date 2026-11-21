//! `alacritty_terminal` 的封裝。其他模組只透過這裡的 [`Terminal`] 使用 VT 模擬器，
//! 升級 `alacritty_terminal` 時只需要改這個檔案（規格 14.1）。

use agt_proto::screen::{Cell, Color, Cursor, Row, Screen, flags, modes};
use agt_proto::sync::MutexExt;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AColor, NamedColor, Processor};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// 模擬器產生、需要由外部處理的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum TermEvent {
    /// 要回寫給 PTY 的資料（例如 DA / DSR 查詢的回應）。
    PtyWrite(String),
    Title(String),
    Bell,
    /// 程式透過 OSC 52 要求複製到剪貼簿的文字（例如 Claude Code 的「c to copy」）。
    Clipboard(String),
}

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<TermEvent>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let ev = match event {
            Event::PtyWrite(s) => TermEvent::PtyWrite(s),
            Event::Title(t) => TermEvent::Title(t),
            Event::Bell => TermEvent::Bell,
            Event::ClipboardStore(_, text) => TermEvent::Clipboard(text),
            _ => return,
        };
        self.0.lock_or_recover().push(ev);
    }
}

pub struct Terminal {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        let events = Listener::default();
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let size = Size {
            cols: cols.max(2) as usize,
            rows: rows.max(1) as usize,
        };
        Terminal {
            term: Term::new(config, &size, events.clone()),
            parser: Processor::new(),
            events,
        }
    }

    /// 餵入 PTY 輸出，回傳需要處理的事件。
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<TermEvent> {
        self.parser.advance(&mut self.term, bytes);
        std::mem::take(&mut *self.events.0.lock_or_recover())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(Size {
            cols: cols.max(2) as usize,
            rows: rows.max(1) as usize,
        });
    }

    pub fn cols(&self) -> u16 {
        self.term.columns() as u16
    }

    pub fn rows(&self) -> u16 {
        self.term.screen_lines() as u16
    }

    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// 捲動歷史；正數往上。`i32::MIN` 回到底部。
    pub fn scroll(&mut self, delta: i32) {
        if delta == i32::MIN {
            self.term.scroll_display(Scroll::Bottom);
        } else {
            self.term.scroll_display(Scroll::Delta(delta));
        }
    }

    fn line_text(&self, line: Line) -> String {
        let grid = self.term.grid();
        let row = &grid[line];
        let mut s = String::with_capacity(self.term.columns());
        for x in 0..self.term.columns() {
            let c = &row[Column(x)];
            if c.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            s.push(c.c);
        }
        s
    }

    /// 從目前畫面頂端往上（較舊）搜尋，找到時捲動到該行並回傳 true。
    pub fn search_up(&mut self, query: &str) -> bool {
        if query.is_empty() {
            return false;
        }
        let q = query.to_lowercase();
        let offset = self.display_offset() as i32;
        let history = self.history_size() as i32;
        // 目前畫面頂端的 Line 為 -offset；從它的上一行開始往上找
        let mut line = -offset - 1;
        while line >= -history {
            if self.line_text(Line(line)).to_lowercase().contains(&q) {
                // 讓找到的那一行出現在畫面頂端
                let target = -line;
                self.scroll(target - offset);
                return true;
            }
            line -= 1;
        }
        false
    }

    /// 畫面最後 n 行的文字：以「游標所在列」與「最後一個非空白列」中較下面的為結尾
    /// （例如 Claude Code 輸入框下方的狀態列也要算進去，但結尾的空白列不算），不受捲動影響。
    pub fn last_lines(&self, n: usize) -> Vec<String> {
        let rows = self.term.screen_lines() as i32;
        let cursor = self.term.grid().cursor.point.line.0.clamp(0, rows - 1);
        let last_text = (0..rows)
            .rev()
            .find(|y| !self.line_text(Line(*y)).trim().is_empty())
            .unwrap_or(0);
        let end = cursor.max(last_text) + 1;
        let start = end - n as i32;
        (start..end)
            .filter(|y| *y >= -(self.history_size() as i32))
            .map(|y| self.line_text(Line(y)).trim_end().to_string())
            .collect()
    }

    pub fn mode_flags(&self) -> u16 {
        let m = self.term.mode();
        let mut out = 0;
        let mut set = |cond: bool, f: u16| {
            if cond {
                out |= f;
            }
        };
        set(m.contains(TermMode::APP_CURSOR), modes::APP_CURSOR);
        set(m.contains(TermMode::APP_KEYPAD), modes::APP_KEYPAD);
        set(m.intersects(TermMode::MOUSE_MODE), modes::MOUSE);
        set(m.contains(TermMode::MOUSE_DRAG), modes::MOUSE_DRAG);
        set(m.contains(TermMode::MOUSE_MOTION), modes::MOUSE_MOTION);
        set(m.contains(TermMode::SGR_MOUSE), modes::SGR_MOUSE);
        set(
            m.contains(TermMode::BRACKETED_PASTE),
            modes::BRACKETED_PASTE,
        );
        set(m.contains(TermMode::ALT_SCREEN), modes::ALT_SCREEN);
        out
    }

    /// 產生目前畫面（含捲動位置）的快照。
    pub fn screen(&self) -> Screen {
        let grid = self.term.grid();
        let cols = self.term.columns();
        let rows = self.term.screen_lines();
        let offset = grid.display_offset() as i32;
        let mut lines = Vec::with_capacity(rows);
        for y in 0..rows as i32 {
            let row = &grid[Line(y - offset)];
            let mut out: Row = Vec::with_capacity(cols);
            for x in 0..cols {
                out.push(convert_cell(&row[Column(x)]));
            }
            lines.push(out);
        }
        let Point { line, column } = grid.cursor.point;
        let cy = line.0 + offset;
        let visible =
            self.term.mode().contains(TermMode::SHOW_CURSOR) && cy >= 0 && (cy as usize) < rows;
        Screen {
            cols: cols as u16,
            rows: rows as u16,
            lines,
            cursor: Cursor {
                x: column.0.min(cols.saturating_sub(1)) as u16,
                y: cy.clamp(0, rows as i32 - 1) as u16,
                visible,
            },
            modes: self.mode_flags(),
            display_offset: offset as u32,
            history: grid.history_size() as u32,
        }
    }
}

fn convert_color(c: AColor) -> Color {
    match c {
        AColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AColor::Indexed(i) => Color::Idx(i),
        AColor::Named(n) => {
            let v = n as usize;
            if v < 16 {
                Color::Idx(v as u8)
            } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&v) {
                Color::Idx((v - NamedColor::DimBlack as usize) as u8)
            } else {
                Color::Default
            }
        }
    }
}

fn convert_cell(c: &alacritty_terminal::term::cell::Cell) -> Cell {
    let f = c.flags;
    let mut out = 0u16;
    let mut set = |cond: bool, b: u16| {
        if cond {
            out |= b;
        }
    };
    set(f.contains(Flags::BOLD), flags::BOLD);
    set(f.contains(Flags::ITALIC), flags::ITALIC);
    set(f.intersects(Flags::ALL_UNDERLINES), flags::UNDERLINE);
    set(f.contains(Flags::INVERSE), flags::INVERSE);
    set(f.contains(Flags::DIM), flags::DIM);
    set(f.contains(Flags::STRIKEOUT), flags::STRIKE);
    set(f.contains(Flags::HIDDEN), flags::HIDDEN);
    set(f.contains(Flags::WIDE_CHAR), flags::WIDE);
    set(
        f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
        flags::SPACER,
    );
    let mut fg = convert_color(c.fg);
    if let AColor::Named(n) = c.fg {
        let v = n as usize;
        if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&v) {
            out |= flags::DIM;
        }
        if v < 8 && f.contains(Flags::BOLD) {
            // 粗體 + 基本色 → 亮色，與大多數終端機一致
            fg = Color::Idx(v as u8 + 8);
        }
    }
    let ch = if f.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
        ' '
    } else {
        c.c
    };
    Cell {
        ch,
        zw: c.zerowidth().map(|z| z.iter().collect()),
        fg,
        bg: convert_color(c.bg),
        flags: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_width_and_color() {
        let mut t = Terminal::new(10, 3, 100);
        t.advance("\x1b[31m中文\x1b[0mab".as_bytes());
        let s = t.screen();
        let row = &s.lines[0];
        assert_eq!(row[0].ch, '中');
        assert!(row[0].flags & flags::WIDE != 0);
        assert!(row[1].flags & flags::SPACER != 0);
        assert_eq!(row[2].ch, '文');
        assert_eq!(row[4].ch, 'a');
        assert_eq!(row[0].fg, Color::Idx(1));
        assert_eq!(s.row_text(0), "中文ab");
        assert_eq!(s.cursor.x, 6);
    }

    #[test]
    fn scrollback_and_search() {
        let mut t = Terminal::new(20, 3, 100);
        for i in 0..20 {
            t.advance(format!("line {i}\r\n").as_bytes());
        }
        assert!(t.history_size() > 0);
        assert!(t.search_up("line 3"));
        assert_eq!(t.screen().row_text(0), "line 3");
        t.scroll(i32::MIN);
        assert_eq!(t.display_offset(), 0);
    }

    #[test]
    fn answers_device_attributes() {
        let mut t = Terminal::new(20, 3, 10);
        let ev = t.advance(b"\x1b[c");
        assert!(ev.iter().any(|e| matches!(e, TermEvent::PtyWrite(_))));
    }

    #[test]
    fn osc52_copy_becomes_clipboard_event() {
        let mut t = Terminal::new(20, 3, 10);
        // "https://x" 的 base64
        let ev = t.advance(b"\x1b]52;c;aHR0cHM6Ly94\x07");
        assert_eq!(ev, vec![TermEvent::Clipboard("https://x".into())]);
        // 讀取剪貼簿（?）不允許
        assert!(t.advance(b"\x1b]52;c;?\x07").is_empty());
    }

    #[test]
    fn last_lines_end_at_cursor() {
        let mut t = Terminal::new(20, 10, 100);
        t.advance(b"one\r\ntwo (y/n) ");
        assert_eq!(t.last_lines(2), vec!["one", "two (y/n)"]);
        // 游標下方還有文字（例如輸入框下方的狀態列）時也要包含
        let mut t = Terminal::new(20, 10, 100);
        t.advance(b"top\r\n> \r\nNot logged in\x1b[2;3H");
        assert_eq!(t.last_lines(3), vec!["top", ">", "Not logged in"]);
    }

    #[test]
    fn resize_keeps_content() {
        let mut t = Terminal::new(20, 5, 100);
        t.advance(b"hello");
        t.resize(30, 8);
        let s = t.screen();
        assert_eq!((s.cols, s.rows), (30, 8));
        assert_eq!(s.row_text(0), "hello");
    }
}
