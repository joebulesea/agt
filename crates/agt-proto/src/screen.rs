//! 畫面快照與差異。Client 不需要 VT 模擬器，直接畫 cell。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Color {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

pub mod flags {
    pub const BOLD: u16 = 1 << 0;
    pub const ITALIC: u16 = 1 << 1;
    pub const UNDERLINE: u16 = 1 << 2;
    pub const INVERSE: u16 = 1 << 3;
    pub const DIM: u16 = 1 << 4;
    pub const STRIKE: u16 = 1 << 5;
    pub const HIDDEN: u16 = 1 << 6;
    /// 全形字元（佔 2 欄）的第一格。
    pub const WIDE: u16 = 1 << 7;
    /// 全形字元的第二格（不需要畫）。
    pub const SPACER: u16 = 1 << 8;
}

/// 終端機模式旗標。
pub mod modes {
    pub const APP_CURSOR: u16 = 1 << 0;
    pub const MOUSE: u16 = 1 << 1;
    pub const MOUSE_DRAG: u16 = 1 << 2;
    pub const MOUSE_MOTION: u16 = 1 << 3;
    pub const SGR_MOUSE: u16 = 1 << 4;
    pub const BRACKETED_PASTE: u16 = 1 << 5;
    pub const ALT_SCREEN: u16 = 1 << 6;
    pub const APP_KEYPAD: u16 = 1 << 7;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cell {
    pub ch: char,
    /// 附加在這個 cell 上的零寬字元（組合字元、emoji 變體選擇符）。
    pub zw: Option<String>,
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            zw: None,
            fg: Color::Default,
            bg: Color::Default,
            flags: 0,
        }
    }
}

pub type Row = Vec<Cell>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<Row>,
    pub cursor: Cursor,
    pub modes: u16,
    /// 往上捲動的行數；0 = 在底部。
    pub display_offset: u32,
    /// 滾動歷史總行數。
    pub history: u32,
}

impl Screen {
    pub fn has_mode(&self, m: u16) -> bool {
        self.modes & m != 0
    }

    /// 以文字形式取出某一列（測試與搜尋用）。
    pub fn row_text(&self, y: usize) -> String {
        row_text(&self.lines[y])
    }

    /// 計算與舊畫面的差異；尺寸不同時回傳 `None`（應改送快照）。
    pub fn diff_from(&self, old: &Screen) -> Option<Vec<(u16, Row)>> {
        if old.cols != self.cols || old.rows != self.rows || old.lines.len() != self.lines.len() {
            return None;
        }
        Some(
            self.lines
                .iter()
                .zip(old.lines.iter())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, (a, _))| (i as u16, a.clone()))
                .collect(),
        )
    }

    /// 套用差異。
    pub fn apply(&mut self, diff: &ScreenDiff) {
        for (y, row) in &diff.rows {
            if let Some(slot) = self.lines.get_mut(*y as usize) {
                *slot = row.clone();
            }
        }
        self.cursor = diff.cursor;
        self.modes = diff.modes;
        self.display_offset = diff.display_offset;
        self.history = diff.history;
    }
}

fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c)
}

impl Screen {
    /// 畫面上所有的 http(s) 網址（由上到下）。被折成多列的長網址會接回一整串：
    /// 網址延伸到該列文字結尾、且該列接近填滿時，下一列開頭（略過縮排）的連續網址字元視為接續。
    pub fn urls(&self) -> Vec<String> {
        let cols = self.cols as usize;
        let rows: Vec<Vec<char>> = self
            .lines
            .iter()
            .map(|r| {
                r.iter()
                    .filter(|c| c.flags & flags::SPACER == 0)
                    .map(|c| c.ch)
                    .collect()
            })
            .collect();
        let mut out = vec![];
        let mut y = 0;
        while y < rows.len() {
            let row = &rows[y];
            let line: String = row.iter().collect();
            let mut search_from = 0;
            let mut next_y = y + 1;
            while let Some(pos) = find_scheme(&line[search_from..]).map(|p| p + search_from) {
                let start = line[..pos].chars().count();
                let mut url: String = row[start..]
                    .iter()
                    .take_while(|c| is_url_char(**c))
                    .collect();
                let end = start + url.chars().count();
                search_from = pos + url.len().max(1);
                // 網址是否延伸到這一列文字的結尾，且這列接近填滿 → 可能被折行
                let mut cy = y;
                let mut cur_end = end;
                loop {
                    let r = &rows[cy];
                    let text_len = r.iter().rposition(|c| *c != ' ').map_or(0, |i| i + 1);
                    if cur_end < text_len || text_len + 4 < cols || cy + 1 >= rows.len() {
                        break;
                    }
                    let next = &rows[cy + 1];
                    let skip = next.iter().take_while(|c| **c == ' ').count();
                    let piece: String = next[skip..]
                        .iter()
                        .take_while(|c| is_url_char(**c))
                        .collect();
                    if piece.is_empty() || find_scheme(&piece) == Some(0) {
                        break;
                    }
                    cur_end = skip + piece.chars().count();
                    url.push_str(&piece);
                    cy += 1;
                    next_y = next_y.max(cy + 1);
                    let rest = &rows[cy];
                    if rest[cur_end..].iter().any(|c| *c != ' ') {
                        break;
                    }
                }
                let url = url
                    .trim_end_matches(['.', ',', ')', ']', '\'', ';', ':'])
                    .to_string();
                if url.len() > "https://".len() {
                    out.push(url);
                }
                if cy > y {
                    break;
                }
            }
            y = next_y;
        }
        out
    }
}

fn find_scheme(s: &str) -> Option<usize> {
    match (s.find("https://"), s.find("http://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

pub fn row_text(row: &Row) -> String {
    let mut s = String::new();
    for c in row {
        if c.flags & flags::SPACER != 0 {
            continue;
        }
        s.push(c.ch);
        if let Some(zw) = &c.zw {
            s.push_str(zw);
        }
    }
    s.trim_end().to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenDiff {
    pub rows: Vec<(u16, Row)>,
    pub cursor: Cursor,
    pub modes: u16,
    pub display_offset: u32,
    pub history: u32,
}

/// kind 0x02 封包的內容（postcard）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScreenUpdate {
    Snapshot { id: String, screen: Screen },
    Diff { id: String, diff: ScreenDiff },
}

impl ScreenUpdate {
    pub fn id(&self) -> &str {
        match self {
            ScreenUpdate::Snapshot { id, .. } | ScreenUpdate::Diff { id, .. } => id,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("ScreenUpdate 序列化不會失敗")
    }

    pub fn decode(b: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &[&str]) -> Screen {
        Screen {
            cols: 4,
            rows: text.len() as u16,
            lines: text
                .iter()
                .map(|t| {
                    t.chars()
                        .map(|ch| Cell {
                            ch,
                            ..Default::default()
                        })
                        .collect()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn diff_and_apply() {
        let a = screen(&["abcd", "efgh"]);
        let b = screen(&["abcd", "efgX"]);
        let rows = b.diff_from(&a).unwrap();
        assert_eq!(rows.len(), 1);
        let mut c = a.clone();
        c.apply(&ScreenDiff {
            rows,
            cursor: b.cursor,
            modes: 0,
            display_offset: 0,
            history: 0,
        });
        assert_eq!(c, b);
    }

    fn screen_w(cols: u16, text: &[&str]) -> Screen {
        Screen {
            cols,
            rows: text.len() as u16,
            lines: text
                .iter()
                .map(|t| {
                    let mut r: Row = t
                        .chars()
                        .map(|ch| Cell {
                            ch,
                            ..Default::default()
                        })
                        .collect();
                    r.resize(cols as usize, Cell::default());
                    r
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn urls_join_wrapped_rows() {
        // 終端機自動折行（從第 0 欄接續）
        let s = screen_w(
            20,
            &[
                "Open: https://claude",
                ".ai/oauth?code=1&x=",
                "2",
                "Paste code here",
            ],
        );
        assert_eq!(s.urls(), vec!["https://claude.ai/oauth?code=1&x=2"]);
        // 程式自己折行並縮排
        let s = screen_w(20, &["  https://example.co", "  m/a/b/c.", "next"]);
        assert_eq!(s.urls(), vec!["https://example.com/a/b/c"]);
        // 短網址不會吃掉下一列；同一列兩個網址
        let s = screen_w(40, &["see https://a.com, http://b.org/x", "hello"]);
        assert_eq!(s.urls(), vec!["https://a.com", "http://b.org/x"]);
    }

    #[test]
    fn postcard_roundtrip() {
        let u = ScreenUpdate::Snapshot {
            id: "x".into(),
            screen: screen(&["中文", "ab"]),
        };
        assert_eq!(ScreenUpdate::decode(&u.encode()).unwrap(), u);
    }
}
