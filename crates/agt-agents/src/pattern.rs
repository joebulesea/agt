//! 以輸出規則判斷狀態的通用 Adapter（Antigravity CLI、GitHub Copilot CLI 與自訂 Agent）。
//!
//! - 符合 `waiting_patterns` 且輸出已靜止 500 ms → WaitingInput（規格 6.5）
//! - 超過 `idle_timeout_secs` 沒有輸出且未符合任何規則 → Idle

use crate::{AgentSettings, base_command, compile_patterns};
use agt_core::{AgentAdapter, CommandSpec, LaunchCtx, OscEvent, ScreenView, StatusSourceKind};
use agt_proto::AgentStatus;
use std::time::Duration;

pub const QUIET_FOR_WAITING: Duration = Duration::from_millis(500);

/// Antigravity CLI 的預設等待規則。
const ANTIGRAVITY_PATTERNS: &[&str] = &[r"\(y/n\)", r"Allow\?"];
/// GitHub Copilot CLI 的預設等待規則：工具核准選單（「Do you want to run this command?」
/// 下方的「No, and tell Copilot what to do differently」）與資料夾信任詢問。
const COPILOT_PATTERNS: &[&str] = &[r"No, and tell Copilot", r"Do you trust the files"];

pub struct PatternAdapter {
    id: &'static str,
    settings: AgentSettings,
    patterns: Vec<regex::Regex>,
    waiting: bool,
}

impl PatternAdapter {
    pub fn new(id: &'static str, mut settings: AgentSettings) -> Self {
        let (command, patterns): (&str, &[&str]) = match id {
            "antigravity" => ("agy", ANTIGRAVITY_PATTERNS),
            "copilot" => ("copilot", COPILOT_PATTERNS),
            _ => ("", &[]),
        };
        if settings.command.is_empty() {
            settings.command = command.into();
        }
        if settings.waiting_patterns.is_empty() {
            settings.waiting_patterns = patterns.iter().map(|p| p.to_string()).collect();
        }
        let patterns = compile_patterns(&settings.waiting_patterns);
        PatternAdapter {
            id,
            settings,
            patterns,
            waiting: false,
        }
    }

    /// 規則比對（Claude Adapter 的備援也使用）。
    pub fn matches(&self, screen: &ScreenView) -> bool {
        let text = screen.text();
        self.patterns.iter().any(|p| p.is_match(&text))
    }
}

impl AgentAdapter for PatternAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn launch(&self, ctx: &LaunchCtx) -> CommandSpec {
        base_command(&self.settings.command, &self.settings, ctx)
    }

    fn install_hooks(&self, _ctx: &LaunchCtx) -> agt_core::Result<()> {
        // 尚未確認 Antigravity CLI / Copilot CLI 是否有可用的 hooks 機制（規格 14.2）
        Ok(())
    }

    fn on_osc(&mut self, ev: &OscEvent) -> Option<AgentStatus> {
        if self.settings.source() == StatusSourceKind::None {
            return None;
        }
        match ev {
            OscEvent::Notify { .. } => {
                self.waiting = true;
                Some(AgentStatus::WaitingInput)
            }
            _ => None,
        }
    }

    fn on_screen(&mut self, screen: &ScreenView) -> Option<AgentStatus> {
        if self.settings.source() == StatusSourceKind::None {
            return None;
        }
        if screen.quiet_for >= QUIET_FOR_WAITING {
            if self.matches(screen) {
                self.waiting = true;
                return Some(AgentStatus::WaitingInput);
            }
            None
        } else {
            self.waiting = false;
            Some(AgentStatus::Running)
        }
    }

    fn on_idle(&mut self, idle: Duration) -> Option<AgentStatus> {
        if self.settings.source() == StatusSourceKind::None || self.waiting {
            return None;
        }
        (idle >= Duration::from_secs(self.settings.idle_timeout_secs.max(1)))
            .then_some(AgentStatus::Idle)
    }

    fn on_output(&mut self, current: AgentStatus) -> Option<AgentStatus> {
        match current {
            AgentStatus::Idle | AgentStatus::Done => Some(AgentStatus::Running),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(lines: &[&str], quiet_ms: u64) -> ScreenView {
        ScreenView {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            quiet_for: Duration::from_millis(quiet_ms),
        }
    }

    #[test]
    fn waiting_requires_quiet() {
        let mut a = PatternAdapter::new("antigravity", AgentSettings::default());
        assert_eq!(
            a.on_screen(&view(&["Continue? (y/n)"], 100)),
            Some(AgentStatus::Running)
        );
        assert_eq!(
            a.on_screen(&view(&["Continue? (y/n)"], 600)),
            Some(AgentStatus::WaitingInput)
        );
        // 等待中不會因閒置而變成 Idle
        assert_eq!(a.on_idle(Duration::from_secs(60)), None);
        assert_eq!(
            a.on_screen(&view(&["working"], 10)),
            Some(AgentStatus::Running)
        );
        assert_eq!(a.on_idle(Duration::from_secs(60)), Some(AgentStatus::Idle));
    }

    #[test]
    fn copilot_defaults() {
        let mut a = PatternAdapter::new("copilot", AgentSettings::default());
        assert_eq!(a.settings.command, "copilot");
        let menu = [
            "Do you want to run this command?",
            "❯ 1. Yes",
            "  3. No, and tell Copilot what to do differently (Esc)",
        ];
        assert_eq!(
            a.on_screen(&view(&menu, 600)),
            Some(AgentStatus::WaitingInput)
        );
        // 自訂 Agent 沒有預設指令與規則
        let c = PatternAdapter::new("custom", AgentSettings::default());
        assert!(c.settings.command.is_empty() && c.patterns.is_empty());
    }

    #[test]
    fn default_command() {
        let a = PatternAdapter::new("antigravity", AgentSettings::default());
        let ctx = LaunchCtx {
            session_id: "s".into(),
            cwd: "/tmp".into(),
            agt_exe: "/bin/agt".into(),
            socket: "/sock".into(),
            conversation_id: "c".into(),
            resume: false,
            restarted: false,
            gitdir: None,
        };
        let spec = a.launch(&ctx);
        assert_eq!(spec.program, "agy");
        assert!(spec.env.contains(&("AGT_SESSION_ID".into(), "s".into())));
        assert!(spec.env.contains(&("AGT_SOCKET".into(), "/sock".into())));
    }
}
