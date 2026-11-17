//! agt 的 TUI。只依賴 `agt-proto`，透過協定和 daemon 溝通；未來的 GUI 也依照同樣方式接入。

mod app;
mod dirs;
mod keys;
mod ui;

pub use keys::{Prefix, encode_key};

use agt_proto::client::{Connected, expect_sessions};
use agt_proto::{ClientMsg, ReplyBody};
use app::App;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    EventStream,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::time::{Duration, Instant};

pub struct Options {
    /// 要聚焦的分頁（id、短 id 或前綴）。
    pub focus: Option<String>,
    /// 新增分頁時預設的 repo 路徑。
    pub cwd: String,
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
}

/// 執行 TUI 直到使用者 detach（`q`）或 daemon 中斷。
pub async fn run(conn: Connected, opts: Options) -> io::Result<Option<String>> {
    let Connected {
        client,
        mut events,
        hello,
    } = conn;
    let (ui, config_error) = match hello {
        ReplyBody::Hello {
            ui, config_error, ..
        } => (ui, config_error),
        _ => (Default::default(), None),
    };
    let sessions = client
        .request(ClientMsg::ListSessions)
        .await
        .and_then(expect_sessions)
        .map_err(io::Error::other)?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(client, ui, config_error, opts.cwd, tx);
    for s in sessions {
        app.upsert(s);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        prev_hook(info);
    }));
    let mut term = Terminal::new(CrosstermBackend::new(stdout))?;
    term.clear()?;

    // 先畫一次以取得主區域大小，再 attach
    term.draw(|f| ui::draw(f, &mut app))?;
    let focus = match &opts.focus {
        Some(f) => app
            .sessions
            .iter()
            .find(|s| {
                s.id == *f || s.short_id == *f || (f.len() >= 3 && s.id.starts_with(f.as_str()))
            })
            .map(|s| s.id.clone()),
        None => None,
    }
    .or_else(|| app.ordered().first().map(|s| s.id.clone()));
    app.select(focus);

    let mut input = EventStream::new();
    let mut frame = tokio::time::interval(Duration::from_millis(16));
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut slow = tokio::time::interval(Duration::from_millis(500));
    let mut last_draw = Instant::now() - Duration::from_secs(1);

    let result: io::Result<()> = loop {
        tokio::select! {
            ev = input.next() => match ev {
                Some(Ok(ev)) => app.on_event(ev),
                Some(Err(e)) => break Err(e),
                None => break Ok(()),
            },
            inc = events.recv() => match inc {
                Some(inc) => app.on_incoming(inc),
                None => { app.quit = true; }
            },
            Some(ev) = rx.recv() => app.on_app_event(ev),
            _ = frame.tick() => {
                // 大量輸出時合併幀：最多 60 fps
                if app.dirty && last_draw.elapsed() >= Duration::from_millis(15) {
                    app.dirty = false;
                    last_draw = Instant::now();
                    if let Err(e) = term.draw(|f| ui::draw(f, &mut app)) {
                        break Err(e);
                    }
                }
            }
            _ = slow.tick() => app.tick(),
        }
        if let Some(text) = app.clipboard_out.take() {
            use std::io::Write;
            let w = term.backend_mut();
            let _ = w.write_all(keys::osc52(&text).as_bytes());
            let _ = w.flush();
        }
        if app.quit {
            break Ok(());
        }
    };

    if let Some(id) = app.active.clone() {
        let _ = app.client.send(ClientMsg::Detach { id });
    }
    restore();
    let _ = std::panic::take_hook();
    result?;
    Ok(app
        .message
        .map(|(m, err, _)| if err { m } else { String::new() })
        .filter(|m| !m.is_empty()))
}
