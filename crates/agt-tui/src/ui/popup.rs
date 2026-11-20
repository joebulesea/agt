//! 對話框與目錄瀏覽器的繪製。

use super::*;

pub(super) fn centered(area: Rect, w: u16, h: u16) -> Rect {
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
pub(super) fn wrap_lines(text: &str, w: usize) -> Vec<String> {
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

/// 文字視窗（錯誤訊息等）的外框寬度：畫面寬度左右各留 3 欄。
pub(super) fn text_popup_width(screen_w: u16) -> u16 {
    screen_w.saturating_sub(6)
}

/// 文字視窗實際顯示的列：一般文字依視窗內寬逐字折行（全形字佔 2 欄），
/// diff 不折行（保留程式碼的排版）。捲動位置以這裡的列數計算。
pub fn text_rows(lines: &[String], diff: bool, screen_w: u16) -> Vec<String> {
    if diff {
        return lines.to_vec();
    }
    let w = (text_popup_width(screen_w).saturating_sub(2) as usize).max(1);
    let mut out = vec![];
    for l in lines {
        let mut cur = String::new();
        let mut cw = 0;
        for c in l.chars() {
            let n = c.width().unwrap_or(0);
            if cw + n > w && !cur.is_empty() {
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
pub(super) fn truncated_note(action: &Confirm) -> String {
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
pub(super) fn confirm_layout(area: Rect, text: &str) -> (Rect, Rect, u16, Vec<String>) {
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

pub(super) fn block(title: &str) -> Block<'_> {
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
pub(super) fn draw_browser(
    f: &mut Frame,
    area: Rect,
    b: &crate::dirs::DirBrowser,
    hits: &mut Vec<ClickHit>,
) {
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
pub(super) fn draw_popup(f: &mut Frame, app: &App) -> Vec<ClickHit> {
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
                ("x", "關閉分頁（選擇刪除或保留 worktree 副本）"),
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
            // 收尾（merge / pr）前會自動 commit 的 Agent，在欄位上標出來
            let agent_label = if app.ui.auto_commit_agents.contains(&agent) {
                format!("◀ {agent} ▶  auto_commit")
            } else {
                format!("◀ {agent} ▶")
            };
            // Base 留空時實際會用哪個 branch：主 repo 不在預設 branch 上時，PR 與 merge 的目標會跟著它
            let (base_hint, base_warn) = if form.base.trim().is_empty() {
                let repo = crate::app::expand_home(form.repo.trim());
                let head = crate::repohead::read(std::path::Path::new(&repo));
                crate::repohead::base_hint(head.as_ref())
            } else {
                (String::new(), None)
            };
            let mut lines = vec![
                Line::raw(""),
                field(0, "Agent", agent_label),
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
                        base_hint
                    } else {
                        form.base.clone()
                    },
                ),
                // 平常是空行；Base 不是預設 branch 時改成警告（框的高度不變）
                match base_warn {
                    Some(w) => Line::styled(
                        format!("  {}", truncate(&w, (r.width as usize).saturating_sub(4))),
                        Style::new().fg(Color::Yellow),
                    ),
                    None => Line::raw(""),
                },
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
        Popup::Close { busy, selected, .. } => {
            let mut lines = vec![Line::raw("")];
            if let Some(agent) = busy {
                lines.push(Line::styled(
                    format!("  {agent} 仍在執行中，關閉會結束它"),
                    Style::new().fg(Color::Yellow),
                ));
                lines.push(Line::raw(""));
            }
            let top = lines.len();
            for (i, (name, desc)) in CLOSE_ITEMS.iter().enumerate() {
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
            f.render_widget(Paragraph::new(lines).block(block("關閉分頁")), r);
            hits.push(ClickHit {
                rect: r,
                click: Click::Nothing,
            });
            // menu_hits 假設選項從第二行開始；有提醒時往下移
            let off = (top - 1) as u16;
            menu_hits(
                &mut hits,
                Rect::new(r.x, r.y + off, r.width, r.height.saturating_sub(off)),
                CLOSE_ITEMS.len(),
            );
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
                text_popup_width(area.width),
                area.height.saturating_sub(4),
            );
            let inner_h = r.height.saturating_sub(2) as usize;
            let rows = text_rows(lines, *diff, area.width);
            let lines = &rows;
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
                "{title}  {}/{} · ↑↓ PgUp PgDn 滾輪 · c 複製 · q 關閉",
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
