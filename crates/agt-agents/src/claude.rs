//! Claude Code Adapter（規格 6.3）。
//!
//! 主要狀態來源是 hooks：寫入 worktree 內的 `.claude/settings.local.json`（合併既有設定），
//! 並把該檔加入 `info/exclude`，不動使用者的全域設定、也不會被 commit。
//! 備援為 OSC 9 / 777、BEL 與輸出規則。

use crate::pattern::{PatternAdapter, QUIET_FOR_WAITING};
use crate::{AgentSettings, base_command};
use agt_core::{AgentAdapter, CommandSpec, LaunchCtx, OscEvent, ScreenView, StatusSourceKind};
use agt_proto::AgentStatus;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const SETTINGS_FILE: &str = ".claude/settings.local.json";
/// 用來辨識 agt 安裝的 hook，重新安裝時先移除舊的。
const MARKER: &str = "notify --status";

/// Claude Code hook → `agt notify` 參數。
pub const HOOKS: &[(&str, &str)] = &[
    ("UserPromptSubmit", "running"),
    ("PreToolUse", "running"),
    ("Notification", "waiting"),
    ("Stop", "done"),
];

pub struct ClaudeAdapter {
    settings: AgentSettings,
    fallback: PatternAdapter,
    hooks_active: AtomicBool,
    /// 沒有 hooks 時：畫面上出現過「esc to interrupt」（Claude 正在處理），用來判斷這一輪何時完成。
    working: bool,
    /// 沒有 hooks 時：判定為完成後保持 Done，不被閒置計時改成 Idle。
    done: bool,
}

/// Claude 處理中時，輸入框上方的狀態列會顯示這段文字。
const WORKING_MARK: &str = "esc to interrupt";
/// 「esc to interrupt」消失、且輸出靜止這麼久之後，視為這一輪已完成。
const QUIET_FOR_DONE: Duration = Duration::from_millis(1500);

impl ClaudeAdapter {
    pub fn new(mut settings: AgentSettings) -> Self {
        if settings.command.is_empty() {
            settings.command = "claude".into();
        }
        let mut fb = settings.clone();
        if fb.waiting_patterns.is_empty() {
            fb.waiting_patterns = vec![
                r"Do you want to (proceed|make this edit|create)".into(),
                r"❯ 1\. Yes".into(),
                r"\(y/n\)".into(),
                // 尚未登入（例如新的沙箱）、登入流程等待貼上授權碼
                r"Not logged in".into(),
                r"Paste code here".into(),
            ];
        }
        ClaudeAdapter {
            fallback: PatternAdapter::new("custom", fb),
            settings,
            hooks_active: AtomicBool::new(false),
            working: false,
            done: false,
        }
    }

    fn hooks_mode(&self) -> bool {
        self.hooks_active.load(Ordering::Relaxed)
    }
}

/// 產生 hook 指令字串。Windows 上 Claude Code 透過 bash 執行 hooks，所以統一用正斜線。
pub fn hook_command(agt_exe: &Path, status: &str) -> String {
    let exe = agt_exe.to_string_lossy().replace('\\', "/");
    format!("\"{exe}\" {MARKER} {status}")
}

/// 把 agt 的 hooks 合併進既有的 settings JSON；其他設定與使用者自己的 hooks 保持不變。
pub fn merge_hooks(existing: Option<&str>, agt_exe: &Path) -> agt_core::Result<String> {
    let mut root: Value = match existing.map(str::trim) {
        Some(s) if !s.is_empty() => serde_json::from_str(s)?,
        _ => json!({}),
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| agt_core::Error::msg(format!("{SETTINGS_FILE} 不是 JSON 物件")))?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks: &mut Map<String, Value> = hooks.as_object_mut().unwrap();
    for (event, status) in HOOKS {
        let list = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
        if !list.is_array() {
            *list = json!([]);
        }
        let arr = list.as_array_mut().unwrap();
        // 移除先前由 agt 安裝的項目
        arr.retain(|group| {
            !group["hooks"].as_array().is_some_and(|hs| {
                hs.iter()
                    .any(|h| h["command"].as_str().is_some_and(|c| c.contains(MARKER)))
            })
        });
        let mut group = json!({
            "hooks": [{ "type": "command", "command": hook_command(agt_exe, status), "timeout": 5 }]
        });
        if *event == "PreToolUse" {
            group["matcher"] = json!("*");
        }
        arr.push(group);
    }
    Ok(serde_json::to_string_pretty(&root)? + "\n")
}

/// 寫入 hooks 並加入 exclude。
pub fn install(cwd: &Path, agt_exe: &Path) -> agt_core::Result<()> {
    let path = cwd.join(SETTINGS_FILE);
    let existing = std::fs::read_to_string(&path).ok();
    let merged = merge_hooks(existing.as_deref(), agt_exe)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, merged)?;
    if let Err(e) = agt_git::add_exclude(cwd, SETTINGS_FILE) {
        tracing::debug!("無法加入 info/exclude（可能不是 git repo）：{e}");
    }
    Ok(())
}

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn launch(&self, ctx: &LaunchCtx) -> CommandSpec {
        base_command(&self.settings.command, &self.settings, ctx)
    }

    fn install_hooks(&self, ctx: &LaunchCtx) -> agt_core::Result<()> {
        if self.settings.source() != StatusSourceKind::Hooks {
            return Ok(());
        }
        // 規格 14.1：安裝前檢查 `claude --version`；失敗時退回規則比對。
        // `.cmd`（npm 安裝）要經由 cmd.exe 執行，和啟動 Agent 時一樣用 resolve_program。
        let (program, pre_args) = agt_core::platform::resolve_program(&self.settings.command);
        let mut probe = std::process::Command::new(program);
        probe
            .args(pre_args)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            // daemon 沒有 console，避免每次開分頁都跳出視窗
            use std::os::windows::process::CommandExt;
            probe.creation_flags(0x0800_0000);
        }
        let ok = probe.status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            tracing::warn!(
                "`{} --version` 失敗，改用規則比對判斷狀態",
                self.settings.command
            );
            return Ok(());
        }
        install(&ctx.cwd, &ctx.agt_exe)?;
        self.hooks_active.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn on_osc(&mut self, ev: &OscEvent) -> Option<AgentStatus> {
        match ev {
            OscEvent::Notify { .. } | OscEvent::Bell => Some(AgentStatus::WaitingInput),
            OscEvent::Title(_) => None,
        }
    }

    fn on_screen(&mut self, screen: &ScreenView) -> Option<AgentStatus> {
        if self.settings.source() == StatusSourceKind::None {
            return None;
        }
        if self.hooks_mode() {
            // hooks 正常時只用規則補抓「等待確認」
            return (screen.quiet_for >= QUIET_FOR_WAITING && self.fallback.matches(screen))
                .then_some(AgentStatus::WaitingInput);
        }
        // 沒有 hooks（容器、沙箱）：用「esc to interrupt」判斷 Claude 是否還在處理
        let busy = screen.text().to_lowercase().contains(WORKING_MARK);
        if busy {
            self.working = true;
            self.done = false;
        }
        let st = self.fallback.on_screen(screen);
        if st == Some(AgentStatus::WaitingInput) {
            self.working = false;
            return st;
        }
        if busy {
            return Some(AgentStatus::Running);
        }
        if self.working && screen.quiet_for >= QUIET_FOR_DONE {
            self.working = false;
            self.done = true;
            return Some(AgentStatus::Done);
        }
        if self.done { None } else { st }
    }

    fn on_idle(&mut self, idle: Duration) -> Option<AgentStatus> {
        if self.hooks_mode() || self.done || self.working {
            None
        } else {
            self.fallback.on_idle(idle)
        }
    }

    fn on_output(&mut self, current: AgentStatus) -> Option<AgentStatus> {
        if self.hooks_mode() || self.done {
            // 完成後畫面的小更新（例如游標、輸入）不算重新開始，等出現「esc to interrupt」再改
            None
        } else {
            self.fallback.on_output(current)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_existing_settings() {
        let existing = r#"{
            "permissions": {"allow": ["Bash(ls)"]},
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}
        }"#;
        let out = merge_hooks(Some(existing), Path::new("/usr/bin/agt")).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls)");
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "使用者自己的 hook 要保留");
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "*");
        assert!(
            v["hooks"]["Notification"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("notify --status waiting")
        );

        // 重複安裝不會累積
        let again = merge_hooks(Some(&out), Path::new("/usr/bin/agt")).unwrap();
        let v2: Value = serde_json::from_str(&again).unwrap();
        assert_eq!(v2["hooks"]["Stop"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn windows_paths_use_forward_slashes() {
        let c = hook_command(Path::new(r"C:\Program Files\agt\agt.exe"), "done");
        assert_eq!(c, r#""C:/Program Files/agt/agt.exe" notify --status done"#);
    }

    #[test]
    fn install_writes_file_and_excludes() {
        let d = tempfile::tempdir().unwrap();
        agt_git::git(d.path(), &["init", "-q"]).unwrap();
        install(d.path(), Path::new("/x/agt")).unwrap();
        assert!(d.path().join(SETTINGS_FILE).exists());
        assert!(
            !agt_git::is_dirty(d.path(), true).unwrap(),
            "settings.local.json 應被 exclude"
        );
    }

    #[test]
    fn fallback_without_hooks() {
        let mut a = ClaudeAdapter::new(AgentSettings {
            status_source: "hooks".into(),
            ..Default::default()
        });
        let v = ScreenView {
            lines: vec!["Do you want to proceed?".into(), "❯ 1. Yes".into()],
            quiet_for: Duration::from_secs(1),
        };
        assert_eq!(a.on_screen(&v), Some(AgentStatus::WaitingInput));
        assert_eq!(a.on_osc(&OscEvent::Bell), Some(AgentStatus::WaitingInput));
    }

    #[test]
    fn pattern_mode_detects_turn_done() {
        let mut a = ClaudeAdapter::new(AgentSettings {
            status_source: "pattern".into(),
            ..Default::default()
        });
        let v = |lines: &[&str], ms: u64| ScreenView {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            quiet_for: Duration::from_millis(ms),
        };
        // 處理中：即使輸出暫停也維持 Running，不會變成 Idle
        let busy = ["✻ Thinking… (esc to interrupt)", "> "];
        assert_eq!(a.on_screen(&v(&busy, 100)), Some(AgentStatus::Running));
        assert_eq!(a.on_screen(&v(&busy, 3000)), Some(AgentStatus::Running));
        assert_eq!(a.on_idle(Duration::from_secs(60)), None);
        // 狀態列消失、輸出靜止 → Done，之後不被閒置或小更新改掉
        let idle = ["● 完成了", "> "];
        assert_eq!(a.on_screen(&v(&idle, 200)), Some(AgentStatus::Running));
        assert_eq!(a.on_screen(&v(&idle, 2000)), Some(AgentStatus::Done));
        assert_eq!(a.on_idle(Duration::from_secs(60)), None);
        assert_eq!(a.on_output(AgentStatus::Done), None);
        assert_eq!(a.on_screen(&v(&idle, 100)), None);
        // 下一輪開始
        assert_eq!(a.on_screen(&v(&busy, 100)), Some(AgentStatus::Running));
        // 未登入 → 等待輸入
        let mut b = ClaudeAdapter::new(AgentSettings::default());
        assert_eq!(
            b.on_screen(&v(&["> ", "Not logged in · Run /login"], 800)),
            Some(AgentStatus::WaitingInput)
        );
    }
}
