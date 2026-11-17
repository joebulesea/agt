use crate::types::*;
use serde::{Deserialize, Serialize};

/// 建立工作階段的參數。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSession {
    pub agent: String,
    /// repo（或任意目錄）路徑；相對路徑由 client 先轉成絕對路徑。
    pub repo: String,
    pub task: String,
    #[serde(default)]
    pub base_branch: Option<String>,
    /// 與既有工作階段共用 worktree（`--share <session>`）。
    #[serde(default)]
    pub share: Option<String>,
    /// 使用者已確認可以執行 repo 的 setup 指令（規格 11.4）。
    #[serde(default)]
    pub trust_setup: bool,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

/// Client → Daemon 的控制訊息（規格 4.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    Hello {
        proto_version: u32,
        client_kind: String,
    },
    CreateSession(CreateSession),
    ListSessions,
    GetSession {
        id: String,
    },
    Attach {
        id: String,
    },
    Detach {
        id: String,
    },
    /// JSON 形式的輸入；一般輸入走 kind 0x03 二進位封包。
    Input {
        id: String,
        bytes: Vec<u8>,
    },
    Resize {
        id: String,
        cols: u16,
        rows: u16,
    },
    Kill {
        id: String,
        #[serde(default)]
        cleanup: bool,
        #[serde(default)]
        force: bool,
    },
    Notify {
        session_id: String,
        status: AgentStatus,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        tool: Option<String>,
    },
    Finish {
        id: String,
        #[serde(flatten)]
        action: FinishAction,
        /// 已再次確認（discard 有未 commit / 未 push 的變更時需要）。
        #[serde(default)]
        force: bool,
    },
    Diff {
        id: String,
    },
    Rename {
        id: String,
        task: String,
    },
    /// 滾動歷史；正數往上（較舊），`i32::MIN` 代表回到底部。
    Scroll {
        id: String,
        delta: i32,
    },
    /// 從目前位置往上搜尋歷史。
    Search {
        id: String,
        query: String,
    },
    /// 在同一個 worktree 重新啟動已結束的 Agent；支援時接續上次的對話。
    Restart {
        id: String,
        /// true = 不接續，開新對話。
        #[serde(default)]
        fresh: bool,
    },
    Gc {
        #[serde(default)]
        prune: bool,
    },
    Shutdown {
        #[serde(default)]
        kill_agents: bool,
    },
    DaemonStatus,
}

/// 請求：每個請求帶 `req_id`，回應用同一個 `req_id` 對應。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub req_id: u64,
    #[serde(flatten)]
    pub msg: ClientMsg,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub version: String,
    pub sessions: usize,
    pub running: usize,
    pub clients: usize,
    pub data_dir: String,
    pub config_path: String,
}

/// 回應內容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum ReplyBody {
    Ok,
    Hello {
        daemon_version: String,
        ui: UiConfig,
        #[serde(default)]
        config_error: Option<String>,
    },
    Session(SessionInfo),
    Sessions(Vec<SessionInfo>),
    Text(String),
    Found(bool),
    Gc(Vec<GcItem>),
    Daemon(DaemonInfo),
    Error {
        code: String,
        message: String,
    },
}

/// Daemon → Client 的 JSON 訊息：回應或推播事件（推播沒有 `req_id`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    Reply {
        req_id: u64,
        result: ReplyBody,
    },
    StatusChanged {
        id: String,
        from: AgentStatus,
        to: AgentStatus,
    },
    WorktreeChanged {
        id: String,
        ahead: u32,
        behind: u32,
        dirty: bool,
    },
    SessionUpdated {
        session: SessionInfo,
    },
    SessionRemoved {
        id: String,
    },
    ConfigStatus {
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        ui: Option<UiConfig>,
    },
    /// 分頁中的程式要求複製到剪貼簿（OSC 52）。UI 只在該分頁是目前分頁時轉給外層終端機。
    Clipboard {
        id: String,
        text: String,
    },
    DaemonStopping,
    Error {
        code: String,
        message: String,
    },
}

/// 錯誤代碼。
pub mod codes {
    pub const PROTO_MISMATCH: &str = "proto_mismatch";
    pub const NOT_FOUND: &str = "not_found";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const GIT: &str = "git";
    pub const SETUP_UNTRUSTED: &str = "setup_untrusted";
    pub const NEEDS_CONFIRM: &str = "needs_confirm";
    pub const NEED_STRATEGY: &str = "need_strategy";
    pub const CONFLICT: &str = "conflict";
    pub const INTERNAL: &str = "internal";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = Request {
            req_id: 7,
            msg: ClientMsg::Finish {
                id: "abc".into(),
                action: FinishAction::Merge {
                    strategy: Some(MergeStrategy::Squash),
                },
                force: false,
            },
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let s = r#"{"req_id":1,"type":"Hello","proto_version":1,"client_kind":"tui","future":42}"#;
        let r: Request = serde_json::from_str(s).unwrap();
        assert!(matches!(r.msg, ClientMsg::Hello { .. }));
    }

    #[test]
    fn reply_roundtrip() {
        let m = ServerMsg::Reply {
            req_id: 3,
            result: ReplyBody::Sessions(vec![]),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<ServerMsg>(&s).unwrap(), m);
    }
}
