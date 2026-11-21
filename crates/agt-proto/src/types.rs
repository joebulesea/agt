use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// Agent 狀態（規格 5.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentStatus {
    Starting,
    Running,
    Thinking,
    WaitingInput,
    Done,
    Idle,
    Error,
    Exited,
}

impl AgentStatus {
    pub const ALL: [AgentStatus; 8] = [
        AgentStatus::Starting,
        AgentStatus::Running,
        AgentStatus::Thinking,
        AgentStatus::WaitingInput,
        AgentStatus::Done,
        AgentStatus::Idle,
        AgentStatus::Error,
        AgentStatus::Exited,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Starting => "starting",
            AgentStatus::Running => "running",
            AgentStatus::Thinking => "thinking",
            AgentStatus::WaitingInput => "waiting",
            AgentStatus::Done => "done",
            AgentStatus::Idle => "idle",
            AgentStatus::Error => "error",
            AgentStatus::Exited => "exited",
        }
    }

    /// 左側欄燈號（規格 8.2）。顏色之外一定搭配符號。
    pub fn symbol(self) -> &'static str {
        match self {
            AgentStatus::Starting => "…",
            AgentStatus::Running => "●",
            AgentStatus::Thinking => "◐",
            AgentStatus::WaitingInput => "⚠",
            AgentStatus::Done => "✓",
            AgentStatus::Error => "✗",
            AgentStatus::Idle | AgentStatus::Exited => "○",
        }
    }

    /// 是否代表行程仍在執行。
    pub fn is_alive(self) -> bool {
        !matches!(self, AgentStatus::Exited)
    }

    /// 是否代表 Agent 正在忙（關閉分頁前需要確認）。
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            AgentStatus::Starting | AgentStatus::Running | AgentStatus::Thinking
        )
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "starting" => AgentStatus::Starting,
            "running" => AgentStatus::Running,
            "thinking" => AgentStatus::Thinking,
            "waiting" | "waitinginput" | "waiting_input" => AgentStatus::WaitingInput,
            "done" => AgentStatus::Done,
            "idle" => AgentStatus::Idle,
            "error" => AgentStatus::Error,
            "exited" => AgentStatus::Exited,
            other => return Err(format!("未知的狀態：{other}")),
        })
    }
}

/// 工作階段資訊（規格 5.1）；`screen` 只存在 daemon 記憶體，不在這裡。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub short_id: String,
    pub agent: String,
    #[serde(default)]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub repo_name: Option<String>,
    pub task: String,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub base_branch: Option<String>,
    /// 實際執行目錄（worktree 或 repo 目錄）。
    #[serde(default)]
    pub cwd: String,
    pub status: AgentStatus,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub behind: u32,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub created_at: i64,
    /// 與其他工作階段共用 worktree（`--share`）。
    #[serde(default)]
    pub shared: bool,
    /// 標籤（見 `tags::normalize`）。
    #[serde(default)]
    pub tags: Vec<String>,
    /// Agent 回報的用量（context、訂閱額度）；目前只有 Claude Code 會回報（見 `usage`）。
    /// 只存在記憶體，不寫進資料庫。
    #[serde(default)]
    pub usage: Option<crate::usage::Usage>,
}

impl SessionInfo {
    pub fn has_worktree(&self) -> bool {
        self.worktree.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeStrategy {
    Merge,
    Squash,
    Rebase,
}

impl FromStr for MergeStrategy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "merge" => Ok(MergeStrategy::Merge),
            "squash" => Ok(MergeStrategy::Squash),
            "rebase" => Ok(MergeStrategy::Rebase),
            o => Err(format!("未知的 merge 策略：{o}")),
        }
    }
}

/// 收尾動作（規格 7.3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum FinishAction {
    Merge {
        #[serde(default)]
        strategy: Option<MergeStrategy>,
    },
    Pr,
    Keep,
    Discard,
}

impl FinishAction {
    pub fn name(&self) -> &'static str {
        match self {
            FinishAction::Merge { .. } => "merge",
            FinishAction::Pr => "pr",
            FinishAction::Keep => "keep",
            FinishAction::Discard => "discard",
        }
    }
}

/// daemon 下發給 UI 的設定。TUI 不讀設定檔，只依賴協定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiConfig {
    pub prefix_key: String,
    pub sidebar_width: u16,
    /// ask | merge | squash | rebase
    pub merge_strategy: String,
    /// off | ask | on：分頁以 OSC 52 寫入剪貼簿時怎麼處理
    #[serde(default = "default_osc52")]
    pub osc52: String,
    #[serde(default)]
    pub agents: Vec<String>,
    /// 設了 `auto_commit` 的 Agent（`extends` 展開後）。新增分頁時用來提示收尾會自動 commit。
    #[serde(default)]
    pub auto_commit_agents: Vec<String>,
    /// dark | light | colorblind：TUI 的配色
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 設定檔 `[colors]`：覆寫主題中個別顏色（名稱見 `colors::COLOR_KEYS`）
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
    /// 設定檔 `[templates.<名稱>]`，依名稱排序。
    #[serde(default)]
    pub templates: Vec<TemplateInfo>,
}

/// 新增分頁的範本（設定檔 `[templates.<名稱>]`）：預先填好 Agent、任務名稱、base 與標籤。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub agent: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_theme() -> String {
    "dark".into()
}

fn default_osc52() -> String {
    "ask".into()
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            prefix_key: "ctrl-a".into(),
            sidebar_width: 26,
            merge_strategy: "ask".into(),
            osc52: default_osc52(),
            agents: vec![
                "claude".into(),
                "antigravity".into(),
                "copilot".into(),
                "codex".into(),
                "shell".into(),
            ],
            auto_commit_agents: Vec::new(),
            theme: default_theme(),
            colors: BTreeMap::new(),
            templates: Vec::new(),
        }
    }
}

/// `agt gc` 找到的不一致項目（規格 7.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GcItem {
    /// `orphan_worktree`：git 有、資料庫沒有；`missing_worktree`：資料庫有、磁碟上沒有；
    /// `closed_worktree`：分頁已關閉，但 worktree 副本還留著。
    pub kind: String,
    pub repo: String,
    pub path: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
}
