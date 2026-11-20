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
        shared: false,
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
