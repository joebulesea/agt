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

/// Agent 結束後的提示列：放在最後一列內容下方空一列；畫面滿了就放在最底下。
#[test]
fn exit_hint_below_last_content() {
    use agt_proto::screen::{Cell, Cursor};
    let screen = |texts: &[&str], rows: u16| Screen {
        cols: 20,
        rows,
        lines: (0..rows as usize)
            .map(|y| {
                let t = texts.get(y).copied().unwrap_or("");
                let mut r: Vec<Cell> = t
                    .chars()
                    .map(|ch| Cell {
                        ch,
                        ..Cell::default()
                    })
                    .collect();
                r.resize(20, Cell::default());
                r
            })
            .collect(),
        cursor: Cursor::default(),
        modes: 0,
        display_offset: 0,
        history: 0,
    };
    assert_eq!(exit_hint_row(&screen(&[], 10), 10), 0);
    assert_eq!(exit_hint_row(&screen(&["", "Resume", "claude"], 10), 10), 4);
    let full: Vec<&str> = vec!["x"; 10];
    assert_eq!(exit_hint_row(&screen(&full, 10), 10), 9);
    // 主區域比畫面矮時也不超出
    assert_eq!(exit_hint_row(&screen(&["", "", "", "a"], 10), 3), 2);
}

#[test]
fn truncated_note_shows_total_for_clipboard() {
    let note = truncated_note(&Confirm::Clipboard("中".repeat(1234)));
    assert!(note.contains("全部 1234 字"), "{note}");
    let other = truncated_note(&Confirm::Close { id: "x".into() });
    assert!(other.contains("已截斷") && !other.contains("字"), "{other}");
}

/// 錯誤視窗：長訊息依視窗內寬折行（全形字 2 欄），空行保留；diff 不折行。
#[test]
fn text_popup_wraps_errors_but_not_diff() {
    // 畫面 20 欄 → 外框 14、內寬 12
    let lines = vec![
        "git gh pr create 失敗：Base sha".to_string(),
        String::new(),
        "ok".to_string(),
    ];
    let rows = text_rows(&lines, false, 20);
    assert!(rows.iter().all(|r| r.width() <= 12), "{rows:?}");
    assert_eq!(rows.concat(), lines.concat());
    assert!(rows.len() > lines.len());
    assert_eq!(rows[rows.len() - 2..], ["".to_string(), "ok".to_string()]);
    assert_eq!(text_rows(&lines, true, 20), lines);
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

/// diff 視窗：頂端有分頁列（目前頁反白）；內容比視窗高時右側畫出捲軸。
#[test]
fn diff_popup_renders_tab_bar_and_scrollbar() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let (client, _sent) = agt_proto::client::Client::detached();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(client, Default::default(), None, ".".into(), tx);
    let first: Vec<String> = (0..60).map(|i| format!("+line{i}")).collect();
    let tabs = vec![
        ("已 commit（main...HEAD）".to_string(), first.clone()),
        ("未 commit 的變更".to_string(), vec!["（無）".to_string()]),
    ];
    app.popup = Some(Popup::Text {
        title: "diff".into(),
        lines: first,
        scroll: 0,
        diff: true,
        tabs,
        tab: 0,
    });
    let (w, h) = (80u16, 20u16);
    let render = |app: &App| {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| {
            draw_popup(f, app);
        })
        .unwrap();
        t.backend().buffer().clone()
    };
    let text = |b: &ratatui::buffer::Buffer, y: u16| {
        (0..w)
            .map(|x| b[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    let compact = |b: &ratatui::buffer::Buffer| -> String {
        // 全形字後面的格子是空白，比對前先去掉空白
        (0..h)
            .map(|y| text(b, y))
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    };
    let has_bar = |b: &ratatui::buffer::Buffer| {
        (0..h).any(|y| (0..w).any(|x| matches!(b[(x, y)].symbol(), "█" | "▲" | "▼")))
    };
    let buf = render(&app);
    let all = compact(&buf);
    assert!(all.contains("1已commit（main...HEAD）"), "{all}");
    assert!(all.contains("2未commit的變更"), "{all}");
    assert!(all.contains("Tab/←→/1-9切換分頁"), "{all}");
    assert!(all.contains("+line0"), "{all}");
    assert!(!all.contains("###"), "{all}");
    // 目前頁（第 1 頁）反白：分頁列的底色是 Cyan
    let bar_y = (0..h)
        .find(|y| text(&buf, *y).contains("commit（"))
        .unwrap();
    assert!((0..w).any(|x| buf[(x, bar_y)].bg == Color::Cyan && buf[(x, bar_y)].symbol() == "1"));
    // 內容超出高度：畫出捲軸
    assert!(has_bar(&buf), "內容比視窗高，應該有捲軸");

    // 切到只有一行的第 2 頁：沒有捲軸，內容換成該頁
    if let Some(Popup::Text {
        tabs,
        tab,
        lines,
        scroll,
        ..
    }) = app.popup.as_mut()
    {
        *tab = 1;
        *lines = tabs[1].1.clone();
        *scroll = 0;
    }
    let buf = render(&app);
    let all = compact(&buf);
    assert!(all.contains("（無）") && !all.contains("+line0"), "{all}");
    assert!(!has_bar(&buf), "內容放得下時不應有捲軸");
}
