//! 畫面繪製：左側欄、主區域、底部狀態列（規格 8.1）。

use crate::app::{App, CLOSE_ITEMS, Click, ClickHit, Confirm, FINISH_ITEMS, Popup, STRATEGIES};
use crate::theme::theme;
use agt_proto::screen::{Color as PColor, Screen, flags};
use agt_proto::{AgentStatus, CheckState, LandingLevel, SessionInfo};
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod panes;
#[cfg(test)]
pub(crate) use panes::usage_spans;
mod popup;

use panes::*;
pub use popup::*;

/// 外層終端機不支援真彩時，把 RGB 降級成 256 色。
pub fn truecolor() -> bool {
    let ct = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_lowercase();
    ct.contains("truecolor")
        || ct.contains("24bit")
        || std::env::var_os("WT_SESSION").is_some()
        || matches!(
            std::env::var("TERM_PROGRAM").unwrap_or_default().as_str(),
            "iTerm.app" | "WezTerm" | "vscode"
        )
}

fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + ((r as u16 - 8) * 24 / 247) as u8;
    }
    let q = |v: u8| ((v as u16 * 5 + 127) / 255) as u8;
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

pub(crate) fn color(c: PColor, tc: bool) -> Color {
    match c {
        PColor::Default => Color::Reset,
        PColor::Idx(i) => Color::Indexed(i),
        PColor::Rgb(r, g, b) if tc => Color::Rgb(r, g, b),
        PColor::Rgb(r, g, b) => Color::Indexed(rgb_to_256(r, g, b)),
    }
}

pub fn status_style(s: AgentStatus) -> Style {
    let t = theme();
    match s {
        AgentStatus::Running => Style::new().fg(t.status_running.unwrap_or(t.green)),
        AgentStatus::Thinking => Style::new().fg(t.status_thinking.unwrap_or(t.cyan)),
        AgentStatus::WaitingInput => Style::new()
            .fg(t.status_waiting.unwrap_or(t.yellow))
            .add_modifier(Modifier::BOLD),
        AgentStatus::Done => Style::new().fg(t.status_done.unwrap_or(t.gray)),
        AgentStatus::Error => Style::new().fg(t.status_error.unwrap_or(t.red)),
        AgentStatus::Starting => Style::new().fg(t.status_starting.unwrap_or(t.blue)),
        AgentStatus::Idle | AgentStatus::Exited => {
            Style::new().fg(t.status_idle.unwrap_or(t.dark_gray))
        }
    }
}

/// 依顯示寬度截斷（全形字元佔 2 欄）。
/// 依顯示寬度截斷、保留結尾（路徑的最後幾層比開頭重要），例如 `…\proj\agt`。
pub fn tail(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out: Vec<char> = vec![];
    let mut w = 1;
    for c in s.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out.into_iter().rev().collect()
}

pub fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// 等待輸入的時間，例如 `3m`、`2h05m`、`1d`；不到 1 分鐘（或時間未知）不顯示。
pub fn format_wait(secs: i64) -> Option<String> {
    if secs < 60 {
        return None;
    }
    Some(if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{:02}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{}d", secs / 86_400)
    })
}

fn pad(s: &str, width: usize) -> String {
    let t = truncate(s, width);
    let w = t.width();
    format!("{t}{}", " ".repeat(width.saturating_sub(w)))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.click_hits.clear();
    if area.width < 10 || area.height < 4 {
        return;
    }
    let collapsed = (area.width < 80) != app.sidebar_toggled;
    let side_w = if collapsed {
        4
    } else {
        app.ui.sidebar_width.min(area.width / 2)
    };
    let body_h = area.height - 1;
    let sidebar = Rect::new(0, 0, side_w, body_h);
    let sep_x = side_w;
    let main_outer = Rect::new(side_w + 1, 0, area.width.saturating_sub(side_w + 1), body_h);
    let main = Rect::new(main_outer.x, 1, main_outer.width, body_h.saturating_sub(1));
    let status = Rect::new(0, body_h, area.width, 1);
    app.sidebar_area = sidebar;
    app.main_area = main;
    app.screen_area = area;
    app.send_resize();

    let border = Style::new().fg(theme().dark_gray);
    let buf = f.buffer_mut();
    for y in 0..body_h {
        buf[(sep_x, y)].set_symbol("│").set_style(border);
    }
    draw_sidebar(buf, app, sidebar, collapsed);
    draw_title(buf, app, Rect::new(main_outer.x, 0, main_outer.width, 1));
    if let Some(screen) = &app.screen {
        draw_screen(buf, screen, main);
        draw_exit_hint(buf, app, main);
    } else if app.sessions.is_empty() {
        let p = app.prefix.label();
        let lines = vec![
            Line::raw(""),
            Line::styled(
                "  還沒有任何分頁",
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::raw(format!(
                "  點左上角或底部的「+ 新增」按鈕，或按 {p} n 新增分頁"
            )),
            Line::raw(format!(
                "  按 {p} ? 查看所有快捷鍵，{p} q 離開（Agent 會繼續在背景執行）"
            )),
        ];
        Paragraph::new(lines).render_ref(main, buf);
    }
    draw_status(buf, app, status);

    let show_cursor = app.popup.is_none() && app.scroll.is_none() && !app.prefix_armed;
    if let (true, Some(s)) = (show_cursor, &app.screen)
        && s.cursor.visible
        && s.cursor.x < main.width
        && s.cursor.y < main.height
    {
        f.set_cursor_position(Position::new(main.x + s.cursor.x, main.y + s.cursor.y));
    }
    if app.popup.is_some() {
        let hits = draw_popup(f, app);
        app.click_hits.extend(hits);
    }
}

/// 畫一個按鈕並登記點擊範圍，回傳寬度。
fn button(
    buf: &mut Buffer,
    hits: &mut Vec<ClickHit>,
    x: u16,
    y: u16,
    label: &str,
    style: Style,
    click: Click,
) -> u16 {
    let w = label.width() as u16;
    buf.set_string(x, y, label, style);
    hits.push(ClickHit {
        rect: Rect::new(x, y, w, 1),
        click,
    });
    w
}

fn button_style() -> Style {
    Style::new()
        .fg(theme().on_accent)
        .bg(theme().cyan)
        .add_modifier(Modifier::BOLD)
}

/// 選單項目從框內第二列開始（第一列空白），每項一列。
fn menu_hits(hits: &mut Vec<ClickHit>, r: Rect, n: usize) {
    hits.push(ClickHit {
        rect: r,
        click: Click::Nothing,
    });
    for i in 0..n as u16 {
        let y = r.y + 2 + i;
        if y + 1 < r.y + r.height {
            hits.push(ClickHit {
                rect: Rect::new(r.x + 1, y, r.width.saturating_sub(2), 1),
                click: Click::Item(i as usize),
            });
        }
    }
}

trait RenderRef {
    fn render_ref(self, area: Rect, buf: &mut Buffer);
}
impl RenderRef for Paragraph<'_> {
    fn render_ref(self, area: Rect, buf: &mut Buffer) {
        ratatui::widgets::Widget::render(self, area, buf);
    }
}

#[cfg(test)]
mod tests;
