//! 設定檔（規格 9）：預設值 ← 全域設定 ← repo 的 `.agt.toml`。

use agt_agents::AgentSettings;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const DEFAULT_TOML: &str = include_str!("../../../config/default.toml");
/// 環境變數：把設定、資料與 worktree 都放在這個目錄下（測試與可攜模式用）。
pub const ENV_HOME: &str = "AGT_HOME";

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct General {
    pub prefix_key: String,
    pub sidebar_width: u16,
    pub worktree_root: String,
    pub notify: bool,
    pub idle_shutdown_secs: u64,
    pub scrollback_lines: usize,
    pub merge_strategy: String,
    pub log_output: bool,
    /// 分頁中的程式以 OSC 52 寫入剪貼簿時：off | ask | on
    pub osc52: String,
    /// 收尾（merge / pr）前要求確認的額外路徑，加在內建清單之外。
    pub sensitive_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    #[serde(default)]
    pub copy_files: Vec<String>,
    #[serde(default)]
    pub setup: Option<String>,
    #[serde(default)]
    pub base_branch: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub general: General,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentSettings>,
    #[serde(default)]
    pub repos: BTreeMap<String, RepoConfig>,
}

/// 只用來檢查使用者設定檔（每個欄位都可省略），以取得精確的行號。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct PartialConfig {
    #[serde(default)]
    general: Option<PartialGeneral>,
    // AgentSettings 每個欄位都有預設值，直接拿來檢查；新增欄位時不必再改這裡
    #[serde(default)]
    agents: BTreeMap<String, AgentSettings>,
    #[serde(default)]
    repos: BTreeMap<String, RepoConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct PartialGeneral {
    prefix_key: Option<String>,
    sidebar_width: Option<u16>,
    worktree_root: Option<String>,
    notify: Option<bool>,
    idle_shutdown_secs: Option<u64>,
    scrollback_lines: Option<usize>,
    merge_strategy: Option<String>,
    log_output: Option<bool>,
    osc52: Option<String>,
    sensitive_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub home: PathBuf,
    /// 終端機紀錄檔的位置；None = `data_dir/logs`。
    /// Windows 放在 `%LOCALAPPDATA%\agt\logs`：紀錄檔可能含有機密且很大，
    /// 不能放在會隨網域漫遊設定檔同步到伺服器的 `%APPDATA%`。
    pub logs_dir: Option<PathBuf>,
}

impl Paths {
    pub fn detect() -> Paths {
        let home = directories::BaseDirs::new()
            .map(|b| b.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        if let Ok(h) = std::env::var(ENV_HOME)
            && !h.is_empty()
        {
            let h = PathBuf::from(h);
            return Paths {
                config_file: h.join("config.toml"),
                data_dir: h.join("data"),
                home: h,
                logs_dir: None,
            };
        }
        let base = directories::BaseDirs::new();
        let config_dir = base
            .as_ref()
            .map(|b| b.config_dir().join("agt"))
            .unwrap_or_else(|| home.join(".config/agt"));
        // Windows：所有東西都放在 %APPDATA%\agt 底下（資料在 data\ 子目錄）。
        // 其他平台維持 XDG 慣例：設定在 ~/.config/agt，資料在 ~/.local/share/agt。
        let data_dir = if cfg!(windows) {
            config_dir.join("data")
        } else {
            base.as_ref()
                .map(|b| b.data_local_dir().join("agt"))
                .unwrap_or_else(|| home.join(".agt/data"))
        };
        let logs_dir = if cfg!(windows) {
            base.as_ref()
                .map(|b| b.data_local_dir().join("agt").join("logs"))
        } else {
            None
        };
        Paths {
            config_file: config_dir.join("config.toml"),
            data_dir,
            home,
            logs_dir,
        }
    }

    /// 舊版的資料目錄（Windows：`%LOCALAPPDATA%\agt`），用來搬移到新位置。
    /// 設定了 `AGT_HOME` 或不是 Windows 時沒有舊位置。
    pub fn legacy_data_dir() -> Option<PathBuf> {
        if !cfg!(windows) || std::env::var(ENV_HOME).is_ok_and(|h| !h.is_empty()) {
            return None;
        }
        directories::BaseDirs::new().map(|b| b.data_local_dir().join("agt"))
    }

    /// 把舊位置的資料（資料庫、終端機紀錄、日誌）整個搬到 `data_dir`。
    ///
    /// 只在新位置還不存在、舊位置存在、而且舊版 daemon 沒有在執行時搬移；
    /// 舊版 daemon 還在執行或搬移失敗時，這次繼續使用舊位置，不會遺失分頁資料。
    /// 回傳要記錄到日誌的訊息。
    pub fn migrate_from(&mut self, legacy: &Path) -> Option<String> {
        // 舊版的資料目錄一定有資料庫；只有 logs\ 的 %LOCALAPPDATA%\agt 是新版放紀錄檔的地方
        if self.data_dir.exists() || !legacy.join("agt.db").is_file() {
            return None;
        }
        // 舊版 daemon 持有舊位置的 lock：還在執行時不能搬
        let old_lock = legacy.join("agtd.lock");
        if old_lock.exists() {
            match std::fs::OpenOptions::new().write(true).open(&old_lock) {
                Ok(f) if f.try_lock().is_ok() => drop(f),
                _ => {
                    let msg = format!(
                        "舊版 daemon 仍在使用 {}，這次沿用舊位置；停止舊版 daemon 後重新啟動即會搬移",
                        legacy.display()
                    );
                    self.data_dir = legacy.to_path_buf();
                    return Some(msg);
                }
            }
        }
        if let Some(parent) = self.data_dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::rename(legacy, &self.data_dir) {
            Ok(()) => Some(format!(
                "已把資料從 {} 搬到 {}",
                legacy.display(),
                self.data_dir.display()
            )),
            Err(e) => {
                let msg = format!(
                    "無法把資料從 {} 搬到 {}（{e}），這次沿用舊位置",
                    legacy.display(),
                    self.data_dir.display()
                );
                self.data_dir = legacy.to_path_buf();
                Some(msg)
            }
        }
    }

    pub fn db(&self) -> PathBuf {
        self.data_dir.join("agt.db")
    }
    pub fn logs(&self) -> PathBuf {
        self.logs_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("logs"))
    }

    /// 把舊位置（`data_dir/logs`）的紀錄檔搬到 [`logs`](Self::logs)。daemon 取得 lock 後呼叫，
    /// 這時沒有 Agent 在寫入。搬不動的檔案留在原處（之後的清理也不會處理），回傳要記錄的訊息。
    pub fn migrate_logs(&self) -> Option<String> {
        let old = self.data_dir.join("logs");
        let new = self.logs();
        if agt_git::same_path(&old, &new) || !old.is_dir() {
            return None;
        }
        if let Err(e) = std::fs::create_dir_all(&new) {
            return Some(format!("無法建立紀錄檔目錄 {}：{e}", new.display()));
        }
        let (mut moved, mut failed) = (0, 0);
        for e in std::fs::read_dir(&old).ok()?.flatten() {
            let to = new.join(e.file_name());
            let ok = std::fs::rename(e.path(), &to).is_ok()
                || (std::fs::copy(e.path(), &to).is_ok() && std::fs::remove_file(e.path()).is_ok());
            if ok { moved += 1 } else { failed += 1 }
        }
        let _ = std::fs::remove_dir(&old); // 空了才會成功
        (moved + failed > 0).then(|| {
            format!(
                "已把 {moved} 個紀錄檔從 {} 搬到 {}{}",
                old.display(),
                new.display(),
                if failed > 0 {
                    format!("（{failed} 個無法搬移，留在原處）")
                } else {
                    String::new()
                }
            )
        })
    }
    pub fn lock(&self) -> PathBuf {
        self.data_dir.join("agtd.lock")
    }

    /// 展開開頭的 `~`。
    pub fn expand(&self, p: &str) -> PathBuf {
        if p == "~" {
            self.home.clone()
        } else if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
            self.home.join(rest)
        } else {
            PathBuf::from(p)
        }
    }
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

fn toml_err(file: &Path, text: &str, e: &toml::de::Error) -> String {
    let line = e.span().map(|s| line_of(text, s.start)).unwrap_or(0);
    let msg = e.message().trim().replace('\n', " ");
    format!("{}:{line}：{msg}", file.display())
}

/// 深度合併：`over` 的值覆蓋 `base`；表格逐層合併。
fn merge(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

/// 依名稱決定狀態判斷方式的內建 Agent。`extends` 這些 Agent 又沒寫 `adapter` 時，
/// 自動設成同名的 adapter；否則名稱不同（例如 `claude-advisor`）會被當成一般的自訂 Agent。
const NAMED_ADAPTERS: &[&str] = &["claude", "antigravity", "copilot", "codex", "shell"];

/// 展開 `extends`：沒寫的欄位沿用被繼承的 Agent（可以多層，不可循環）。
/// 必須在 [`inherit_adapter_args`] 之前執行。
fn resolve_extends(value: &mut toml::Value) -> Result<(), String> {
    let Some(agents) = value.get_mut("agents").and_then(|a| a.as_table_mut()) else {
        return Ok(());
    };
    let names: Vec<String> = agents.keys().cloned().collect();
    let mut done = BTreeSet::new();
    for name in names {
        resolve_one(agents, &name, &mut Vec::new(), &mut done)?;
    }
    Ok(())
}

fn resolve_one(
    agents: &mut toml::map::Map<String, toml::Value>,
    name: &str,
    chain: &mut Vec<String>,
    done: &mut BTreeSet<String>,
) -> Result<(), String> {
    if done.contains(name) {
        return Ok(());
    }
    if chain.iter().any(|n| n == name) {
        chain.push(name.to_string());
        return Err(format!(
            "agents.{}.extends 形成循環：{}",
            chain[0],
            chain.join(" → ")
        ));
    }
    let base = agents
        .get(name)
        .and_then(|a| a.get("extends"))
        .and_then(|e| e.as_str())
        .filter(|e| !e.is_empty())
        .map(str::to_string);
    if let Some(base) = base {
        if !agents.contains_key(&base) {
            return Err(format!("agents.{name}.extends 指向不存在的 Agent {base:?}"));
        }
        chain.push(name.to_string());
        resolve_one(agents, &base, chain, done)?;
        chain.pop();
        let inherited = agents
            .get(&base)
            .and_then(|a| a.as_table())
            .cloned()
            .unwrap_or_default();
        if let Some(t) = agents.get_mut(name).and_then(|a| a.as_table_mut()) {
            for (k, v) in inherited {
                if k != "extends" && !t.contains_key(&k) {
                    t.insert(k, v);
                }
            }
            if !t.contains_key("adapter") && NAMED_ADAPTERS.contains(&base.as_str()) {
                t.insert("adapter".into(), toml::Value::String(base));
            }
        }
    }
    done.insert(name.to_string());
    Ok(())
}

/// 設定了 `adapter` 的 Agent（例如 `claude-docker` 沿用 `claude`）沒寫 `new_args` / `resume_args` 時，
/// 沿用該 adapter 對應 Agent 的值；否則每次重新啟動都會開新對話、無法接續。
/// 要關閉接續功能可明確寫 `resume_args = []`。
fn inherit_adapter_args(value: &mut toml::Value) {
    let Some(agents) = value.get_mut("agents").and_then(|a| a.as_table_mut()) else {
        return;
    };
    let mut fills = Vec::new();
    for (name, agent) in agents.iter() {
        let Some(base) = agent.get("adapter").and_then(|a| a.as_str()) else {
            continue;
        };
        if base == name {
            continue;
        }
        let Some(base_agent) = agents.get(base) else {
            continue;
        };
        for key in ["new_args", "resume_args"] {
            if agent.get(key).is_none()
                && let Some(v) = base_agent.get(key)
            {
                fills.push((name.clone(), key, v.clone()));
            }
        }
    }
    for (name, key, v) in fills {
        if let Some(t) = agents.get_mut(&name).and_then(|a| a.as_table_mut()) {
            t.insert(key.to_string(), v);
        }
    }
}

pub fn parse_prefix_key(s: &str) -> Result<(), String> {
    let lower = s.to_ascii_lowercase();
    let key = lower
        .strip_prefix("ctrl-")
        .or_else(|| lower.strip_prefix("ctrl+"))
        .or_else(|| lower.strip_prefix("c-"))
        .ok_or_else(|| format!("prefix_key 必須是 ctrl-<字母>，而不是 {s:?}"))?;
    if key.len() == 1
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || "[]\\^_ ".contains(c))
    {
        Ok(())
    } else {
        Err(format!("prefix_key 必須是 ctrl-<字母>，而不是 {s:?}"))
    }
}

impl Config {
    /// 載入設定。檔案不存在時使用預設值；驗證失敗時回傳「檔案:行號：原因」。
    pub fn load(file: &Path) -> Result<Config, String> {
        let mut value: toml::Value = toml::from_str(DEFAULT_TOML).expect("內建預設設定必須合法");
        if let Ok(text) = std::fs::read_to_string(file) {
            toml::from_str::<PartialConfig>(&text).map_err(|e| toml_err(file, &text, &e))?;
            let user: toml::Value = toml::from_str(&text).map_err(|e| toml_err(file, &text, &e))?;
            merge(&mut value, user);
        }
        resolve_extends(&mut value).map_err(|e| format!("{}：{e}", file.display()))?;
        inherit_adapter_args(&mut value);
        let cfg: Config = value
            .try_into()
            .map_err(|e: toml::de::Error| format!("{}：{}", file.display(), e.message()))?;
        cfg.validate()
            .map_err(|e| format!("{}：{e}", file.display()))?;
        Ok(cfg)
    }

    pub fn defaults() -> Config {
        let v: toml::Value = toml::from_str(DEFAULT_TOML).unwrap();
        v.try_into().unwrap()
    }

    pub fn validate(&self) -> Result<(), String> {
        parse_prefix_key(&self.general.prefix_key)?;
        if !matches!(
            self.general.merge_strategy.as_str(),
            "ask" | "merge" | "squash" | "rebase"
        ) {
            return Err(format!(
                "general.merge_strategy 必須是 ask、merge、squash 或 rebase，而不是 {:?}",
                self.general.merge_strategy
            ));
        }
        if !matches!(self.general.osc52.as_str(), "off" | "ask" | "on") {
            return Err(format!(
                "general.osc52 必須是 off、ask 或 on，而不是 {:?}",
                self.general.osc52
            ));
        }
        if self.general.sidebar_width < 10 {
            return Err("general.sidebar_width 至少要 10".into());
        }
        for (name, a) in &self.agents {
            a.validate(name)?;
        }
        Ok(())
    }

    pub fn ui(&self) -> agt_proto::UiConfig {
        agt_proto::UiConfig {
            prefix_key: self.general.prefix_key.clone(),
            sidebar_width: self.general.sidebar_width,
            merge_strategy: self.general.merge_strategy.clone(),
            osc52: self.general.osc52.clone(),
            agents: self.agents.keys().cloned().collect(),
            auto_commit_agents: self
                .agents
                .iter()
                .filter(|(_, a)| a.auto_commit)
                .map(|(n, _)| n.clone())
                .collect(),
        }
    }

    /// 取得 repo 專屬設定：全域 `[repos."<路徑>"]` 之後再套用 repo 內的 `.agt.toml`。
    pub fn repo(&self, paths: &Paths, root: &Path) -> Result<RepoConfig, String> {
        let mut out = RepoConfig::default();
        for (k, v) in &self.repos {
            let p = paths.expand(k);
            if agt_git::same_path(&p, root) {
                out = v.clone();
            }
        }
        let local = root.join(".agt.toml");
        if let Ok(text) = std::fs::read_to_string(&local) {
            let r: RepoConfig = toml::from_str(&text).map_err(|e| toml_err(&local, &text, &e))?;
            if !r.copy_files.is_empty() {
                out.copy_files = r.copy_files;
            }
            if r.setup.is_some() {
                out.setup = r.setup;
            }
            if r.base_branch.is_some() {
                out.base_branch = r.base_branch;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let c = Config::defaults();
        c.validate().unwrap();
        assert_eq!(c.general.sidebar_width, 26);
        assert_eq!(c.agents["claude"].status_source, "hooks");
        assert!(!c.agents["shell"].worktree);
        // copilot 用分頁自己的對話 ID 接續（不是 --continue 的「最近一次」）
        let sid = ["--session-id", "{conversation}"];
        assert_eq!(c.agents["copilot"].new_args, sid);
        assert_eq!(c.agents["copilot"].resume_args, sid);
    }

    #[test]
    fn copilot_docker_inherits_session_args() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(
            &f,
            "[agents.copilot-docker]\ncommand = \"docker\"\nadapter = \"copilot\"\n",
        )
        .unwrap();
        let c = Config::load(&f).unwrap();
        let a = &c.agents["copilot-docker"];
        assert_eq!(a.new_args, ["--session-id", "{conversation}"]);
        assert_eq!(a.resume_args, ["--session-id", "{conversation}"]);
    }

    #[test]
    fn user_overrides_merge() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(
            &f,
            "[general]\nsidebar_width = 30\n[agents.claude]\nargs = [\"--verbose\"]\n",
        )
        .unwrap();
        let c = Config::load(&f).unwrap();
        assert_eq!(c.general.sidebar_width, 30);
        assert_eq!(c.general.idle_shutdown_secs, 600);
        assert_eq!(c.agents["claude"].args, vec!["--verbose"]);
        assert_eq!(c.agents["claude"].command, "claude");
    }

    #[test]
    fn migrates_legacy_data_dir() {
        let d = tempfile::tempdir().unwrap();
        let legacy = d.path().join("local").join("agt");
        std::fs::create_dir_all(legacy.join("logs")).unwrap();
        std::fs::write(legacy.join("agt.db"), "db").unwrap();
        std::fs::write(legacy.join("agtd.lock"), "").unwrap();
        std::fs::write(legacy.join("logs").join("s.log"), "log").unwrap();
        let new = d.path().join("roaming").join("agt").join("data");
        let mut p = Paths {
            config_file: d.path().join("roaming/agt/config.toml"),
            data_dir: new.clone(),
            home: d.path().to_path_buf(),
            logs_dir: None,
        };
        let msg = p.migrate_from(&legacy).unwrap();
        assert!(msg.contains("已把資料"), "{msg}");
        assert_eq!(p.data_dir, new);
        assert_eq!(std::fs::read_to_string(new.join("agt.db")).unwrap(), "db");
        assert!(new.join("logs").join("s.log").exists());
        assert!(!legacy.exists());
        // 新位置已存在：不再搬移
        std::fs::create_dir_all(&legacy).unwrap();
        assert!(p.migrate_from(&legacy).is_none());
        assert_eq!(p.data_dir, new);
    }

    #[test]
    fn logs_move_to_their_own_dir_and_do_not_look_like_legacy_data() {
        let d = tempfile::tempdir().unwrap();
        let data = d.path().join("roaming").join("data");
        let local = d.path().join("local").join("agt");
        std::fs::create_dir_all(data.join("logs")).unwrap();
        std::fs::write(data.join("logs").join("a.log"), "a").unwrap();
        std::fs::write(data.join("logs").join("a.log.1"), "b").unwrap();
        let p = Paths {
            config_file: d.path().join("c.toml"),
            data_dir: data.clone(),
            home: d.path().to_path_buf(),
            logs_dir: Some(local.join("logs")),
        };
        assert!(p.migrate_logs().unwrap().contains("2 個"));
        assert_eq!(
            std::fs::read_to_string(local.join("logs/a.log")).unwrap(),
            "a"
        );
        assert!(!data.join("logs").exists());
        assert!(p.migrate_logs().is_none(), "已經搬過");
        // 只有 logs\ 的 %LOCALAPPDATA%\agt 不是舊版資料目錄，不能被搬去當資料目錄
        let mut fresh = Paths {
            data_dir: d.path().join("new-data"),
            ..p.clone()
        };
        assert!(fresh.migrate_from(&local).is_none());
        assert_eq!(fresh.data_dir, d.path().join("new-data"));
        // 沒有另外指定時沿用 data_dir/logs
        let plain = Paths {
            logs_dir: None,
            ..p
        };
        assert_eq!(plain.logs(), data.join("logs"));
        assert!(plain.migrate_logs().is_none());
    }

    #[test]
    fn keeps_legacy_dir_while_old_daemon_runs() {
        let d = tempfile::tempdir().unwrap();
        let legacy = d.path().join("old");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("agt.db"), "db").unwrap();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(legacy.join("agtd.lock"))
            .unwrap();
        lock.try_lock().unwrap(); // 模擬舊版 daemon 執行中
        let mut p = Paths {
            config_file: d.path().join("c.toml"),
            data_dir: d.path().join("new"),
            home: d.path().to_path_buf(),
            logs_dir: None,
        };
        let msg = p.migrate_from(&legacy).unwrap();
        assert!(msg.contains("沿用舊位置"), "{msg}");
        assert_eq!(p.data_dir, legacy);
        assert!(!d.path().join("new").exists());
    }

    #[test]
    fn docker_agent_settings() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(
            &f,
            "[agents.claude-docker]\ncommand = \"docker\"\nadapter = \"claude\"\nstatus_source = \"pattern\"\nargs = [\"run\", \"{cwd}:/work\"]\ncleanup = [\"docker\", \"rm\", \"-f\", \"agt-{session}\"]\n",
        )
        .unwrap();
        let c = Config::load(&f).unwrap();
        let a = &c.agents["claude-docker"];
        assert_eq!(a.adapter, "claude");
        assert_eq!(a.cleanup.len(), 4);
        assert!(!a.auto_commit);
        assert!(a.update.is_empty());
        assert!(c.ui().agents.contains(&"claude-docker".to_string()));
        // 沒寫 new_args / resume_args 時沿用 adapter（claude）的，重新啟動才能接續對話
        assert_eq!(a.new_args, ["--session-id", "{conversation}"]);
        assert_eq!(a.resume_args, ["--resume", "{conversation}"]);
        // 明確寫成空陣列 = 不接續
        std::fs::write(
            &f,
            "[agents.claude-docker]\ncommand = \"docker\"\nadapter = \"claude\"\nresume_args = []\n",
        )
        .unwrap();
        let c = Config::load(&f).unwrap();
        assert!(c.agents["claude-docker"].resume_args.is_empty());
        assert_eq!(c.agents["claude-docker"].new_args.len(), 2);
        std::fs::write(&f, "[agents.x]\nadapter = \"nope\"\n").unwrap();
        assert!(Config::load(&f).unwrap_err().contains("adapter"));
    }

    #[test]
    fn extends_reuses_base_agent() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(
            &f,
            r#"
[agents.claude-sbx]
command = "sbx"
adapter = "claude"
status_source = "pattern"
args = ["run", "--name", "agt-{session}", "claude", "{cwd}", "--"]
restart_args = ["run", "--name", "agt-{session}", "--"]
on_close = ["sbx", "rm", "-f", "agt-{session}"]
auto_commit = true

[agents.claude-sbx-advisor]
extends = "claude-sbx"
extra_args = ["--model", "sonnet", "--advisor", "opus"]

[agents.claude-advisor]
extends = "claude"
extra_args = ["--model", "sonnet", "--advisor", "opus"]

[agents.deep]
extends = "claude-sbx-advisor"
auto_commit = false
"#,
        )
        .unwrap();
        let c = Config::load(&f).unwrap();
        let base = &c.agents["claude-sbx"];
        let a = &c.agents["claude-sbx-advisor"];
        assert_eq!(a.command, "sbx");
        assert_eq!(a.args, base.args);
        assert_eq!(a.restart_args, base.restart_args);
        assert_eq!(a.on_close, base.on_close);
        assert!(a.auto_commit);
        assert_eq!(a.adapter, "claude");
        assert_eq!(a.extra_args, ["--model", "sonnet", "--advisor", "opus"]);
        // adapter 的 new_args / resume_args 也一路沿用下來
        assert_eq!(a.resume_args, ["--resume", "{conversation}"]);
        assert!(base.extra_args.is_empty(), "不能反過來改到被繼承的 Agent");

        // 繼承內建的 claude：沒寫 adapter 也要沿用 Claude 的 hooks 與狀態判斷
        let h = &c.agents["claude-advisor"];
        assert_eq!(h.command, "claude");
        assert_eq!(h.adapter, "claude");
        assert_eq!(h.status_source, "hooks");
        assert_eq!(h.new_args, ["--session-id", "{conversation}"]);

        // 多層：自己寫的欄位優先
        let deep = &c.agents["deep"];
        assert_eq!(deep.args, base.args);
        assert_eq!(deep.extra_args, a.extra_args);
        assert!(!deep.auto_commit);
        assert!(c.ui().agents.contains(&"claude-sbx-advisor".to_string()));
    }

    #[test]
    fn extends_errors() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(&f, "[agents.a]\nextends = \"nope\"\n").unwrap();
        let e = Config::load(&f).unwrap_err();
        assert!(e.contains("agents.a.extends") && e.contains("nope"), "{e}");
        std::fs::write(
            &f,
            "[agents.a]\nextends = \"b\"\n[agents.b]\nextends = \"a\"\n",
        )
        .unwrap();
        assert!(Config::load(&f).unwrap_err().contains("循環"));
        std::fs::write(&f, "[agents.a]\nextends = \"a\"\n").unwrap();
        assert!(Config::load(&f).unwrap_err().contains("循環"));
    }

    #[test]
    fn errors_have_line_numbers() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(&f, "[general]\nnotify = true\nsidebar_width = \"wide\"\n").unwrap();
        let e = Config::load(&f).unwrap_err();
        assert!(e.contains("config.toml:3"), "{e}");
        std::fs::write(&f, "[general]\nmerge_strategy = \"yolo\"\n").unwrap();
        assert!(Config::load(&f).unwrap_err().contains("merge_strategy"));
        std::fs::write(&f, "[agents.x]\nwaiting_patterns = ['(']\n").unwrap();
        assert!(Config::load(&f).unwrap_err().contains("正規表達式"));
    }

    #[test]
    fn repo_config_layers() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let paths = Paths {
            config_file: d.path().join("c.toml"),
            data_dir: d.path().join("data"),
            home: d.path().to_path_buf(),
            logs_dir: None,
        };
        let mut c = Config::defaults();
        c.repos.insert(
            "~/proj".into(),
            RepoConfig {
                copy_files: vec![".env".into()],
                setup: Some("make".into()),
                base_branch: None,
            },
        );
        std::fs::write(root.join(".agt.toml"), "setup = \"npm install\"\n").unwrap();
        let r = c.repo(&paths, &root).unwrap();
        assert_eq!(r.copy_files, vec![".env"]);
        assert_eq!(r.setup.as_deref(), Some("npm install"));
    }
}
