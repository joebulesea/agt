//! AgentAdapter 實作（規格 6）。新增一種 Agent 只需要實作一個 Adapter 加上一段設定。
//! 設定檔中未知的 Agent 名稱會使用通用的 [`PatternAdapter`]。

mod claude;
mod pattern;
mod shell;

pub use claude::ClaudeAdapter;
pub use pattern::PatternAdapter;
pub use shell::ShellAdapter;

use agt_core::{AgentAdapter, CommandSpec, LaunchCtx, StatusSourceKind};
use agt_proto::transport::{ENV_SESSION, ENV_SOCKET};
use serde::Deserialize;

/// 設定檔 `[agents.<name>]` 的內容（規格 9）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentSettings {
    /// 空字串 = 使用者預設 shell。
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// hooks | pattern | none
    #[serde(default = "default_source")]
    pub status_source: String,
    #[serde(default)]
    pub waiting_patterns: Vec<String>,
    #[serde(default = "default_idle")]
    pub idle_timeout_secs: u64,
    /// 是否為這種 Agent 建立 worktree。
    #[serde(default = "default_true")]
    pub worktree: bool,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// 判斷狀態的方式沿用哪一種內建 Agent：claude | antigravity | copilot | shell | pattern。
    /// 空字串 = 依 Agent 名稱（`claude`、`shell`、`antigravity`、`copilot`，其他名稱用 pattern）。
    /// 例如在容器裡執行的 `claude-docker` 設成 `"claude"`，就能沿用 Claude Code 的規則。
    #[serde(default)]
    pub adapter: String,
    /// 收尾指令：啟動前、Agent 結束後與分頁被關閉時執行，例如 `["docker", "rm", "-f", "agt-{session}"]`。
    #[serde(default)]
    pub cleanup: Vec<String>,
    /// 關閉分頁（關閉、收尾）時執行一次，例如 `["sbx", "rm", "-f", "agt-{session}"]`。
    /// 和 `cleanup` 不同：重新啟動 Agent、Agent 結束或 daemon 停止時都不會執行，
    /// 適合用來刪除要跨重新啟動保留的資源（例如保存對話記錄的沙箱）。
    #[serde(default)]
    pub on_close: Vec<String>,
    /// 收尾（merge / pr）前，自動把 worktree 中未 commit 的變更 commit 起來。
    /// 給無法自行使用 git 的 Agent（例如 `claude-docker`）使用。
    #[serde(default)]
    pub auto_commit: bool,
    /// 更新指令：daemon 啟動時在背景執行一次（`update_on_new` 時新建分頁也會執行），例如 `["claude", "update"]`。
    /// 不支援 `{cwd}`、`{session}` 變數。
    #[serde(default)]
    pub update: Vec<String>,
    /// 新建分頁時，先在分頁中執行一次 `update` 再啟動 Agent（輸出顯示在分頁中；
    /// 更新失敗仍會啟動 Agent）。預設開啟；設成 `false` 則只在 daemon 啟動時更新。
    /// 重新啟動（`R` / `N`）不會執行。
    #[serde(default = "default_true")]
    pub update_on_new: bool,
    /// 開新對話時接在 `args` 後面的參數，例如 `["--session-id", "{conversation}"]`。
    #[serde(default)]
    pub new_args: Vec<String>,
    /// 接續上次對話時（`R` 重新啟動）接在 `args` 後面的參數，例如 `["--resume", "{conversation}"]`。
    /// 空的 = 不支援接續，重新啟動一律開新對話。
    #[serde(default)]
    pub resume_args: Vec<String>,
    /// 重新啟動（`R` / `N`）時取代 `args`；空的 = 沿用 `args`。
    /// 例如 Docker Sandboxes：沙箱第一次要用 `sbx run --name X claude <路徑>` 建立，
    /// 之後沙箱已存在、不能再指定路徑，只能用 `sbx run --name X` 接回去。
    #[serde(default)]
    pub restart_args: Vec<String>,
}

/// 可用的 `adapter` 值。
pub const ADAPTERS: &[&str] = &["claude", "antigravity", "copilot", "shell", "pattern"];

fn default_source() -> String {
    "pattern".into()
}
fn default_idle() -> u64 {
    20
}
fn default_true() -> bool {
    true
}

impl Default for AgentSettings {
    fn default() -> Self {
        AgentSettings {
            command: String::new(),
            args: vec![],
            status_source: default_source(),
            waiting_patterns: vec![],
            idle_timeout_secs: default_idle(),
            worktree: true,
            env: Default::default(),
            adapter: String::new(),
            cleanup: vec![],
            on_close: vec![],
            auto_commit: false,
            update: vec![],
            update_on_new: true,
            new_args: vec![],
            resume_args: vec![],
            restart_args: vec![],
        }
    }
}

impl AgentSettings {
    pub fn source(&self) -> StatusSourceKind {
        match self.status_source.as_str() {
            "hooks" => StatusSourceKind::Hooks,
            "none" => StatusSourceKind::None,
            _ => StatusSourceKind::Pattern,
        }
    }

    /// 檢查設定內容（正規表達式可以編譯等）。
    pub fn validate(&self, name: &str) -> Result<(), String> {
        if !matches!(self.status_source.as_str(), "hooks" | "pattern" | "none") {
            return Err(format!(
                "agents.{name}.status_source 必須是 hooks、pattern 或 none，而不是 {:?}",
                self.status_source
            ));
        }
        if !self.adapter.is_empty() && !ADAPTERS.contains(&self.adapter.as_str()) {
            return Err(format!(
                "agents.{name}.adapter 必須是 {} 之一，而不是 {:?}",
                ADAPTERS.join("、"),
                self.adapter
            ));
        }
        for p in &self.waiting_patterns {
            regex::Regex::new(p).map_err(|e| {
                format!("agents.{name}.waiting_patterns 的正規表達式 {p:?} 無效：{e}")
            })?;
        }
        Ok(())
    }
}

/// 建立 Adapter。
pub fn build(agent: &str, settings: &AgentSettings) -> Result<Box<dyn AgentAdapter>, String> {
    settings.validate(agent)?;
    let kind = if settings.adapter.is_empty() {
        agent
    } else {
        settings.adapter.as_str()
    };
    Ok(match kind {
        "claude" => Box::new(ClaudeAdapter::new(settings.clone())),
        "shell" => Box::new(ShellAdapter::new(settings.clone())),
        "antigravity" => Box::new(PatternAdapter::new("antigravity", settings.clone())),
        "copilot" => Box::new(PatternAdapter::new("copilot", settings.clone())),
        _ => Box::new(PatternAdapter::new("custom", settings.clone())),
    })
}

/// 各 Adapter 共用：組出指令並注入 `AGT_SESSION_ID` 與 `AGT_SOCKET`（規格 6.4）。
pub(crate) fn base_command(
    program: &str,
    settings: &AgentSettings,
    ctx: &LaunchCtx,
) -> CommandSpec {
    let mut env: Vec<(String, String)> = settings
        .env
        .iter()
        .map(|(k, v)| (k.clone(), expand(v, ctx)))
        .collect();
    env.push((ENV_SESSION.into(), ctx.session_id.clone()));
    env.push((ENV_SOCKET.into(), ctx.socket.clone()));
    let extra = if ctx.resume {
        &settings.resume_args
    } else {
        &settings.new_args
    };
    CommandSpec {
        program: program.to_string(),
        args: expand_all(
            if ctx.restarted && !settings.restart_args.is_empty() {
                &settings.restart_args
            } else {
                &settings.args
            }
            .iter()
            .chain(extra),
            ctx,
        ),
        env,
        cwd: ctx.cwd.clone(),
        cleanup: expand_all(&settings.cleanup, ctx),
    }
}

/// 關閉分頁時要執行的指令（`on_close`，已替換變數）。
pub fn on_close_command(settings: &AgentSettings, ctx: &LaunchCtx) -> Vec<String> {
    expand_all(&settings.on_close, ctx)
}

/// 替換參數中的變數：`{cwd}` = 工作目錄（worktree），`{session}` = 分頁 id，
/// `{conversation}` = 分頁目前的對話 ID，`{gitdir}` = 主 repo 的 `.git` 目錄。
pub fn expand(s: &str, ctx: &LaunchCtx) -> String {
    let gitdir = ctx
        .gitdir
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    s.replace("{cwd}", &ctx.cwd.to_string_lossy())
        .replace("{session}", &ctx.session_id)
        .replace("{conversation}", &ctx.conversation_id)
        .replace("{gitdir}", &gitdir)
}

/// 替換整串參數。沒有 `{gitdir}`（不是 git worktree）時，含有 `{gitdir}` 的參數整個略過，
/// 例如掛載用的 `"{gitdir}:ro"` 不會變成無效的 `":ro"`。
pub fn expand_all<'a>(list: impl IntoIterator<Item = &'a String>, ctx: &LaunchCtx) -> Vec<String> {
    list.into_iter()
        .filter(|a| ctx.gitdir.is_some() || !a.contains("{gitdir}"))
        .map(|a| expand(a, ctx))
        .collect()
}

pub(crate) fn compile_patterns(list: &[String]) -> Vec<regex::Regex> {
    list.iter()
        .filter_map(|p| regex::Regex::new(p).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> LaunchCtx {
        LaunchCtx {
            session_id: "abc123".into(),
            cwd: std::path::PathBuf::from("/wt/proj"),
            agt_exe: "/bin/agt".into(),
            socket: "/tmp/agt.sock".into(),
            conversation_id: "conv-1".into(),
            resume: false,
            restarted: false,
            gitdir: None,
        }
    }

    #[test]
    fn restart_args_replace_args() {
        let s = AgentSettings {
            command: "sbx".into(),
            args: vec![
                "run".into(),
                "--name".into(),
                "agt-{session}".into(),
                "claude".into(),
                "{cwd}".into(),
                "--".into(),
            ],
            restart_args: vec![
                "run".into(),
                "--name".into(),
                "agt-{session}".into(),
                "--".into(),
            ],
            resume_args: vec!["--resume".into(), "{conversation}".into()],
            ..Default::default()
        };
        let mut c = ctx();
        assert_eq!(
            base_command("sbx", &s, &c).args,
            ["run", "--name", "agt-abc123", "claude", "/wt/proj", "--"]
        );
        c.restarted = true;
        c.resume = true;
        assert_eq!(
            base_command("sbx", &s, &c).args,
            ["run", "--name", "agt-abc123", "--", "--resume", "conv-1"]
        );
        // 沒有設定 restart_args 時沿用 args
        let plain = AgentSettings {
            args: vec!["x".into()],
            ..Default::default()
        };
        assert_eq!(base_command("a", &plain, &c).args, ["x"]);
    }

    #[test]
    fn gitdir_args_and_on_close() {
        let s = AgentSettings {
            command: "sbx".into(),
            args: vec![
                "run".into(),
                "{cwd}".into(),
                "{gitdir}:ro".into(),
                "--".into(),
            ],
            on_close: vec!["sbx".into(), "rm".into(), "agt-{session}".into()],
            ..Default::default()
        };
        let mut c = ctx();
        // 不是 git worktree：含 {gitdir} 的參數整個略過
        assert_eq!(base_command("sbx", &s, &c).args, ["run", "/wt/proj", "--"]);
        c.gitdir = Some("/repo/.git".into());
        assert_eq!(
            base_command("sbx", &s, &c).args,
            ["run", "/wt/proj", "/repo/.git:ro", "--"]
        );
        assert_eq!(on_close_command(&s, &c), ["sbx", "rm", "agt-abc123"]);
        assert!(
            base_command("sbx", &s, &c).cleanup.is_empty(),
            "on_close 不是 cleanup"
        );
    }

    #[test]
    fn new_and_resume_args() {
        let s = AgentSettings {
            command: "claude".into(),
            args: vec!["--verbose".into()],
            new_args: vec!["--session-id".into(), "{conversation}".into()],
            resume_args: vec!["--resume".into(), "{conversation}".into()],
            ..Default::default()
        };
        let mut c = ctx();
        assert_eq!(
            base_command("claude", &s, &c).args,
            ["--verbose", "--session-id", "conv-1"]
        );
        c.resume = true;
        assert_eq!(
            base_command("claude", &s, &c).args,
            ["--verbose", "--resume", "conv-1"]
        );
    }

    #[test]
    fn placeholders_are_expanded() {
        let s = AgentSettings {
            command: "docker".into(),
            args: vec![
                "run".into(),
                "--name".into(),
                "agt-{session}".into(),
                "-v".into(),
                "{cwd}:/work".into(),
            ],
            cleanup: vec![
                "docker".into(),
                "rm".into(),
                "-f".into(),
                "agt-{session}".into(),
            ],
            ..Default::default()
        };
        let spec = base_command("docker", &s, &ctx());
        assert_eq!(
            spec.args,
            ["run", "--name", "agt-abc123", "-v", "/wt/proj:/work"]
        );
        assert_eq!(spec.cleanup, ["docker", "rm", "-f", "agt-abc123"]);
    }

    #[test]
    fn adapter_override() {
        let s = AgentSettings {
            command: "docker".into(),
            adapter: "claude".into(),
            ..Default::default()
        };
        assert_eq!(build("claude-docker", &s).unwrap().id(), "claude");
        assert_eq!(
            build("copilot", &AgentSettings::default()).unwrap().id(),
            "copilot"
        );
        assert_eq!(
            build("claude-docker", &AgentSettings::default())
                .unwrap()
                .id(),
            "custom"
        );
        let bad = AgentSettings {
            adapter: "nope".into(),
            ..Default::default()
        };
        assert!(build("x", &bad).is_err());
    }
}
