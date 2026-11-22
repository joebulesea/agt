//! IPC Router：接受連線、檢查協定版本、分派請求、對訂閱者推播事件。

use crate::state::{Daemon, OpError, OpResult};
use agt_proto::codec::{self, decode_input, encode_frame};
use agt_proto::transport::Conn;
use agt_proto::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

type Out = mpsc::UnboundedSender<Vec<u8>>;

fn json_frame(msg: &ServerMsg) -> Vec<u8> {
    let body = serde_json::to_vec(msg).expect("ServerMsg 序列化不會失敗");
    encode_frame(KIND_JSON, &body).unwrap_or_default()
}

fn screen_frame(u: &ScreenUpdate) -> Option<Vec<u8>> {
    encode_frame(KIND_SCREEN, &u.encode()).ok()
}

fn reply(out: &Out, req_id: u64, result: OpResult<ReplyBody>) {
    let result = match result {
        Ok(r) => r,
        Err(OpError { code, message }) => ReplyBody::Error {
            code: code.into(),
            message,
        },
    };
    let body =
        serde_json::to_vec(&ServerMsg::Reply { req_id, result }).expect("ServerMsg 序列化不會失敗");
    // 超過封包上限（例如巨大的 diff）時改回錯誤，否則 client 會一直等不到回應
    let frame = encode_frame(KIND_JSON, &body).unwrap_or_else(|_| {
        json_frame(&ServerMsg::Reply {
            req_id,
            result: ReplyBody::Error {
                code: codes::INTERNAL.into(),
                message: format!("回應太大（{} bytes），超過 16 MiB 的封包上限", body.len()),
            },
        })
    });
    let _ = out.send(frame);
}

/// 連線後必須在這段時間內送出 Hello，否則中斷連線。
const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 每條連線最多排隊的畫面封包數；超過時畫面轉送 task 會等待、落後，之後改送完整快照。
const SCREEN_QUEUE: usize = 32;

struct ConnState {
    /// 畫面封包（有上限）；JSON 回應與事件走 `out`（無上限，量小）。
    screens: mpsc::Sender<Vec<u8>>,
    attached: HashMap<String, JoinHandle<()>>,
    /// 這個 client 對各分頁要求的大小；有輸入時套用（最後一個有輸入的 client 決定 PTY 大小）。
    sizes: HashMap<String, (u16, u16)>,
    is_ui: bool,
    events_task: Option<JoinHandle<()>>,
}

pub async fn serve_conn(d: Arc<Daemon>, stream: Conn) {
    let (out, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (screens, screens_rx) = mpsc::channel::<Vec<u8>>(SCREEN_QUEUE);
    let mut st = ConnState {
        screens,
        attached: HashMap::new(),
        sizes: HashMap::new(),
        is_ui: false,
        events_task: None,
    };
    // 是否已完成 Hello；逾時檢查（另一個 future）也要讀，所以用 atomic
    let hello = AtomicBool::new(false);
    let serve = codec::drive(&stream, out_rx, screens_rx, |kind, payload| {
        let hello_done = hello.load(Ordering::Relaxed);
        match (kind, payload) {
            (KIND_JSON, payload) => {
                let req: Request = match serde_json::from_slice(&payload) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = out.send(json_frame(&ServerMsg::Error {
                            code: codes::BAD_REQUEST.into(),
                            message: format!("無法解析請求：{e}"),
                        }));
                        return true;
                    }
                };
                if !hello_done {
                    match req.msg {
                        ClientMsg::Hello {
                            proto_version,
                            ref client_kind,
                        } => {
                            if proto_version != PROTO_VERSION {
                                reply(
                                    &out,
                                    req.req_id,
                                    Err(OpError::new(
                                        codes::PROTO_MISMATCH,
                                        format!(
                                            "協定版本不同（client {proto_version}、daemon {PROTO_VERSION}），請執行 `agt daemon restart`"
                                        ),
                                    )),
                                );
                                return false;
                            }
                            hello.store(true, Ordering::Relaxed);
                            if client_kind != "notify" && client_kind != "cli" {
                                st.is_ui = true;
                                d.ui_clients.fetch_add(1, Ordering::Relaxed);
                                st.events_task =
                                    Some(forward_events(d.events.subscribe(), out.clone()));
                            }
                            d.touch();
                            let cfg = d.cfg();
                            reply(
                                &out,
                                req.req_id,
                                Ok(ReplyBody::Hello {
                                    daemon_version: env!("CARGO_PKG_VERSION").into(),
                                    ui: cfg.ui(),
                                    config_error: d.cfg_error(),
                                }),
                            );
                        }
                        _ => {
                            reply(
                                &out,
                                req.req_id,
                                Err(OpError::new(
                                    codes::BAD_REQUEST,
                                    "連線後的第一個訊息必須是 Hello",
                                )),
                            );
                            return false;
                        }
                    }
                    return true;
                }
                tracing::debug!(req_id = req.req_id, msg = ?std::mem::discriminant(&req.msg), "請求");
                handle(&d, &out, &mut st, req);
            }
            (KIND_INPUT, payload) if hello_done => {
                if let Some((id, bytes)) = decode_input(&payload) {
                    input(&d, &st, &id, bytes);
                }
            }
            _ => {}
        }
        true
    });
    // 連線後遲遲不送 Hello 的 client 不讓它一直佔著連線
    let no_hello = async {
        tokio::time::sleep(HELLO_TIMEOUT).await;
        if hello.load(Ordering::Relaxed) {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        _ = serve => {}
        _ = no_hello => tracing::debug!("連線逾時未送出 Hello，中斷"),
    }

    for (_, t) in st.attached.drain() {
        t.abort();
    }
    if let Some(t) = st.events_task.take() {
        t.abort();
    }
    if st.is_ui {
        d.ui_clients.fetch_sub(1, Ordering::Relaxed);
    }
    d.touch();
}

fn forward_events(mut rx: broadcast::Receiver<ServerMsg>, out: Out) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(m) => {
                    if out.send(json_frame(&m)).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    })
}

fn input(d: &Daemon, st: &ConnState, id: &str, bytes: &[u8]) {
    let Ok(id) = d.resolve(id) else { return };
    if let Some(s) = d.session(&id) {
        if let Some(&(c, r)) = st.sizes.get(&id)
            && s.size() != (c, r)
        {
            s.resize(c, r);
        }
        s.write(bytes);
    }
}

/// 已結束（daemon 重啟前）的工作階段沒有 PTY，送一個說明畫面。
fn placeholder(info: &SessionInfo, cols: u16, rows: u16) -> Screen {
    let mut lines: Vec<Row> = vec![vec![Cell::default(); cols as usize]; rows as usize];
    let msgs = [
        format!("[agt] {} 已結束（daemon 重新啟動前的工作階段）", info.task),
        "按 Ctrl+A R 在同一個 worktree 重新啟動 Agent，或 Ctrl+A x 關閉分頁。".to_string(),
    ];
    for (y, m) in msgs.iter().enumerate() {
        if let Some(line) = lines.get_mut(y + 1) {
            let mut x = 1usize;
            for ch in m.chars() {
                let w = if ch.is_ascii() { 1 } else { 2 };
                if x + w > cols as usize {
                    break;
                }
                line[x] = Cell {
                    ch,
                    flags: if w == 2 { screen::flags::WIDE } else { 0 } | screen::flags::DIM,
                    ..Default::default()
                };
                if w == 2 {
                    line[x + 1].flags = screen::flags::SPACER;
                }
                x += w;
            }
        }
    }
    Screen {
        cols,
        rows,
        lines,
        cursor: Cursor::default(),
        ..Default::default()
    }
}

fn attach(d: &Arc<Daemon>, out: &Out, st: &mut ConnState, id: &str) -> OpResult<ReplyBody> {
    let id = d.resolve(id)?;
    if let Some(t) = st.attached.remove(&id) {
        t.abort();
    }
    let Some(session) = d.session(&id) else {
        let info = d.get(&id)?;
        let (c, r) = st.sizes.get(&id).copied().unwrap_or((80, 24));
        if let Some(f) = screen_frame(&ScreenUpdate::Snapshot {
            id: id.clone(),
            screen: placeholder(&info, c, r),
        }) {
            let _ = out.send(f);
        }
        return Ok(ReplyBody::Ok);
    };
    if let Some(&(c, r)) = st.sizes.get(&id) {
        session.resize(c, r);
    }
    let (mut rx, screen) = session.attach();
    let screens = st.screens.clone();
    let id2 = id.clone();
    let task = tokio::spawn(async move {
        // 快照與之後的差異走同一個有上限的佇列，順序不會錯；client 讀太慢時這裡會等待，
        // broadcast 因此落後（Lagged），再改送一份新的完整快照。
        if let Some(f) = screen_frame(&ScreenUpdate::Snapshot {
            id: id2.clone(),
            screen,
        }) && screens.send(f).await.is_err()
        {
            return;
        }
        loop {
            match rx.recv().await {
                Ok(u) => {
                    if let Some(f) = screen_frame(&u)
                        && screens.send(f).await.is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // 落後太多：重新送完整快照
                    let screen = session.snapshot();
                    if let Some(f) = screen_frame(&ScreenUpdate::Snapshot {
                        id: id2.clone(),
                        screen,
                    }) && screens.send(f).await.is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    st.attached.insert(id, task);
    Ok(ReplyBody::Ok)
}

fn handle(d: &Arc<Daemon>, out: &Out, st: &mut ConnState, req: Request) {
    let req_id = req.req_id;
    // 可能較慢的操作（git、行程）放到 blocking 執行緒，不卡住這條連線的輸入。
    macro_rules! blocking {
        ($body:expr) => {{
            let d = d.clone();
            let out = out.clone();
            tokio::task::spawn_blocking(move || {
                let d = &d;
                let r: OpResult<ReplyBody> = $body(d);
                reply(&out, req_id, r);
            });
        }};
    }
    match req.msg {
        ClientMsg::Hello { .. } => reply(out, req_id, Ok(ReplyBody::Ok)),
        ClientMsg::CreateSession(c) => {
            blocking!(|d: &Arc<Daemon>| d.create(c).map(|s| ReplyBody::Session(Box::new(s))))
        }
        ClientMsg::ListSessions => reply(out, req_id, Ok(ReplyBody::Sessions(d.list()))),
        ClientMsg::GetSession { id } => reply(
            out,
            req_id,
            d.get(&id).map(|s| ReplyBody::Session(Box::new(s))),
        ),
        ClientMsg::Attach { id } => {
            let r = attach(d, out, st, &id);
            reply(out, req_id, r);
        }
        ClientMsg::Detach { id } => {
            if let Ok(id) = d.resolve(&id)
                && let Some(t) = st.attached.remove(&id)
            {
                t.abort();
            }
            reply(out, req_id, Ok(ReplyBody::Ok));
        }
        ClientMsg::Input { id, bytes } => {
            input(d, st, &id, &bytes);
            reply(out, req_id, Ok(ReplyBody::Ok));
        }
        ClientMsg::Resize { id, cols, rows } => {
            // 已結束分頁的說明畫面也用這個大小配置，所以在這裡就限制
            let (cols, rows) = (cols.clamp(2, MAX_COLS), rows.clamp(1, MAX_ROWS));
            let r = d.resolve(&id).map(|id| {
                st.sizes.insert(id.clone(), (cols, rows));
                if let Some(s) = d.session(&id) {
                    s.resize(cols, rows);
                }
                ReplyBody::Ok
            });
            reply(out, req_id, r);
        }
        ClientMsg::Kill { id, cleanup, force } => {
            blocking!(|d: &Arc<Daemon>| d.kill(&id, cleanup, force).map(|_| ReplyBody::Ok))
        }
        ClientMsg::Notify {
            session_id,
            status,
            message,
            tool,
        } => reply(
            out,
            req_id,
            d.notify(&session_id, status, message, tool)
                .map(|_| ReplyBody::Ok),
        ),
        ClientMsg::Finish { id, action, force } => {
            blocking!(|d: &Arc<Daemon>| d.finish(&id, action, force).map(ReplyBody::Text))
        }
        ClientMsg::FinishPreview { id, action } => {
            blocking!(|d: &Arc<Daemon>| d.finish_preview(&id, &action).map(ReplyBody::Text))
        }
        ClientMsg::Diff { id } => blocking!(|d: &Arc<Daemon>| d.diff(&id).map(ReplyBody::Text)),
        ClientMsg::Sync { id } => {
            blocking!(|d: &Arc<Daemon>| d.sync_base(&id).map(ReplyBody::Text))
        }
        ClientMsg::Rename { id, task } => reply(
            out,
            req_id,
            d.rename(&id, &task)
                .map(|s| ReplyBody::Session(Box::new(s))),
        ),
        ClientMsg::SetTags { id, tags } => reply(
            out,
            req_id,
            d.set_tags(&id, &tags)
                .map(|s| ReplyBody::Session(Box::new(s))),
        ),
        ClientMsg::Scroll { id, delta } => {
            let r = d.resolve(&id).map(|id| {
                if let Some(s) = d.session(&id) {
                    s.scroll(delta);
                }
                ReplyBody::Ok
            });
            reply(out, req_id, r);
        }
        ClientMsg::Search { id, query } => {
            let r = d
                .resolve(&id)
                .map(|id| ReplyBody::Found(d.session(&id).is_some_and(|s| s.search(&query))));
            reply(out, req_id, r);
        }
        ClientMsg::Restart { id, fresh } => {
            blocking!(|d: &Arc<Daemon>| d
                .restart(&id, fresh)
                .map(|s| ReplyBody::Session(Box::new(s))))
        }
        ClientMsg::Gc { prune } => blocking!(|d: &Arc<Daemon>| d.gc(prune).map(ReplyBody::Gc)),
        ClientMsg::DaemonStatus => reply(out, req_id, Ok(ReplyBody::Daemon(d.info()))),
        ClientMsg::Shutdown { kill_agents } => {
            let _ = d.events.send(ServerMsg::DaemonStopping);
            if kill_agents {
                d.kill_all();
            }
            tracing::debug!("Shutdown：Agent 已結束，回覆 client");
            reply(out, req_id, Ok(ReplyBody::Ok));
            d.shutdown.notify_one();
        }
    }
}
