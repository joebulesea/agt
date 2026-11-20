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
    /// 哪些狀態要發系統通知（`notify = true` 時才有作用）：waiting | done | error | exited
    pub notify_on: Vec<String>,
    pub idle_shutdown_secs: u64,
    pub scrollback_lines: usize,
    pub merge_strategy: String,
    pub log_output: bool,
    /// 分頁中的程式以 OSC 52 寫入剪貼簿時：off | ask | on
    pub osc52: String,
    /// TUI 配色：dark | light | colorblind
    pub theme: String,
    /// 收尾（merge / pr）前要求確認的額外路徑，加在內建清單之外。
    pub sensitive_paths: Vec<String>,
}

/// `theme` 可用的值（與 agt-tui 的主題名稱一致）。
pub const THEMES: [&str; 3] = ["dark", "light", "colorblind"];

/// `notify_on` 可用的值，對應 `AgentStatus::as_str()`。
pub const NOTIFY_ON_VALUES: [&str; 4] = ["waiting", "done", "error", "exited"];

impl General {
    /// 分頁變成 `to` 時是否要發系統通知。
    pub fn should_notify(&self, to: agt_proto::AgentStatus) -> bool {
        self.notify && self.notify_on.iter().any(|v| v == to.as_str())
    }
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
    /// 覆寫主題中個別顏色，例如 `cyan = "#00afaf"`、`yellow = "208"`
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
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
    #[serde(default)]
    colors: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct PartialGeneral {
    prefix_key: Option<String>,
    sidebar_width: Option<u16>,
    worktree_root: Option<String>,
    notify: Option<bool>,
    notify_on: Option<Vec<String>>,
    idle_shutdown_secs: Option<u64>,
    scrollback_lines: Option<usize>,
    merge_strategy: Option<String>,
    log_output: Option<bool>,
    osc52: Option<String>,
    theme: Option<String>,
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
        let v: toml::Value = toml::from_str(DEFAULT_TOML).expect("內建預設設定必須合法");
        v.try_into().expect("內建預設設定必須符合 Config 結構")
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
        if !THEMES.contains(&self.general.theme.as_str()) {
            return Err(format!(
                "general.theme 必須是 dark、light 或 colorblind，而不是 {:?}",
                self.general.theme
            ));
        }
        for (k, v) in &self.colors {
            if !agt_proto::colors::COLOR_KEYS.contains(&k.as_str()) {
                return Err(format!(
                    "colors.{k} 不是可覆寫的名稱（可用：{}）",
                    agt_proto::colors::COLOR_KEYS.join("、")
                ));
            }
            agt_proto::colors::parse_color(v).map_err(|e| format!("colors.{k}：{e}"))?;
        }
        for v in &self.general.notify_on {
            if !NOTIFY_ON_VALUES.contains(&v.as_str()) {
                return Err(format!(
                    "general.notify_on 只能包含 waiting、done、error、exited，而不是 {v:?}"
                ));
            }
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
            theme: self.general.theme.clone(),
            colors: self.colors.clone(),
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
mod tests;
