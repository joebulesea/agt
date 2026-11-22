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
    /// 進入目前狀態的時間（Unix 秒）；側欄用它顯示「等待輸入多久了」。
    /// 只存在記憶體：daemon 重啟後是載入的時間。0 表示未知。
    #[serde(default)]
    pub status_since: i64,
    /// 等待輸入時 Agent 在問什麼（hook 的訊息，或畫面上的提問那一行）；側欄預覽用。
    /// 只在狀態是 `WaitingInput` 時有值，已清除控制字元並限制長度。只存在記憶體。
    #[serde(default)]
    pub prompt: Option<String>,
    /// 自動檢查（`[repos] auto_check`）的狀態與結果；Agent 又開始工作時清除。只存在記憶體。
    #[serde(default)]
    pub check: Option<CheckInfo>,
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
    /// 同一個 repo 裡，也動過同一批檔案的其他分頁（已 commit、未 commit、未追蹤的都算）。
    /// 由 daemon 定期比對，只存在記憶體；沒有重疊時是空的。
    #[serde(default)]
    pub overlaps: Vec<Overlap>,
    /// 這個分頁收尾開出的 PR 有沒有進 base（見 `LandingInfo`；完整清單用 `agt landing`）。
    /// 由 daemon 從資料庫帶入，分頁關閉後只剩 `agt landing` 查得到。
    #[serde(default)]
    pub landing: Vec<LandingInfo>,
}

/// 側欄顯示 PR 進度的一行（見 [`SessionInfo::landing_label`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandingLabel {
    pub text: String,
    pub level: LandingLevel,
}

/// 顯示的顏色：異常（紅）、完成（綠）、一般提示（黃）、進行中（灰）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandingLevel {
    Problem,
    Done,
    Notice,
    Progress,
}

/// 「已進 base」的提示顯示多久（從查到結果算起）。
pub const LANDED_SHOW_SECS: i64 = 24 * 3600;

/// 與另一個分頁重疊的檔案。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlap {
    pub short_id: String,
    pub task: String,
    /// 重疊的檔案（排序後最多 [`Overlap::MAX_FILES`] 個）。
    pub files: Vec<String>,
    /// 重疊的檔案總數。
    pub total: usize,
}

impl Overlap {
    pub const MAX_FILES: usize = 20;
}

/// 自動檢查的進度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    Running,
    Passed,
    Failed,
}

/// 自動檢查的狀態；失敗時 `detail` 是輸出的最後一行（已清除控制字元、限制長度）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckInfo {
    pub state: CheckState,
    #[serde(default)]
    pub detail: String,
}

/// 收尾開出的 PR 之後有沒有進 base。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LandingState {
    /// PR 還開著，或還沒查到結果。
    Open,
    /// 已合併，內容在 base 裡。
    Landed,
    /// 已合併，但合進的不是預期的 base（例如堆疊 PR 只進了前一組的 branch）。
    WrongBase,
    /// PR 被關閉，沒有合併。
    Closed,
    /// 已合併到預期的 base，但合併 commit 不在 base 裡。
    Missing,
}

impl LandingState {
    pub fn as_str(self) -> &'static str {
        match self {
            LandingState::Open => "open",
            LandingState::Landed => "landed",
            LandingState::WrongBase => "wrongbase",
            LandingState::Closed => "closed",
            LandingState::Missing => "missing",
        }
    }

    /// 要提醒使用者的異常：內容沒進預期的 base。
    pub fn is_problem(self) -> bool {
        matches!(self, LandingState::WrongBase | LandingState::Missing)
    }

    /// 已有結果，不必再查。
    pub fn is_final(self) -> bool {
        self != LandingState::Open
    }
}

impl std::str::FromStr for LandingState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "open" => LandingState::Open,
            "landed" => LandingState::Landed,
            "wrongbase" => LandingState::WrongBase,
            "closed" => LandingState::Closed,
            "missing" => LandingState::Missing,
            other => return Err(format!("不認得的 landing 狀態：{other}")),
        })
    }
}

/// 追蹤中的一個 PR（`agt landing`）。不綁分頁：分頁關閉後仍在。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LandingInfo {
    pub short_id: String,
    pub task: String,
    pub repo_path: String,
    /// 組名；沒有分組時是 `pr`。
    pub name: String,
    pub branch: String,
    /// 內容最後應該進到的 branch。
    pub base: String,
    pub state: LandingState,
    /// 說明：進錯的 branch、或上次檢查失敗的原因（已清除控制字元、限制長度）。
    #[serde(default)]
    pub detail: String,
    pub created_at: i64,
    #[serde(default)]
    pub checked_at: Option<i64>,
}

impl SessionInfo {
    /// 狀態和 `before` 不同時，把 `status_since` 更新成現在。
    pub fn note_status_change(&mut self, before: AgentStatus) {
        if self.status != before {
            self.status_since = now_secs();
            if self.status != AgentStatus::WaitingInput {
                self.prompt = None;
            }
            // Agent 又開始工作：之前的檢查結果過期了
            if matches!(
                self.status,
                AgentStatus::Running | AgentStatus::Thinking | AgentStatus::Starting
            ) {
                self.check = None;
            }
        }
    }

    pub fn has_worktree(&self) -> bool {
        self.worktree.is_some()
    }

    /// 側欄的 PR 進度（一行）。異常（沒進預期的 base）一直顯示；全部進了 base 只顯示
    /// [`LANDED_SHOW_SECS`]；PR 都還開著時不顯示（避免吵）；堆疊的 PR 只進了一部分時顯示進度。
    pub fn landing_label(&self, now: i64) -> Option<LandingLabel> {
        let ls = &self.landing;
        let names = |f: &dyn Fn(&LandingInfo) -> bool| {
            ls.iter()
                .filter(|l| f(l))
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join("、")
        };
        if let Some(first) = ls.iter().find(|l| l.state.is_problem()) {
            return Some(LandingLabel {
                text: format!(
                    "⚠ PR 沒進 {}：{}",
                    first.base,
                    names(&|l| l.state.is_problem())
                ),
                level: LandingLevel::Problem,
            });
        }
        if ls.iter().any(|l| l.state == LandingState::Closed) {
            return Some(LandingLabel {
                text: format!(
                    "✗ PR 已關閉：{}",
                    names(&|l| l.state == LandingState::Closed)
                ),
                level: LandingLevel::Notice,
            });
        }
        let landed = ls
            .iter()
            .filter(|l| l.state == LandingState::Landed)
            .count();
        let recent = ls.iter().any(|l| {
            l.state == LandingState::Landed
                && l.checked_at.is_some_and(|t| now - t < LANDED_SHOW_SECS)
        });
        if landed == 0 || !recent {
            return None;
        }
        let base = &ls[0].base;
        Some(if landed == ls.len() {
            LandingLabel {
                text: format!("✓ PR 已進 {base}"),
                level: LandingLevel::Done,
            }
        } else {
            LandingLabel {
                text: format!("… PR 已進 {base}：{landed}/{}", ls.len()),
                level: LandingLevel::Progress,
            }
        })
    }

    /// 與其他分頁重疊時的一行說明（側欄與 `agt ls` 用），例如 `⚠ 與 a3f 重疊（3 檔）`。
    pub fn overlap_label(&self) -> Option<String> {
        match self.overlaps.as_slice() {
            [] => None,
            [o] => Some(format!("⚠ 與 {} 重疊（{} 檔）", o.short_id, o.total)),
            many => {
                let ids: Vec<&str> = many.iter().map(|o| o.short_id.as_str()).collect();
                Some(format!("⚠ 與 {} 重疊", ids.join("、")))
            }
        }
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
    /// 側欄：等待輸入的分頁多一行預覽 Agent 在問什麼。
    #[serde(default = "default_true")]
    pub sidebar_prompt: bool,
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

fn default_true() -> bool {
    true
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
            sidebar_prompt: true,
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

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod landing_label_tests {
    use super::*;

    fn li(name: &str, state: LandingState, checked: Option<i64>) -> LandingInfo {
        LandingInfo {
            short_id: "aaaaaa".into(),
            task: "t".into(),
            repo_path: "/r".into(),
            name: name.into(),
            branch: format!("agt/t-{name}"),
            base: "main".into(),
            state,
            detail: String::new(),
            created_at: 0,
            checked_at: checked,
        }
    }

    fn info(landing: Vec<LandingInfo>) -> SessionInfo {
        let mut s: SessionInfo = serde_json::from_str(
            r#"{"id":"i","short_id":"s","agent":"a","task":"t","status":"Running"}"#,
        )
        .unwrap();
        s.landing = landing;
        s
    }

    #[test]
    fn label_shows_problems_first_and_hides_pending() {
        use LandingState::*;
        let now = 1_000_000;
        assert!(info(vec![]).landing_label(now).is_none());
        // 都還開著：不顯示
        assert!(info(vec![li("a", Open, None)]).landing_label(now).is_none());
        let l = info(vec![
            li("a", Landed, Some(now)),
            li("b", WrongBase, Some(now)),
        ])
        .landing_label(now)
        .unwrap();
        assert_eq!(
            (l.text.as_str(), l.level),
            ("⚠ PR 沒進 main：b", LandingLevel::Problem)
        );
        let l = info(vec![li("a", Closed, Some(now))])
            .landing_label(now)
            .unwrap();
        assert_eq!(
            (l.text.as_str(), l.level),
            ("✗ PR 已關閉：a", LandingLevel::Notice)
        );
    }

    #[test]
    fn label_shows_progress_and_landed_expires() {
        use LandingState::*;
        let now = 1_000_000;
        let l = info(vec![li("a", Landed, Some(now)), li("b", Landed, Some(now))])
            .landing_label(now)
            .unwrap();
        assert_eq!(
            (l.text.as_str(), l.level),
            ("✓ PR 已進 main", LandingLevel::Done)
        );
        let l = info(vec![li("a", Landed, Some(now)), li("b", Open, None)])
            .landing_label(now)
            .unwrap();
        assert_eq!(
            (l.text.as_str(), l.level),
            ("… PR 已進 main：1/2", LandingLevel::Progress)
        );
        // 超過 24 小時後不再顯示
        let old = info(vec![li("a", Landed, Some(now - LANDED_SHOW_SECS - 1))]);
        assert!(old.landing_label(now).is_none());
    }
}
