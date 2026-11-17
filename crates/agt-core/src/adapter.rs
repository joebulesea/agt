//! Agent Adapter 介面（規格 6.1）。實作放在 `agt-agents`。

use agt_proto::AgentStatus;
use std::path::PathBuf;
use std::time::Duration;

/// 啟動 Agent 所需的資訊。
#[derive(Debug, Clone)]
pub struct LaunchCtx {
    pub session_id: String,
    /// 工作目錄（worktree，或沒有 worktree 時的 repo 目錄）。
    pub cwd: PathBuf,
    /// `agt` 執行檔的絕對路徑，hooks 會呼叫 `agt notify`。
    pub agt_exe: PathBuf,
    /// daemon 的 socket / pipe 路徑。
    pub socket: String,
    /// 這個分頁目前的對話 ID（UUID），設定中的 `{conversation}`。開新對話時會換一個。
    pub conversation_id: String,
    /// true = 接續上次的對話（加上 `resume_args`）；false = 開新對話（加上 `new_args`）。
    pub resume: bool,
    /// true = 這個分頁之前已經啟動過（重新啟動）；有設定 `restart_args` 時用它取代 `args`。
    pub restarted: bool,
    /// 主 repo 的 `.git` 目錄（設定中的 `{gitdir}`）；不是 git worktree 時為 None。
    /// 把它掛進容器 / 沙箱，Agent 在裡面才能使用 git（worktree 的 `.git` 指向這裡）。
    pub gitdir: Option<PathBuf>,
}

/// 要在 PTY 中執行的指令。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// 收尾指令（例如 `docker rm -f agt-<id>`）：啟動前、Agent 結束後與被關閉時各執行一次，
    /// 用來清掉 Agent 留下的外部資源。空的代表不需要。
    pub cleanup: Vec<String>,
}

/// 從 OSC 序列或 BEL 得到的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum OscEvent {
    /// OSC 9（iTerm2 / ConEmu 通知）或 OSC 777;notify。
    Notify {
        title: Option<String>,
        body: String,
    },
    Bell,
    Title(String),
}

/// 畫面最後幾行的文字（規則比對用）。
#[derive(Debug, Clone, Default)]
pub struct ScreenView {
    pub lines: Vec<String>,
    /// 最後一次輸出距今多久。
    pub quiet_for: Duration,
}

impl ScreenView {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// 狀態來源（規格 9 的 `status_source`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSourceKind {
    Hooks,
    Pattern,
    None,
}

pub trait AgentAdapter: Send + Sync {
    /// "claude" | "antigravity" | "shell"
    fn id(&self) -> &'static str;
    /// 指令、參數、環境變數、cwd = worktree。
    fn launch(&self, ctx: &LaunchCtx) -> CommandSpec;
    /// 在 worktree 裡安裝狀態回報。
    fn install_hooks(&self, ctx: &LaunchCtx) -> crate::Result<()>;
    fn on_osc(&mut self, ev: &OscEvent) -> Option<AgentStatus>;
    /// 備援：規則比對。最多每 250 ms 呼叫一次，只看畫面最後 10 行。
    fn on_screen(&mut self, screen: &ScreenView) -> Option<AgentStatus>;
    fn on_idle(&mut self, idle: Duration) -> Option<AgentStatus>;
    /// 有新輸出時呼叫。預設：不改變狀態（Starting → Running 由 Session 處理）。
    fn on_output(&mut self, _current: AgentStatus) -> Option<AgentStatus> {
        None
    }
    fn status_source(&self) -> StatusSourceKind {
        StatusSourceKind::Pattern
    }
}
