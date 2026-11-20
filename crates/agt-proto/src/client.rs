//! TUI、CLI 與未來 GUI 共用的 daemon client。

use crate::codec::{encode_frame, input_frame};
use crate::transport;
use crate::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("無法連線到 daemon：{0}")]
    Connect(#[from] std::io::Error),
    #[error("與 daemon 的連線已中斷")]
    Closed,
    #[error("{message}")]
    Remote { code: String, message: String },
    #[error("回應格式不符預期")]
    Unexpected,
}

impl ClientError {
    pub fn code(&self) -> Option<&str> {
        match self {
            ClientError::Remote { code, .. } => Some(code),
            _ => None,
        }
    }
}

/// 推播給 client 的事件。
#[derive(Debug)]
pub enum Incoming {
    Msg(Box<ServerMsg>),
    Screen(ScreenUpdate),
    Closed,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<ReplyBody>>>>;

#[derive(Clone)]
pub struct Client {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    next: Arc<AtomicU64>,
}

pub struct Connected {
    pub client: Client,
    pub events: mpsc::UnboundedReceiver<Incoming>,
    pub hello: ReplyBody,
}

impl Client {
    /// 連線並送出 `Hello`。版本不同時回傳 `proto_mismatch` 錯誤。
    pub async fn connect(ep: &str, client_kind: &str) -> Result<Connected, ClientError> {
        let stream = transport::connect(ep).await?;
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();

        let p2 = pending.clone();
        // client 不送畫面：sender 立刻丟掉，drive 會把這個佇列視為已關閉
        let (_, no_screens) = mpsc::channel::<Vec<u8>>(1);
        tokio::spawn(async move {
            crate::codec::drive(&stream, rx, no_screens, |kind, payload| {
                match kind {
                    KIND_JSON => {
                        let Ok(msg) = serde_json::from_slice::<ServerMsg>(&payload) else {
                            return true;
                        };
                        if let ServerMsg::Reply { req_id, result } = msg {
                            if let Some(tx) = p2.lock().unwrap().remove(&req_id) {
                                let _ = tx.send(result);
                            }
                        } else {
                            // 沒有人在聽事件也沒關係，回應仍要繼續處理。
                            let _ = ev_tx.send(Incoming::Msg(Box::new(msg)));
                        }
                    }
                    KIND_SCREEN => {
                        if let Ok(u) = ScreenUpdate::decode(&payload) {
                            let _ = ev_tx.send(Incoming::Screen(u));
                        }
                    }
                    _ => {}
                }
                true
            })
            .await;
            p2.lock().unwrap().clear();
            let _ = ev_tx.send(Incoming::Closed);
        });

        let client = Client {
            tx,
            pending,
            next: Arc::new(AtomicU64::new(1)),
        };
        let hello = client
            .request(ClientMsg::Hello {
                proto_version: PROTO_VERSION,
                client_kind: client_kind.into(),
            })
            .await?;
        Ok(Connected {
            client,
            events: ev_rx,
            hello,
        })
    }

    /// 不連線的 client（測試用）：送出的封包放進回傳的佇列，`request` 永遠等不到回應。
    /// 讓 TUI 的 `App` 不需要 daemon 就能建立並測試。
    pub fn detached() -> (Client, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = Client {
            tx,
            pending: Arc::default(),
            next: Arc::new(AtomicU64::new(1)),
        };
        (client, rx)
    }

    fn send_json(&self, req_id: u64, msg: ClientMsg) -> Result<(), ClientError> {
        let body =
            serde_json::to_vec(&Request { req_id, msg }).map_err(|_| ClientError::Unexpected)?;
        let frame = encode_frame(KIND_JSON, &body)?;
        self.tx.send(frame).map_err(|_| ClientError::Closed)
    }

    /// 送出請求並等待回應。daemon 回傳 `Error` 時轉成 `ClientError::Remote`。
    pub async fn request(&self, msg: ClientMsg) -> Result<ReplyBody, ClientError> {
        let req_id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(req_id, tx);
        self.send_json(req_id, msg)?;
        match rx.await {
            Ok(ReplyBody::Error { code, message }) => Err(ClientError::Remote { code, message }),
            Ok(r) => Ok(r),
            Err(_) => Err(ClientError::Closed),
        }
    }

    /// 帶逾時的請求。
    pub async fn request_timeout(
        &self,
        msg: ClientMsg,
        t: Duration,
    ) -> Result<ReplyBody, ClientError> {
        match tokio::time::timeout(t, self.request(msg)).await {
            Ok(r) => r,
            Err(_) => Err(ClientError::Closed),
        }
    }

    /// 送出請求但不等待回應。
    pub fn send(&self, msg: ClientMsg) -> Result<(), ClientError> {
        let req_id = self.next.fetch_add(1, Ordering::Relaxed);
        self.send_json(req_id, msg)
    }

    /// 送出使用者輸入（kind 0x03）。
    pub fn input(&self, id: &str, bytes: &[u8]) -> Result<(), ClientError> {
        let frame = input_frame(id, bytes)?;
        self.tx.send(frame).map_err(|_| ClientError::Closed)
    }
}

/// 從回應中取出 `SessionInfo`。
pub fn expect_session(r: ReplyBody) -> Result<SessionInfo, ClientError> {
    match r {
        ReplyBody::Session(s) => Ok(s),
        _ => Err(ClientError::Unexpected),
    }
}

pub fn expect_sessions(r: ReplyBody) -> Result<Vec<SessionInfo>, ClientError> {
    match r {
        ReplyBody::Sessions(s) => Ok(s),
        _ => Err(ClientError::Unexpected),
    }
}
