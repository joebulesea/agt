use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// 不連 daemon 的 App；回傳的接收端收到 App 送給 daemon 的封包。
fn test_app(osc52: &str) -> (App, mpsc::UnboundedReceiver<Vec<u8>>) {
    let (client, sent) = Client::detached();
    let (tx, _rx) = mpsc::unbounded_channel();
    let ui = UiConfig {
        osc52: osc52.into(),
        ..UiConfig::default()
    };
    (App::new(client, ui, None, ".".into(), tx), sent)
}

fn session(id: &str) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        short_id: id.chars().take(4).collect(),
        agent: "shell".into(),
        repo_path: None,
        repo_name: None,
        task: format!("task-{id}"),
        worktree: None,
        branch: None,
        base_branch: None,
        cwd: ".".into(),
        status: AgentStatus::Idle,
        exit_code: None,
        ahead: 0,
        behind: 0,
        dirty: false,
        created_at: 0,
        status_since: 0,
        prompt: None,
        check: None,
        shared: false,
        tags: vec![],
        usage: None,
        overlaps: Vec::new(),
    }
}

fn key(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn clipboard(id: &str, text: &str) -> Incoming {
    Incoming::Msg(Box::new(ServerMsg::Clipboard {
        id: id.into(),
        text: text.into(),
    }))
}

fn with_active(osc52: &str) -> (App, mpsc::UnboundedReceiver<Vec<u8>>) {
    let (mut app, sent) = test_app(osc52);
    app.upsert(session("aaaa"));
    app.upsert(session("bbbb"));
    app.select(Some("aaaa".into()));
    (app, sent)
}

#[test]
fn osc52_ask_shows_dialog_and_y_copies() {
    let (mut app, _sent) = with_active("ask");
    app.on_incoming(clipboard("aaaa", "echo hi"));
    assert!(matches!(
        app.popup,
        Some(Popup::Confirm {
            action: Confirm::Clipboard(_),
            ..
        })
    ));
    assert!(app.clipboard_out.is_none(), "確認前不能寫入剪貼簿");
    app.on_event(key('y'));
    assert!(app.popup.is_none());
    assert_eq!(app.clipboard_out.as_deref(), Some("echo hi"));
}

#[test]
fn osc52_ask_n_rejects() {
    let (mut app, _sent) = with_active("ask");
    app.on_incoming(clipboard("aaaa", "rm -rf /"));
    app.on_event(key('n'));
    assert!(app.popup.is_none());
    assert!(app.clipboard_out.is_none());
}

#[test]
fn osc52_background_session_is_ignored() {
    let (mut app, _sent) = with_active("on");
    app.on_incoming(clipboard("bbbb", "secret"));
    assert!(app.clipboard_out.is_none());
    assert!(app.popup.is_none());
}

#[test]
fn osc52_off_and_on() {
    let (mut app, _sent) = with_active("off");
    app.on_incoming(clipboard("aaaa", "x"));
    assert!(app.clipboard_out.is_none() && app.popup.is_none());
    let (mut app, _sent) = with_active("on");
    app.on_incoming(clipboard("aaaa", "x"));
    assert_eq!(app.clipboard_out.as_deref(), Some("x"));
}

#[test]
fn osc52_ask_does_not_interrupt_other_popup() {
    let (mut app, _sent) = with_active("ask");
    app.on_event(ctrl('a'));
    app.on_event(key('?'));
    assert!(matches!(app.popup, Some(Popup::Help)));
    app.on_incoming(clipboard("aaaa", "x"));
    assert!(
        matches!(app.popup, Some(Popup::Help)),
        "不能蓋掉原本的對話框"
    );
    assert!(app.clipboard_out.is_none());
    assert!(app.message.as_ref().is_some_and(|(_, err, _)| *err));
}

#[test]
fn prefix_commands_switch_sessions_and_open_help() {
    let (mut app, _sent) = with_active("ask");
    assert_eq!(app.active.as_deref(), Some("aaaa"));
    app.on_event(ctrl('a'));
    assert!(app.prefix_armed);
    app.on_event(key('2'));
    assert!(!app.prefix_armed);
    assert_eq!(app.active.as_deref(), Some("bbbb"));
    app.on_event(ctrl('a'));
    app.on_event(key('?'));
    assert!(matches!(app.popup, Some(Popup::Help)));
    app.on_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(app.popup.is_none());
}

#[test]
fn typing_goes_to_active_session_as_input() {
    let (mut app, mut sent) = with_active("ask");
    while sent.try_recv().is_ok() {} // 清掉 Attach / Resize
    app.on_event(key('x'));
    let frame = sent.try_recv().expect("應送出輸入封包");
    assert!(!frame.is_empty());
}

#[test]
fn new_form_opens_and_escape_closes() {
    let (mut app, _sent) = with_active("ask");
    app.on_event(ctrl('a'));
    app.on_event(key('n'));
    assert!(matches!(app.popup, Some(Popup::New(_))));
    app.on_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(app.popup.is_none());
}

// ---- diff 視窗分頁 ----

const DIFF: &str = "### 已 commit（main...HEAD）\ndiff --git a/a b/a\n+one\n\n\n### 未 commit 的變更\n（無）\n\n\n### 未追蹤的檔案\nnew.txt\n";

type Tabs = Vec<(String, Vec<String>)>;

fn text_popup(app: &App) -> (&Vec<String>, usize, &Tabs, usize) {
    match &app.popup {
        Some(Popup::Text {
            lines,
            scroll,
            tabs,
            tab,
            ..
        }) => (lines, *scroll, tabs, *tab),
        _ => panic!("應該有文字視窗"),
    }
}

fn special(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn diff_popup_splits_sections_into_tabs() {
    let (mut app, _s) = test_app("off");
    app.show_text("diff", DIFF, true);
    let (lines, scroll, tabs, tab) = text_popup(&app);
    assert_eq!(tabs.len(), 3);
    assert_eq!(tabs[0].0, "已 commit（main...HEAD）");
    assert_eq!(tabs[1].0, "未 commit 的變更");
    assert_eq!(tabs[2].0, "未追蹤的檔案");
    // 標題行不在內容裡；第一頁是目前頁
    assert!(
        tabs.iter()
            .all(|t| t.1.iter().all(|l| !l.starts_with("###")))
    );
    assert_eq!(tab, 0);
    assert_eq!(scroll, 0);
    assert!(lines.iter().any(|l| l == "+one"));
    assert_eq!(tabs[1].1[0], "（無）");
    assert!(tabs[2].1.iter().any(|l| l == "new.txt"));
}

#[test]
fn plain_text_popup_has_no_tabs() {
    let (mut app, _s) = test_app("off");
    // 錯誤訊息即使含有 ### 也不分頁
    app.show_text("錯誤", "### 看起來像標題\nbody", false);
    let (lines, _, tabs, _) = text_popup(&app);
    assert!(tabs.is_empty());
    assert_eq!(lines.len(), 2);
    // 沒有任何標題的 diff 也不分頁，內容原樣顯示
    app.show_text("diff", "diff --git a b\n+x", true);
    let (lines, _, tabs, _) = text_popup(&app);
    assert!(tabs.is_empty());
    assert_eq!(lines.len(), 2);
    // 只有一頁時，切換鍵不動作也不當機
    app.on_event(special(KeyCode::Tab));
    assert!(app.popup.is_some());
}

#[test]
fn diff_popup_keys_switch_tabs_and_reset_scroll() {
    let (mut app, _s) = test_app("off");
    app.show_text("diff", DIFF, true);
    // 先往下捲，切換頁後捲動歸零
    app.on_event(key('j'));
    assert_eq!(text_popup(&app).1, 1);
    app.on_event(special(KeyCode::Tab));
    let (lines, scroll, _, tab) = text_popup(&app);
    assert_eq!((tab, scroll), (1, 0));
    assert_eq!(lines[0], "（無）");
    // → / l 下一頁，到最後一頁再往後會繞回第一頁
    app.on_event(special(KeyCode::Right));
    assert_eq!(text_popup(&app).3, 2);
    app.on_event(key('l'));
    assert_eq!(text_popup(&app).3, 0);
    // ← / h / BackTab 上一頁（第一頁往前繞到最後一頁）
    app.on_event(special(KeyCode::Left));
    assert_eq!(text_popup(&app).3, 2);
    app.on_event(key('h'));
    assert_eq!(text_popup(&app).3, 1);
    app.on_event(special(KeyCode::BackTab));
    assert_eq!(text_popup(&app).3, 0);
    // 數字鍵直接跳；超出頁數的數字不動作
    app.on_event(key('3'));
    let (lines, _, _, tab) = text_popup(&app);
    assert_eq!(tab, 2);
    assert!(lines.iter().any(|l| l == "new.txt"));
    app.on_event(key('9'));
    assert_eq!(text_popup(&app).3, 2);
    app.on_event(key('1'));
    assert_eq!(text_popup(&app).3, 0);
    // q 關閉
    app.on_event(key('q'));
    assert!(app.popup.is_none());
}

#[test]
fn diff_popup_scroll_is_limited_to_current_tab() {
    let (mut app, _s) = test_app("off");
    app.show_text("diff", DIFF, true);
    app.on_event(key('2'));
    // 「未 commit」頁只有幾行：一直往下捲不會超出該頁的最後一行
    for _ in 0..50 {
        app.on_event(key('j'));
    }
    let (lines, scroll, _, _) = text_popup(&app);
    assert_eq!(scroll, lines.len() - 1);
    // 複製的是目前這頁的內容
    app.on_event(key('c'));
    let copied = app.clipboard_out.clone().unwrap();
    assert!(
        copied.contains("（無）") && !copied.contains("+one"),
        "{copied}"
    );
}

// ---- 範本與標籤 ----

fn tpl(name: &str, agent: &str, task: &str, base: Option<&str>) -> agt_proto::TemplateInfo {
    agt_proto::TemplateInfo {
        name: name.into(),
        description: String::new(),
        agent: agent.into(),
        task: task.into(),
        base: base.map(Into::into),
        tags: vec![],
    }
}

fn app_with_templates() -> App {
    let (client, _sent) = Client::detached();
    let (tx, _rx) = mpsc::unbounded_channel();
    let ui = UiConfig {
        templates: vec![
            tpl("fix", "shell", "修正：", Some("develop")),
            tpl("review", "claude", "審查", None),
        ],
        ..UiConfig::default()
    };
    App::new(client, ui, None, ".".into(), tx)
}

fn f3() -> Event {
    Event::Key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE))
}

fn new_form(app: &App) -> &NewForm {
    match &app.popup {
        Some(Popup::New(f)) => f,
        _ => panic!("新增表單沒有開啟"),
    }
}

#[test]
fn template_cycling_fills_the_form_and_wraps_around() {
    let mut app = app_with_templates();
    app.on_event(ctrl('a'));
    app.on_event(key('n'));
    assert_eq!(new_form(&app).template_name(), None);

    app.on_event(f3());
    let f = new_form(&app);
    assert_eq!(f.template_name(), Some("fix"));
    assert_eq!(f.agents[f.agent], "shell");
    assert_eq!((f.task.as_str(), f.base.as_str()), ("修正：", "develop"));

    // 第二個範本沒有 base：不會清掉前一個範本填的 base
    app.on_event(f3());
    let f = new_form(&app);
    assert_eq!(f.template_name(), Some("review"));
    assert_eq!(f.agents[f.agent], "claude");
    assert_eq!((f.task.as_str(), f.base.as_str()), ("審查", "develop"));

    // 最後一個之後回到「不套用」，已填的欄位保留
    app.on_event(f3());
    let f = new_form(&app);
    assert_eq!(f.template_name(), None);
    assert_eq!(f.task, "審查");

    // Ctrl+T 同樣可以切換
    app.on_event(ctrl('t'));
    assert_eq!(new_form(&app).template_name(), Some("fix"));
}

#[test]
fn template_key_is_a_noop_without_templates() {
    let (mut app, _sent) = with_active("ask");
    app.on_event(ctrl('a'));
    app.on_event(key('n'));
    app.on_event(f3());
    assert_eq!(new_form(&app).template_name(), None);
    assert!(new_form(&app).task.is_empty());
}

#[test]
fn template_with_unknown_agent_keeps_current_agent() {
    let mut f = NewForm {
        agents: vec!["claude".into(), "shell".into()],
        agent: 1,
        repo: String::new(),
        task: String::new(),
        base: String::new(),
        field: 2,
        browser: None,
        templates: vec![tpl("x", "gone", "t", None)],
        template: None,
    };
    f.next_template();
    assert_eq!(f.agent, 1);
    assert_eq!(f.task, "t");
}

#[test]
fn tags_popup_starts_with_current_tags_and_escape_cancels() {
    let (mut app, _sent) = test_app("ask");
    let mut s = session("aaaa");
    s.tags = vec!["bug".into(), "ui".into()];
    app.upsert(s);
    app.select(Some("aaaa".into()));
    app.on_event(ctrl('a'));
    app.on_event(key('t'));
    match &app.popup {
        Some(Popup::Tags { input }) => assert_eq!(input, "bug ui"),
        _ => panic!("標籤視窗沒有開啟"),
    }
    app.on_event(key('x'));
    match &app.popup {
        Some(Popup::Tags { input }) => assert_eq!(input, "bug uix"),
        _ => panic!(),
    }
    app.on_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(app.popup.is_none());
}

#[test]
fn tags_popup_needs_an_active_session() {
    let (mut app, _sent) = test_app("ask");
    app.on_event(ctrl('a'));
    app.on_event(key('t'));
    assert!(app.popup.is_none());
}

#[tokio::test]
async fn tags_popup_enter_sends_set_tags_and_closes() {
    let (mut app, mut sent) = with_active("ask");
    app.on_event(ctrl('a'));
    app.on_event(key('t'));
    app.on_event(key('#'));
    app.on_event(key('a'));
    app.on_event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    assert!(app.popup.is_none());
    // 跳過 with_active 先送出的 Attach / Resize，找到 SetTags
    let text = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let frame = sent.recv().await.expect("通道已關閉");
            let text = String::from_utf8_lossy(&frame).into_owned();
            if text.contains("SetTags") {
                return text;
            }
        }
    })
    .await
    .expect("應送出 SetTags 請求");
    assert!(text.contains("\"a\""), "{text}");
}

#[test]
fn status_bar_usage_shows_active_context_and_latest_quota() {
    use agt_proto::usage::{Quota, Usage};
    let (mut app, _sent) = test_app("on");
    let now = 1_000_000;
    let text = |app: &App| {
        crate::ui::usage_spans(app, ratatui::style::Style::new(), now)
            .iter()
            .map(|s| s.content.to_string())
            .collect::<String>()
    };
    let mut a = session("aaaa1111");
    a.usage = Some(Usage {
        context_percent: Some(85.4),
        five_hour: Some(Quota {
            used_percent: 10.0,
            resets_at: now + 3600,
        }),
        updated_at: 1,
        ..Default::default()
    });
    let mut b = session("bbbb2222");
    b.usage = Some(Usage {
        context_percent: Some(12.0),
        five_hour: Some(Quota {
            used_percent: 23.5,
            resets_at: now + 2 * 3600 + 15 * 60,
        }),
        seven_day: Some(Quota {
            used_percent: 41.0,
            resets_at: now - 1, // 已經重置：不顯示
        }),
        updated_at: 2, // 比 a 新：額度用這份
        ..Default::default()
    });
    app.sessions = vec![a, b];
    // 沒有選分頁：只有額度
    assert_eq!(text(&app), "5h 24% 2h15m");
    app.active = Some("aaaa1111".into());
    assert_eq!(text(&app), "ctx 85% · 5h 24% 2h15m");
    // 達到 80% 用紅色
    let spans = crate::ui::usage_spans(&app, ratatui::style::Style::new(), now);
    assert_eq!(spans[0].style.fg, Some(crate::theme::theme().red));
    assert_ne!(spans[2].style.fg, Some(crate::theme::theme().red));
    // 沒有任何用量
    app.sessions.iter_mut().for_each(|s| s.usage = None);
    assert_eq!(text(&app), "");
}

fn switch_app() -> App {
    let (mut app, _sent) = test_app("ask");
    let mut a = session("aaaa");
    a.agent = "claude".into();
    a.task = "登入功能".into();
    a.repo_name = Some("web".into());
    a.tags = vec!["urgent".into()];
    let mut b = session("bbbb");
    b.task = "Fix Parser".into();
    b.repo_name = Some("core".into());
    let mut c = session("cccc");
    c.task = "文件".into();
    c.repo_name = Some("web".into());
    app.upsert(a);
    app.upsert(b);
    app.upsert(c);
    app.select(Some("aaaa".into()));
    app
}

fn typed(app: &mut App, s: &str) {
    for c in s.chars() {
        app.on_event(key(c));
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.on_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn ids(app: &App, q: &str) -> Vec<String> {
    app.switch_matches(q).iter().map(|s| s.id.clone()).collect()
}

#[test]
fn switch_matches_all_terms_case_insensitively() {
    let app = switch_app();
    assert_eq!(ids(&app, "").len(), 3);
    assert_eq!(ids(&app, "PARSER"), ["bbbb"]);
    assert_eq!(ids(&app, "web"), ["aaaa", "cccc"]);
    assert_eq!(ids(&app, "web urgent"), ["aaaa"]);
    assert_eq!(ids(&app, "claude 登入"), ["aaaa"]);
    assert!(ids(&app, "nope").is_empty());
}

#[test]
fn switch_popup_filters_and_enter_jumps() {
    let mut app = switch_app();
    app.on_event(ctrl('a'));
    app.on_event(key(' '));
    assert!(matches!(app.popup, Some(Popup::Switch { .. })));
    typed(&mut app, "parser");
    press(&mut app, KeyCode::Enter);
    assert!(app.popup.is_none());
    assert_eq!(app.active.as_deref(), Some("bbbb"));
}

#[test]
fn switch_popup_arrows_wrap_and_reset_on_typing() {
    let mut app = switch_app();
    app.on_event(ctrl('a'));
    app.on_event(key('p'));
    press(&mut app, KeyCode::Up); // 從第一個往上：回到最後一個
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.active.as_deref(), Some("cccc"));

    app.on_event(ctrl('a'));
    app.on_event(key('p'));
    press(&mut app, KeyCode::Down);
    typed(&mut app, "web"); // 輸入後選擇回到第一個
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.active.as_deref(), Some("aaaa"));
}

#[test]
fn switch_popup_enter_without_match_stays_open_and_escape_cancels() {
    let mut app = switch_app();
    app.on_event(ctrl('a'));
    app.on_event(key(' '));
    typed(&mut app, "zzz");
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.popup, Some(Popup::Switch { .. })));
    assert_eq!(app.active.as_deref(), Some("aaaa"));
    press(&mut app, KeyCode::Esc);
    assert!(app.popup.is_none());
}

/// `s`：對有 worktree 的分頁送出 Sync；沒有 worktree 的分頁只顯示提示，不送請求。
#[tokio::test]
async fn sync_key_sends_request_only_for_worktree_sessions() {
    let (mut app, mut sent) = test_app("ask");
    let mut plain = session("aaaa");
    plain.worktree = None;
    app.upsert(plain);
    app.select(Some("aaaa".into()));
    while sent.try_recv().is_ok() {}
    app.on_event(ctrl('a'));
    app.on_event(key('s'));
    assert!(sent.try_recv().is_err(), "沒有 worktree 不該送請求");

    let mut wt = session("bbbb");
    wt.worktree = Some("/w/b".into());
    app.upsert(wt);
    app.select(Some("bbbb".into()));
    while sent.try_recv().is_ok() {}
    app.on_event(ctrl('a'));
    app.on_event(key('s'));
    // 請求在背景工作執行，這裡只確認有進入「同步中」
    assert!(app.message.as_ref().is_some_and(|m| m.0.contains("同步中")));
}
