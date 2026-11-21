//! 左側欄、分頁標題、主畫面、結束提示與底部狀態列的繪製。

use super::*;

pub(super) fn draw_sidebar(buf: &mut Buffer, app: &mut App, area: Rect, collapsed: bool) {
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
        // 展開按鈕（與 b 鍵相同）
        if area.width >= 4 {
            button(
                buf,
                &mut app.click_hits,
                area.x + 3,
                y,
                "»",
                button_style(),
                Click::Command('b'),
            );
        }
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
        // 收合按鈕（與 b 鍵相同），放在標題右側；空間不足時不顯示以免蓋到「新增」
        let new_visible = area.width >= bw + 6;
        let new_x = area.x + area.width.saturating_sub(bw + 1);
        if area.width >= 8 && (!new_visible || new_x >= area.x + 8) {
            button(
                buf,
                &mut app.click_hits,
                area.x + 5,
                y,
                " « ",
                button_style(),
                Click::Command('b'),
            );
        }
        if new_visible {
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
            Style::new().fg(theme().dark_gray)
        } else {
            status_style(s.status)
        };
        let row_style = if active {
            Style::new()
                .bg(theme().sel_bg)
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
                Style::new().fg(theme().blue),
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
            Style::new().fg(theme().dark_gray).patch(row_style),
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
                Style::new().fg(theme().yellow).patch(row_style),
            );
        }
        y += 1;
        // 第二行：branch +N 或「無 worktree」；有 auto_commit 的 Agent 在行尾加「auto」
        if y < bottom {
            let auto = app.ui.auto_commit_agents.contains(&s.agent);
            let auto_tag = if auto { " auto" } else { "" };
            let detail = match &s.branch {
                Some(b) => {
                    let ahead = if s.ahead > 0 {
                        format!(" +{}", s.ahead)
                    } else {
                        String::new()
                    };
                    let room = w.saturating_sub(6 + ahead.width() + auto_tag.width());
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
                    truncate(&detail, w.saturating_sub(6 + auto_tag.width())),
                    Style::new().fg(theme().dark_gray).patch(row_style),
                );
                if auto && w > 6 + auto_tag.width() {
                    buf.set_string(
                        area.x + (w - auto_tag.width()) as u16,
                        y,
                        auto_tag,
                        Style::new().fg(theme().green).patch(row_style),
                    );
                }
                y += 1;
            }
        }
        // 第三行（有標籤時）：#標籤 #標籤
        if !s.tags.is_empty() && y < bottom {
            let text = s
                .tags
                .iter()
                .map(|t| format!("#{t}"))
                .collect::<Vec<_>>()
                .join(" ");
            buf.set_string(area.x, y, pad("", w), row_style);
            buf.set_string(
                area.x + 6,
                y,
                truncate(&text, w.saturating_sub(6)),
                Style::new().fg(theme().cyan).patch(row_style),
            );
            y += 1;
        }
    }
}

pub(super) fn draw_title(buf: &mut Buffer, app: &mut App, area: Rect) {
    let border = Style::new().fg(theme().dark_gray);
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
    if app.ui.auto_commit_agents.contains(&s.agent) {
        title.push_str("[auto_commit] ");
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
            .fg(theme().dark_gray)
            .add_modifier(Modifier::UNDERLINED),
    );
    app.click_hits.push(ClickHit {
        rect: Rect::new(x, area.y, w, 1),
        click: Click::OpenDir,
    });
}

pub(super) fn draw_screen(buf: &mut Buffer, s: &Screen, area: Rect) {
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

/// 提示列放在畫面最後一列有內容的下一列（空一列）；放不下時放在最底下一列。
pub(super) fn exit_hint_row(s: &Screen, height: u16) -> u16 {
    let rows = (s.rows as usize).min(s.lines.len());
    let last = (0..rows).rev().find(|&y| {
        s.lines[y]
            .iter()
            .any(|c| !matches!(c.ch, ' ' | '\0') && c.flags & flags::HIDDEN == 0)
    });
    let want = last.map_or(0, |y| y as u16 + 2);
    want.min(height.saturating_sub(1))
}

/// Agent 已結束（例如 /exit、/logout）時，提示怎麼重新啟動，並提供可點擊的按鈕。
pub(super) fn draw_exit_hint(buf: &mut Buffer, app: &mut App, area: Rect) {
    let status = app.active_info().map(|s| s.status);
    if !matches!(status, Some(AgentStatus::Exited | AgentStatus::Error)) || area.height == 0 {
        return;
    }
    let Some(screen) = &app.screen else { return };
    if screen.display_offset > 0 || app.scroll.is_some() {
        return;
    }
    let y = area.y + exit_hint_row(screen, area.height);
    let p = app.prefix.label();
    let note = " Agent 已結束 ";
    let restart = format!(" 重新啟動，接續對話（{p} R） ");
    let fresh = format!(" 開新對話（{p} N） ");
    let need = (note.width() + 1 + restart.width() + 1 + fresh.width()) as u16;
    if need > area.width {
        let short = format!(" Agent 已結束：按 {p} R 重新啟動 ");
        buf.set_string(
            area.x,
            y,
            truncate(&short, area.width as usize),
            Style::new().fg(theme().on_accent).bg(theme().yellow),
        );
        app.click_hits.push(ClickHit {
            rect: Rect::new(area.x, y, area.width.min(short.width() as u16), 1),
            click: Click::Command('R'),
        });
        return;
    }
    buf.set_string(area.x, y, " ".repeat(area.width as usize), Style::new());
    let mut x = area.x;
    buf.set_string(x, y, note, Style::new().fg(theme().on_accent).bg(theme().yellow));
    x += note.width() as u16 + 1;
    x += button(
        buf,
        &mut app.click_hits,
        x,
        y,
        &restart,
        button_style(),
        Click::Command('R'),
    ) + 1;
    button(
        buf,
        &mut app.click_hits,
        x,
        y,
        &fresh,
        Style::new().fg(theme().on_accent).bg(theme().tab_on_bg),
        Click::Command('N'),
    );
}

pub(super) fn draw_status(buf: &mut Buffer, app: &mut App, area: Rect) {
    let count = |st: AgentStatus| app.sessions.iter().filter(|s| s.status == st).count();
    let running =
        count(AgentStatus::Running) + count(AgentStatus::Thinking) + count(AgentStatus::Starting);
    let waiting = count(AgentStatus::WaitingInput);
    let done = count(AgentStatus::Done);
    let bar = Style::new().bg(theme().bar_bg).fg(theme().gray);
    buf.set_string(area.x, area.y, " ".repeat(area.width as usize), bar);
    let mut spans = vec![
        Span::styled(format!(" {running} running"), bar.fg(theme().green)),
        Span::styled(" · ", bar),
        Span::styled(
            format!("{waiting} waiting"),
            if waiting > 0 {
                bar.fg(theme().yellow).add_modifier(Modifier::BOLD)
            } else {
                bar
            },
        ),
        Span::styled(" · ", bar),
        Span::styled(format!("{done} done"), bar),
        Span::styled(" │ ", bar.fg(theme().dark_gray)),
    ];
    if app.prefix_armed {
        spans.push(Span::styled(
            format!("{} ▸ 等待指令（? 說明）", app.prefix.label()),
            bar.fg(theme().on_accent).bg(theme().yellow),
        ));
    } else if let Some(sc) = &app.scroll {
        match &sc.searching {
            Some(q) => spans.push(Span::styled(format!("搜尋：{q}▏"), bar.fg(theme().cyan))),
            None => spans.push(Span::styled(
                "滾動模式：↑↓ PgUp PgDn · / 搜尋 · n 下一個 · q 離開",
                bar.fg(theme().cyan),
            )),
        }
    }
    let show_buttons = !app.prefix_armed && app.scroll.is_none();
    let right: Option<(String, Style)> = if let Some((m, err, _)) = &app.message {
        Some((
            m.clone(),
            if *err {
                bar.fg(theme().red)
            } else {
                bar.fg(theme().text)
            },
        ))
    } else if let Some(e) = &app.config_error {
        Some((format!("設定檔錯誤：{e}"), bar.fg(theme().red)))
    } else {
        // 沒有訊息時在最右邊顯示版本號
        Some((
            format!("v{}", env!("CARGO_PKG_VERSION")),
            bar.fg(theme().gray),
        ))
    };
    // 版本號放不下就不顯示（不截斷）；訊息則截斷後顯示
    let is_version = app.message.is_none() && app.config_error.is_none();
    let left = Line::from(spans);
    let mut left_w = left.width() as u16;
    buf.set_line(area.x, area.y, &left, area.width);
    if show_buttons {
        // 可點擊的按鈕；沒有分頁時，只對分頁有效的按鈕改用暗色
        let has_active = app.active.is_some();
        let on = Style::new().fg(theme().on_accent).bg(theme().tab_on_bg);
        let off = Style::new().fg(theme().tab_off_fg).bg(theme().tab_off_bg);
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
                    .fg(theme().on_accent)
                    .bg(theme().yellow)
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
        if room > 4 && !(is_version && text.width() > room) {
            let t = truncate(&text.replace('\n', " "), room);
            let x = area.x + area.width - t.width() as u16 - 1;
            buf.set_string(x, area.y, &t, style);
        }
    }
}
