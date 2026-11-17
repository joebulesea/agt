//! 畫面繪製：左側欄、主區域、底部狀態列（規格 8.1）。

use crate::app::{App, Click, ClickHit, Confirm, FINISH_ITEMS, Popup, STRATEGIES};
use agt_proto::screen::{Color as PColor, Screen, flags};
use agt_proto::{AgentStatus, SessionInfo};
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

fn color(c: PColor, tc: bool) -> Color {
    match c {
        PColor::Default => Color::Reset,
        PColor::Idx(i) => Color::Indexed(i),
        PColor::Rgb(r, g, b) if tc => Color::Rgb(r, g, b),
        PColor::Rgb(r, g, b) => Color::Indexed(rgb_to_256(r, g, b)),
    }
}

pub fn status_style(s: AgentStatus) -> Style {
    match s {
        AgentStatus::Running => Style::new().fg(Color::Green),
        AgentStatus::Thinking => Style::new().fg(Color::Cyan),
        AgentStatus::WaitingInput => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        AgentStatus::Done => Style::new().fg(Color::Gray),
        AgentStatus::Error => Style::new().fg(Color::Red),
        AgentStatus::Starting => Style::new().fg(Color::Blue),
        AgentStatus::Idle | AgentStatus::Exited => Style::new().fg(Color::DarkGray),
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
    app.send_resize();

    let border = Style::new().fg(Color::DarkGray);
    let buf = f.buffer_mut();
    for y in 0..body_h {
        buf[(sep_x, y)].set_symbol("│").set_style(border);
    }
    draw_sidebar(buf, app, sidebar, collapsed);
    draw_title(buf, app, Rect::new(main_outer.x, 0, main_outer.width, 1));
    if let Some(screen) = &app.screen {
        draw_screen(buf, screen, main);
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
        .fg(Color::Black)
        .bg(Color::Cyan)
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

fn draw_sidebar(buf: &mut Buffer, app: &mut App, area: Rect, collapsed: bool) {
    app.sidebar_hits.clear();
    let mut y = area.y;
    let w = area.width as usize;
    let bottom = area.y + area.height;
    if collapsed {
        // 收合時第一列只放一個「+」按鈕
        buf.set_string(area.x, y, pad("", w), Style::new());
        button(
            buf,
            &mut app.click_hits,
            area.x + 1,
            y,
            "+",
            button_style(),
            Click::Command('n'),
        );
        y += 1;
    } else {
        buf.set_string(
            area.x,
            y,
            pad(" agt", w),
            Style::new().add_modifier(Modifier::BOLD),
        );
        let label = " + 新增 ";
        let bw = label.width() as u16;
        if area.width >= bw + 6 {
            button(
                buf,
                &mut app.click_hits,
                area.x + area.width - bw - 1,
                y,
                label,
                button_style(),
                Click::Command('n'),
            );
        }
        y += 1;
    }
    let mut last_repo: Option<Option<String>> = None;
    let ordered: Vec<SessionInfo> = app.ordered().into_iter().cloned().collect();
    for (n, s) in ordered.iter().enumerate() {
        if y >= bottom {
            break;
        }
        let active = app.active.as_deref() == Some(&s.id);
        let sym_style = if s.status == AgentStatus::WaitingInput && !app.blink_on {
            Style::new().fg(Color::DarkGray)
        } else {
            status_style(s.status)
        };
        let row_style = if active {
            Style::new()
                .bg(Color::Indexed(236))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        if collapsed {
            buf.set_string(area.x, y, pad("", w), row_style);
            buf.set_string(area.x + 1, y, s.status.symbol(), sym_style.patch(row_style));
            app.sidebar_hits.push(crate::app::SidebarHit {
                y,
                id: s.id.clone(),
            });
            y += 1;
            continue;
        }
        if last_repo.as_ref() != Some(&s.repo_path) {
            last_repo = Some(s.repo_path.clone());
            let name = s.repo_name.clone().unwrap_or_else(|| "（無 repo）".into());
            let base = s
                .base_branch
                .clone()
                .map(|b| format!("  ({b})"))
                .unwrap_or_default();
            buf.set_string(
                area.x,
                y,
                pad(&format!("▼ {name}{base}"), w),
                Style::new().fg(Color::Blue),
            );
            y += 1;
            if y >= bottom {
                break;
            }
        }
        app.sidebar_hits.push(crate::app::SidebarHit {
            y,
            id: s.id.clone(),
        });
        // 第一行：燈號 Agent 任務 ✎
        buf.set_string(area.x, y, pad("", w), row_style);
        let num = if n < 9 {
            format!("{}", n + 1)
        } else {
            " ".into()
        };
        buf.set_string(
            area.x,
            y,
            &num,
            Style::new().fg(Color::DarkGray).patch(row_style),
        );
        buf.set_string(area.x + 2, y, s.status.symbol(), sym_style.patch(row_style));
        let agent = truncate(&s.agent, 7);
        let mark = if s.dirty { " ✎" } else { "" };
        let avail = w.saturating_sub(4 + 8 + mark.width());
        let text = format!("{} {}", pad(&agent, 7), truncate(&s.task, avail));
        buf.set_string(area.x + 4, y, &text, row_style);
        if !mark.is_empty() {
            buf.set_string(
                area.x + w as u16 - 2,
                y,
                "✎",
                Style::new().fg(Color::Yellow).patch(row_style),
            );
        }
        y += 1;
        // 第二行：branch +N 或「無 worktree」
        if y < bottom {
            let detail = match &s.branch {
                Some(b) => {
                    let ahead = if s.ahead > 0 {
                        format!(" +{}", s.ahead)
                    } else {
                        String::new()
                    };
                    let room = w.saturating_sub(6 + ahead.width());
                    format!("{}{ahead}", truncate(b, room))
                }
                None if s.agent == "shell" => String::new(),
                None => "無 worktree".into(),
            };
            if !detail.is_empty() {
                buf.set_string(area.x, y, pad("", w), row_style);
                buf.set_string(
                    area.x + 6,
                    y,
                    truncate(&detail, w.saturating_sub(6)),
                    Style::new().fg(Color::DarkGray).patch(row_style),
                );
                y += 1;
            }
        }
    }
}

fn draw_title(buf: &mut Buffer, app: &mut App, area: Rect) {
    let border = Style::new().fg(Color::DarkGray);
    buf.set_string(area.x, area.y, "─".repeat(area.width as usize), border);
    let Some(s) = app.active_info() else { return };
    let mut parts = vec![s.agent.clone()];
    if let Some(r) = &s.repo_name {
        parts.push(r.clone());
    }
    if let Some(b) = &s.branch {
        parts.push(b.clone());
    }
    let mut title = format!(" {} ", parts.join(" · "));
    if s.shared {
        title.push_str("[共用] ");
    }
    if let Some(sc) = &app.screen
        && (sc.display_offset > 0 || app.scroll.is_some())
    {
        title.push_str(&format!("[滾動 {}/{}] ", sc.display_offset, sc.history));
    }
    let title = truncate(&title, area.width.saturating_sub(2) as usize);
    buf.set_string(
        area.x + 1,
        area.y,
        &title,
        Style::new().add_modifier(Modifier::BOLD),
    );
    let st = format!(" {} {} ", s.status.symbol(), s.status.as_str());
    let st_x = area.x + area.width.saturating_sub(st.width() as u16 + 1);
    let title_end = area.x + 1 + title.width() as u16;
    if st_x <= title_end {
        return;
    }
    let status = s.status;
    buf.set_string(st_x, area.y, &st, status_style(status));
    // 標題與狀態之間：工作目錄（太長時保留後段），雙擊用檔案管理員開啟
    let Some(dir) = app.active_dir() else { return };
    let dir = agt_proto::text::printable_line(&dir);
    let room = st_x.saturating_sub(title_end + 1) as usize;
    if room < 12 {
        return;
    }
    // 太長時保留後段（路徑的最後幾層最有辨識度）
    let shown = format!(" {} ", tail(&dir, room - 2));
    let w = shown.width() as u16;
    let x = st_x - w;
    buf.set_string(
        x,
        area.y,
        &shown,
        Style::new()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::UNDERLINED),
    );
    app.click_hits.push(ClickHit {
        rect: Rect::new(x, area.y, w, 1),
        click: Click::OpenDir,
    });
}

fn draw_screen(buf: &mut Buffer, s: &Screen, area: Rect) {
    let tc = truecolor();
    let rows = (s.rows as usize)
        .min(area.height as usize)
        .min(s.lines.len());
    for y in 0..rows {
        let line = &s.lines[y];
        let cols = line.len().min(area.width as usize);
        for x in 0..cols {
            let c = &line[x];
            if c.flags & flags::SPACER != 0 {
                continue;
            }
            if c.flags & flags::WIDE != 0 && x + 1 >= area.width as usize {
                continue; // 全形字元放不下
            }
            let mut style = Style::new().fg(color(c.fg, tc)).bg(color(c.bg, tc));
            let mut m = Modifier::empty();
            if c.flags & flags::BOLD != 0 {
                m |= Modifier::BOLD;
            }
            if c.flags & flags::ITALIC != 0 {
                m |= Modifier::ITALIC;
            }
            if c.flags & flags::UNDERLINE != 0 {
                m |= Modifier::UNDERLINED;
            }
            if c.flags & flags::INVERSE != 0 {
                m |= Modifier::REVERSED;
            }
            if c.flags & flags::DIM != 0 {
                m |= Modifier::DIM;
            }
            if c.flags & flags::STRIKE != 0 {
                m |= Modifier::CROSSED_OUT;
            }
            if c.flags & flags::HIDDEN != 0 {
                m |= Modifier::HIDDEN;
            }
            style = style.add_modifier(m);
            let cell = &mut buf[(area.x + x as u16, area.y + y as u16)];
            match &c.zw {
                Some(zw) => {
                    let mut sym = String::with_capacity(4 + zw.len());
                    sym.push(c.ch);
                    sym.push_str(zw);
                    cell.set_symbol(&sym);
                }
                None => {
                    let ch = if c.ch == '\0' || (c.ch as u32) < 0x20 {
                        ' '
                    } else {
                        c.ch
                    };
                    cell.set_char(ch);
                }
            }
            cell.set_style(style);
        }
    }
}

fn draw_status(buf: &mut Buffer, app: &mut App, area: Rect) {
    let count = |st: AgentStatus| app.sessions.iter().filter(|s| s.status == st).count();
    let running =
        count(AgentStatus::Running) + count(AgentStatus::Thinking) + count(AgentStatus::Starting);
    let waiting = count(AgentStatus::WaitingInput);
    let done = count(AgentStatus::Done);
    let bar = Style::new().bg(Color::Indexed(235)).fg(Color::Gray);
    buf.set_string(area.x, area.y, " ".repeat(area.width as usize), bar);
    let mut spans = vec![
        Span::styled(format!(" {running} running"), bar.fg(Color::Green)),
        Span::styled(" · ", bar),
        Span::styled(
            format!("{waiting} waiting"),
            if waiting > 0 {
                bar.fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                bar
            },
        ),
        Span::styled(" · ", bar),
        Span::styled(format!("{done} done"), bar),
        Span::styled(" │ ", bar.fg(Color::DarkGray)),
    ];
    if app.prefix_armed {
        spans.push(Span::styled(
            format!("{} ▸ 等待指令（? 說明）", app.prefix.label()),
            bar.fg(Color::Black).bg(Color::Yellow),
        ));
    } else if let Some(sc) = &app.scroll {
        match &sc.searching {
            Some(q) => spans.push(Span::styled(format!("搜尋：{q}▏"), bar.fg(Color::Cyan))),
            None => spans.push(Span::styled(
                "滾動模式：↑↓ PgUp PgDn · / 搜尋 · n 下一個 · q 離開",
                bar.fg(Color::Cyan),
            )),
        }
    }
    let show_buttons = !app.prefix_armed && app.scroll.is_none();
    let right: Option<(String, Style)> = if let Some((m, err, _)) = &app.message {
        Some((
            m.clone(),
            if *err {
                bar.fg(Color::Red)
            } else {
                bar.fg(Color::White)
            },
        ))
    } else {
        app.config_error
            .as_ref()
            .map(|e| (format!("設定檔錯誤：{e}"), bar.fg(Color::Red)))
    };
    let left = Line::from(spans);
    let mut left_w = left.width() as u16;
    buf.set_line(area.x, area.y, &left, area.width);
    if show_buttons {
        // 可點擊的按鈕；沒有分頁時，只對分頁有效的按鈕改用暗色
        let has_active = app.active.is_some();
        let on = Style::new().fg(Color::Black).bg(Color::Indexed(37));
        let off = Style::new().fg(Color::Indexed(244)).bg(Color::Indexed(238));
        let items: [(&str, char, bool); 7] = [
            (" + 新增 ", 'n', true),
            (" Diff ", 'd', has_active),
            (" 收尾 ", 'm', has_active),
            (" 目錄 ", 'o', has_active),
            (" 改名 ", 'r', has_active),
            (" 關閉 ", 'x', has_active),
            (" 說明 ", '?', true),
        ];
        let end = area.x + area.width;
        let mut x = area.x + left_w;
        // 畫面上有網址（例如登入網址）時，最前面顯示醒目的「複製網址」按鈕與快捷鍵提示
        if app.screen.as_ref().is_some_and(|s| !s.urls().is_empty()) {
            let label = format!(" 複製網址 {} u ", app.prefix.label());
            if x + label.width() as u16 <= end {
                let style = Style::new()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD);
                x += button(
                    buf,
                    &mut app.click_hits,
                    x,
                    area.y,
                    &label,
                    style,
                    Click::Command('u'),
                );
                x += 1;
            }
        }
        for (label, ch, enabled) in items {
            if x + label.width() as u16 > end {
                break;
            }
            let style = if enabled { on } else { off };
            x += button(
                buf,
                &mut app.click_hits,
                x,
                area.y,
                label,
                style,
                Click::Command(ch),
            );
            x += 1;
        }
        let hint = format!(" {} 前綴", app.prefix.label());
        if x + hint.width() as u16 <= end {
            buf.set_string(x, area.y, &hint, bar);
            x += hint.width() as u16;
        }
        left_w = x.min(end) - area.x;
    }
    if let Some((text, style)) = right {
        let room = area.width.saturating_sub(left_w + 3) as usize;
        if room > 4 {
            let t = truncate(&text.replace('\n', " "), room);
            let x = area.x + area.width - t.width() as u16 - 1;
            buf.set_string(x, area.y, &t, style);
        }
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(10);
    let h = h.min(area.height.saturating_sub(2)).max(3);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// 依顯示寬度逐字折行（全形字佔 2 欄），每行前面加一個空白。
/// 不用 ratatui 的依單字折行：它遇到比寬度還長的單字（網址）會在前後多插入空行，
/// 行數無法預估，確認框的按鈕就會被擠到框外。
fn wrap_lines(text: &str, w: usize) -> Vec<String> {
    let w = w.max(2);
    let mut out = vec![];
    for l in text.lines() {
        let mut cur = String::from(" ");
        let mut cw = 1;
        for c in l.chars() {
            let n = c.width().unwrap_or(0);
            if cw + n > w {
                out.push(std::mem::take(&mut cur));
                cw = 0;
            }
            cur.push(c);
            cw += n;
        }
        out.push(cur);
    }
    out
}

/// 確認框的內容放不下、被截斷時，最後的說明（由 [`wrap_lines`] 加上行首空白）。
/// 剪貼簿確認要說清楚：畫面只顯示一部分，但按 y 會複製全部內容。
fn truncated_note(action: &Confirm) -> String {
    match action {
        Confirm::Clipboard(text) => {
            format!("…（只顯示部分，按 y 複製全部 {} 字）", text.chars().count())
        }
        _ => "…（內容太長，已截斷）".into(),
    }
}

/// 確認框的版面：(外框, 內容區, 按鈕列的 y, 折行後的內容)。
/// 高度依框的實際寬度折行後的行數，再加上空行與按鈕列；
/// 超過畫面時縮小內容區（截斷內容），按鈕列固定在框內最後一行。
fn confirm_layout(area: Rect, text: &str) -> (Rect, Rect, u16, Vec<String>) {
    let want = text.lines().map(|l| l.width()).max().unwrap_or(20) as u16 + 4;
    let w = want.max(40).min(area.width.saturating_sub(2)).max(10);
    // 左右框線各佔一欄
    let lines = wrap_lines(text, w.saturating_sub(2) as usize);
    let r = centered(area, w, (lines.len() + 4).min(u16::MAX as usize) as u16);
    let inner = Rect::new(
        r.x + 1,
        r.y + 1,
        r.width.saturating_sub(2),
        r.height.saturating_sub(2),
    );
    let by = inner.y + inner.height.saturating_sub(1);
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(2),
    );
    (r, body, by, lines)
}

fn block(title: &str) -> Block<'_> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().add_modifier(Modifier::BOLD),
        ))
}

/// 目錄瀏覽器（蓋在新增分頁表單上面）。
fn draw_browser(f: &mut Frame, area: Rect, b: &crate::dirs::DirBrowser, hits: &mut Vec<ClickHit>) {
    use crate::dirs::Kind;
    let h = area.height.saturating_sub(4).clamp(8, 26);
    let r = centered(area, 78, h);
    f.render_widget(Clear, r);
    let inner_w = r.width.saturating_sub(2) as usize;
    // 框內：第 1 列位置、第 2 列篩選、清單、最後一列提示
    let list_h = r.height.saturating_sub(2 + 3) as usize;
    let items = b.visible();
    let offset = if b.selected >= list_h {
        b.selected + 1 - list_h
    } else {
        0
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" 位置 ", Style::new().fg(Color::Gray)),
            Span::styled(
                truncate(&b.title(), inner_w.saturating_sub(7)),
                Style::new().add_modifier(Modifier::BOLD),
            ),
        ]),
        match (&b.error, b.filter.is_empty()) {
            (Some(e), _) => Line::styled(format!(" {e}"), Style::new().fg(Color::Red)),
            (None, true) => Line::styled(
                " 輸入文字篩選 · ⎇ = git repo",
                Style::new().fg(Color::DarkGray),
            ),
            (None, false) => Line::from(vec![
                Span::styled(" 篩選 ", Style::new().fg(Color::Gray)),
                Span::styled(format!("{}▏", b.filter), Style::new().fg(Color::Yellow)),
            ]),
        },
    ];
    for (i, e) in items.iter().enumerate().skip(offset).take(list_h) {
        let (icon, label) = match e.kind {
            Kind::UseThis => (
                "✓",
                format!("使用這個目錄{}", if e.git { "（git repo）" } else { "" }),
            ),
            Kind::Parent => ("↑", "..（上一層）".to_string()),
            Kind::Drive => ("▣", e.name.clone()),
            Kind::Dir => (if e.git { "⎇" } else { "▸" }, e.name.clone()),
        };
        let text = truncate(&format!(" {icon} {label}"), inner_w);
        let pad = " ".repeat(inner_w.saturating_sub(text.width()));
        let style = if i == b.selected {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else if e.kind == Kind::UseThis {
            Style::new().fg(Color::Green)
        } else if e.git {
            Style::new().fg(Color::Yellow)
        } else {
            Style::new()
        };
        lines.push(Line::styled(format!("{text}{pad}"), style));
    }
    while lines.len() < 2 + list_h {
        lines.push(Line::raw(""));
    }
    lines.push(Line::styled(
        " ↑↓ 移動 · Enter/→ 進入 · ←/Backspace 上一層 · Tab 選定反白的目錄 · Esc 取消",
        Style::new().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(lines).block(block("選擇 Repo 目錄")), r);
    hits.push(ClickHit {
        rect: r,
        click: Click::Nothing,
    });
    let shown = items.len().saturating_sub(offset).min(list_h);
    for row in 0..shown {
        hits.push(ClickHit {
            rect: Rect::new(r.x + 1, r.y + 3 + row as u16, r.width.saturating_sub(2), 1),
            click: Click::Item(offset + row),
        });
    }
}

/// 畫彈出視窗，回傳其中可點擊的範圍。
fn draw_popup(f: &mut Frame, app: &App) -> Vec<ClickHit> {
    let area = f.area();
    let p = app.prefix.label();
    let mut hits = Vec::new();
    let Some(popup) = &app.popup else {
        return hits;
    };
    // 先登記整個畫面：點彈出視窗以外的地方等於按 Esc
    // （新增表單除外，避免誤點而遺失已輸入的內容）
    if !matches!(popup, Popup::New(_)) {
        hits.push(ClickHit {
            rect: area,
            click: Click::Key(KeyCode::Esc),
        });
    }
    match popup {
        Popup::Help => {
            let rows = [
                ("n", "新增分頁：選 Agent、repo、輸入任務名稱"),
                ("j / k、↓ / ↑", "切換分頁"),
                ("1 – 9", "跳到第 N 個分頁"),
                ("w", "跳到下一個等待輸入的分頁"),
                ("d", "浮動視窗顯示 diff"),
                ("m", "收尾選單：merge / pr / keep / discard"),
                ("r", "重新命名任務"),
                ("R", "重新啟動已結束的 Agent，接續上次的對話"),
                ("N", "重新啟動已結束的 Agent，開新對話"),
                ("[", "進入滾動模式，/ 搜尋歷史"),
                ("u", "複製畫面上最新的網址（例如登入網址）"),
                (
                    "o",
                    "用檔案總管開啟分頁的工作目錄（worktree）；也可以雙擊標題列的路徑",
                ),
                ("x", "關閉分頁（Agent 執行中會先確認）"),
                ("b", "收合或展開左側欄"),
                ("?", "快捷鍵說明"),
                ("q", "detach：只關閉 UI，Agent 繼續執行"),
                (&*format!("{p} 再按一次"), "送出真正的前綴鍵"),
            ];
            let mut lines = vec![
                Line::raw(format!("所有指令都先按前綴鍵 {p}：")),
                Line::raw(""),
            ];
            for (k, d) in rows {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<14}", k), Style::new().fg(Color::Yellow)),
                    Span::raw(d.to_string()),
                ]));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "  燈號：● 執行中  ◐ 思考中  ⚠ 等待輸入  ✓ 完成  ✗ 錯誤  ○ 閒置/結束",
                Style::new().fg(Color::Gray),
            ));
            lines.push(Line::styled(
                "  +N 領先 base 的 commit 數，✎ 有未 commit 的變更",
                Style::new().fg(Color::Gray),
            ));
            lines.push(Line::styled(
                "  滑鼠：點左側欄切換分頁；點側欄與底部狀態列的按鈕執行指令",
                Style::new().fg(Color::Gray),
            ));
            let r = centered(area, 72, lines.len() as u16 + 2);
            f.render_widget(Clear, r);
            f.render_widget(
                Paragraph::new(lines).block(block("快捷鍵（點任意處關閉）")),
                r,
            );
        }
        Popup::New(form) => {
            let r = centered(area, 70, 11);
            f.render_widget(Clear, r);
            let field = |i: usize, label: &str, value: String| {
                let focused = form.field == i;
                let style = if focused {
                    Style::new().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::new()
                };
                let cursor = if focused && i != 0 { "▏" } else { "" };
                Line::from(vec![
                    Span::styled(format!("  {label:<8}"), Style::new().fg(Color::Gray)),
                    Span::styled(format!(" {value}{cursor} "), style),
                ])
            };
            let agent = form.agents.get(form.agent).cloned().unwrap_or_default();
            let mut lines = vec![
                Line::raw(""),
                field(0, "Agent", format!("◀ {agent} ▶")),
                field(
                    1,
                    "Repo",
                    // 留位置給右邊的「瀏覽…」按鈕；太長時保留路徑結尾
                    tail(&form.repo, (r.width as usize).saturating_sub(10 + 4 + 10)),
                ),
                field(2, "任務", form.task.clone()),
                field(
                    3,
                    "Base",
                    if form.base.is_empty() && form.field != 3 {
                        "（預設：目前的 HEAD branch）".into()
                    } else {
                        form.base.clone()
                    },
                ),
                Line::raw(""),
            ];
            let ok = " 建立 (Enter) ";
            let cancel = " 取消 (Esc) ";
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(ok, button_style()),
                Span::raw("  "),
                Span::styled(cancel, Style::new().fg(Color::Black).bg(Color::Gray)),
            ]));
            lines.push(Line::styled(
                "  點欄位輸入 · 點 ◀ ▶ 選 Agent · Tab/↑↓ 切換欄位 · Ctrl+O 瀏覽目錄",
                Style::new().fg(Color::DarkGray),
            ));
            f.render_widget(Paragraph::new(lines).block(block("新增分頁")), r);
            // Repo 列最右邊的「瀏覽…」按鈕
            let browse = " 瀏覽… ";
            let bw = browse.width() as u16;
            let bx = (r.x + r.width).saturating_sub(bw + 2);
            f.buffer_mut()
                .set_string(bx, r.y + 3, browse, button_style());
            // 框內第 1 列空白，第 2–5 列是欄位，第 7 列是按鈕
            let ix = r.x + 1;
            let iw = r.width.saturating_sub(2);
            hits.push(ClickHit {
                rect: r,
                click: Click::Nothing,
            });
            for i in 0..4u16 {
                hits.push(ClickHit {
                    rect: Rect::new(ix, r.y + 2 + i, iw, 1),
                    click: Click::Field(i as usize),
                });
            }
            // Agent 列："  Agent   " 佔 10 欄，接著是 " ◀ name ▶ "
            let ax = ix + 10 + 1;
            hits.push(ClickHit {
                rect: Rect::new(ax, r.y + 2, 2, 1),
                click: Click::Agent(-1),
            });
            let nx = ax + format!("◀ {agent} ").width() as u16;
            hits.push(ClickHit {
                rect: Rect::new(nx, r.y + 2, 2, 1),
                click: Click::Agent(1),
            });
            let by = r.y + 1 + 6;
            if by + 1 < r.y + r.height {
                let okx = ix + 2;
                hits.push(ClickHit {
                    rect: Rect::new(okx, by, ok.width() as u16, 1),
                    click: Click::Key(KeyCode::Enter),
                });
                hits.push(ClickHit {
                    rect: Rect::new(okx + ok.width() as u16 + 2, by, cancel.width() as u16, 1),
                    click: Click::Key(KeyCode::Esc),
                });
            }
            hits.push(ClickHit {
                rect: Rect::new(bx, r.y + 3, bw, 1),
                click: Click::Browse,
            });
            if let Some(b) = &form.browser {
                draw_browser(f, area, b, &mut hits);
            }
        }
        Popup::Finish { selected } => {
            let mut lines = vec![Line::raw("")];
            for (i, (name, desc)) in FINISH_ITEMS.iter().enumerate() {
                let style = if i == *selected {
                    Style::new().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::new()
                };
                lines.push(Line::styled(format!("  {name:<8} {desc}  "), style));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "  點選項或 ↑↓ 選擇 · Enter 執行 · Esc 取消",
                Style::new().fg(Color::DarkGray),
            ));
            let r = centered(area, 56, lines.len() as u16 + 2);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(lines).block(block("收尾")), r);
            menu_hits(&mut hits, r, FINISH_ITEMS.len());
        }
        Popup::Strategy { selected } => {
            let desc = [
                "merge --no-ff",
                "squash 成一個 commit",
                "rebase 後 fast-forward",
            ];
            let mut lines = vec![Line::raw("")];
            for (i, s) in STRATEGIES.iter().enumerate() {
                let style = if i == *selected {
                    Style::new().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::new()
                };
                lines.push(Line::styled(
                    format!("  {:<8} {}  ", format!("{s:?}").to_lowercase(), desc[i]),
                    style,
                ));
            }
            let r = centered(area, 44, lines.len() as u16 + 2);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(lines).block(block("merge 策略")), r);
            menu_hits(&mut hits, r, STRATEGIES.len());
        }
        Popup::Confirm { text, action } => {
            let yes = " 是 (y) ";
            let no = " 否 (n) ";
            let (r, body, by, lines) = confirm_layout(area, text);
            f.render_widget(Clear, r);
            f.render_widget(block("確認"), r);
            // 內容太長時只截斷內容；按鈕固定在最後一行，一定看得到
            let room = body.height as usize;
            let mut lines: Vec<Line> = if lines.len() > room {
                // 截斷說明也依寬度折行，整句一定顯示完整（含字數）
                let note = wrap_lines(&truncated_note(action), body.width as usize);
                let keep = room.saturating_sub(note.len());
                let dim = Style::new().fg(Color::DarkGray);
                lines
                    .into_iter()
                    .take(keep)
                    .map(Line::raw)
                    .chain(note.into_iter().map(|l| Line::styled(l, dim)))
                    .collect()
            } else {
                lines.into_iter().map(Line::raw).collect()
            };
            lines.truncate(room);
            f.render_widget(Paragraph::new(lines), body);
            let yx = r.x + 2;
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(yes, button_style()),
                    Span::raw("  "),
                    Span::styled(no, Style::new().fg(Color::Black).bg(Color::Gray)),
                ])),
                Rect::new(yx, by, r.width.saturating_sub(3), 1),
            );
            hits.push(ClickHit {
                rect: r,
                click: Click::Nothing,
            });
            hits.push(ClickHit {
                rect: Rect::new(yx, by, yes.width() as u16, 1),
                click: Click::Key(KeyCode::Char('y')),
            });
            hits.push(ClickHit {
                rect: Rect::new(yx + yes.width() as u16 + 2, by, no.width() as u16, 1),
                click: Click::Key(KeyCode::Char('n')),
            });
        }
        Popup::Rename { input } => {
            let r = centered(area, 60, 3);
            f.render_widget(Clear, r);
            f.render_widget(
                Paragraph::new(format!(" {input}▏"))
                    .block(block("重新命名任務（Enter 確定，點外面取消）")),
                r,
            );
            hits.push(ClickHit {
                rect: r,
                click: Click::Nothing,
            });
        }
        Popup::Text {
            title,
            lines,
            scroll,
            diff,
        } => {
            let r = centered(
                area,
                area.width.saturating_sub(6),
                area.height.saturating_sub(4),
            );
            let inner_h = r.height.saturating_sub(2) as usize;
            let shown: Vec<Line> = lines
                .iter()
                .skip(*scroll)
                .take(inner_h)
                .map(|l| {
                    let style = if !diff {
                        Style::new()
                    } else if l.starts_with("###") {
                        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff ")
                    {
                        Style::new().add_modifier(Modifier::BOLD)
                    } else if l.starts_with('+') {
                        Style::new().fg(Color::Green)
                    } else if l.starts_with('-') {
                        Style::new().fg(Color::Red)
                    } else if l.starts_with("@@") {
                        Style::new().fg(Color::Cyan)
                    } else {
                        Style::new()
                    };
                    Line::styled(l.clone(), style)
                })
                .collect();
            let t = format!(
                "{title}  {}/{} · ↑↓ PgUp PgDn 滾輪 · q 關閉",
                (*scroll + 1).min(lines.len().max(1)),
                lines.len()
            );
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(shown).block(block(&t)), r);
            hits.push(ClickHit {
                rect: r,
                click: Click::Nothing,
            });
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confirm_text() -> String {
        let long =
            "https://claude.ai/oauth/authorize?code=true&client_id=".to_string() + &"x".repeat(340);
        format!("分頁要求把以下內容複製到剪貼簿（400 字）：\n\n{long}\n\n要複製嗎？")
    }

    /// 剪貼簿確認框：一行很長的內容會折行，按鈕不能被擠到框外。
    #[test]
    fn confirm_buttons_stay_inside_with_long_lines() {
        let text = confirm_text();
        for (w, h) in [(120u16, 40u16), (80, 24), (50, 12), (30, 8), (12, 5)] {
            let (r, body, by, _) = confirm_layout(Rect::new(0, 0, w, h), &text);
            assert!(r.x + r.width <= w && r.y + r.height <= h, "{w}x{h}: {r:?}");
            // 按鈕列在框內（不是框線上），內容區在按鈕列上面
            assert!(
                by > r.y && by < r.y + r.height - 1,
                "{w}x{h}: {r:?} by={by}"
            );
            assert!(body.y + body.height <= by, "{w}x{h}: {body:?} by={by}");
        }
        // 畫面夠大時整段內容都放得下
        let (_, body, _, lines) = confirm_layout(Rect::new(0, 0, 120, 40), &text);
        assert!(
            body.height as usize >= lines.len(),
            "{body:?} {}",
            lines.len()
        );
    }

    #[test]
    fn truncated_note_shows_total_for_clipboard() {
        let note = truncated_note(&Confirm::Clipboard("中".repeat(1234)));
        assert!(note.contains("全部 1234 字"), "{note}");
        let other = truncated_note(&Confirm::Close { id: "x".into() });
        assert!(other.contains("已截斷") && !other.contains("字"), "{other}");
    }

    #[test]
    fn wrap_lines_by_display_width() {
        assert_eq!(wrap_lines("abc", 10), [" abc"]);
        assert_eq!(wrap_lines("abcdef", 4), [" abc", "def"]);
        // 全形字不會被切成一半
        assert_eq!(wrap_lines("中文字", 4), [" 中", "文字"]);
        assert_eq!(wrap_lines("a\n\nb", 10), [" a", " ", " b"]);
        for l in wrap_lines(&confirm_text(), 30) {
            assert!(l.width() <= 30, "{l}");
        }
    }

    /// 依 confirm_layout 的結果畫出來，確認按鈕與最後一行問句都在畫面上。
    #[test]
    fn confirm_renders_buttons() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let text = confirm_text();
        for (w, h) in [(80u16, 24u16), (60, 30)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| {
                let (r, body, by, lines) = confirm_layout(f.area(), &text);
                f.render_widget(block("確認"), r);
                let lines: Vec<Line> = lines.into_iter().map(Line::raw).collect();
                f.render_widget(Paragraph::new(lines), body);
                f.render_widget(
                    Paragraph::new(" 是 (y)   否 (n) "),
                    Rect::new(r.x + 2, by, 20, 1),
                );
            })
            .unwrap();
            let buf = t.backend().buffer().clone();
            let screen: String = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            // 全形字後面的格子是空白，比對前先去掉空白
            let compact: String = screen.chars().filter(|c| *c != ' ').collect();
            assert!(compact.contains("是(y)否(n)"), "{screen}");
            assert!(
                compact.contains("要複製嗎？"),
                "整段內容都要放得下：\n{screen}"
            );
        }
    }

    #[test]
    fn truncate_cjk() {
        assert_eq!(truncate("登入功能", 8), "登入功能");
        assert_eq!(truncate("登入功能實作", 8), "登入功…");
        assert_eq!(tail(r"C:\Users\me\proj", 8), r"…me\proj");
        assert_eq!(tail("short", 8), "short");
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(rgb_to_256(255, 0, 0), 196);
    }
}
