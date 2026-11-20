//! `agtd --check-config`（`agt config check`）：不啟動 daemon，檢查設定檔。
//!
//! 錯誤（載入失敗）→ exit 1；警告（設定合法但很可能不是你要的）→ exit 0 並列出。
//! 只檢查「使用者設定檔裡寫到的 Agent」，內建預設裡沒安裝的 Agent 不會一直警告。

use crate::config::{Config, Paths};
use agt_agents::AgentSettings;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Agent 指令參數裡可用的變數。
const VARS: &[&str] = &["cwd", "session", "conversation", "gitdir"];
/// `update` 在沒有分頁的情境下執行，這些變數沒有值。
const UPDATE_VARS: &[&str] = &[];

/// 找出字串裡 `{name}` 形式、但不是已知變數的名稱（`{{.X}}` 這類 Go template 不算）。
fn unknown_vars(s: &str, allowed: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{'
            && let Some(len) = s[i + 1..].find('}')
        {
            let name = &s[i + 1..i + 1 + len];
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && !allowed.contains(&name)
            {
                out.push(name.to_string());
            }
            i += 1 + len;
        }
        i += 1;
    }
    out
}

fn agent_lists(a: &AgentSettings) -> Vec<(&'static str, &Vec<String>, &'static [&'static str])> {
    vec![
        ("args", &a.args, VARS),
        ("new_args", &a.new_args, VARS),
        ("resume_args", &a.resume_args, VARS),
        ("restart_args", &a.restart_args, VARS),
        ("extra_args", &a.extra_args, VARS),
        ("cleanup", &a.cleanup, VARS),
        ("on_close", &a.on_close, VARS),
        ("update", &a.update, UPDATE_VARS),
    ]
}

/// 對已通過驗證的設定做「合法但可疑」的檢查。
/// `mine`：使用者設定檔裡寫到的 Agent 名稱；`exists`：指令是否找得到（測試時可替換）。
pub fn lint(
    cfg: &Config,
    mine: &BTreeSet<String>,
    paths: &Paths,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut w = Vec::new();
    for (name, a) in &cfg.agents {
        if !mine.contains(name) {
            continue;
        }
        let eff = if a.adapter.is_empty() {
            name.as_str()
        } else {
            a.adapter.as_str()
        };
        if !a.command.is_empty() && !exists(&a.command) {
            w.push(format!(
                "agents.{name}.command：找不到 {:?}（不在 PATH 中，這個 Agent 啟動時會失敗）",
                a.command
            ));
        }
        for (field, list, allowed) in agent_lists(a) {
            for s in list {
                for v in unknown_vars(s, allowed) {
                    if VARS.contains(&v.as_str()) {
                        w.push(format!(
                            "agents.{name}.{field}：{{{v}}} 在 {field} 裡不能用（update 在沒有分頁時執行），會原樣傳給指令"
                        ));
                    } else {
                        w.push(format!(
                            "agents.{name}.{field}：{{{v}}} 不是已知的變數（可用：{{cwd}}、{{session}}、{{conversation}}、{{gitdir}}），會原樣傳給指令"
                        ));
                    }
                }
            }
        }
        for (k, v) in &a.env {
            for var in unknown_vars(v, VARS) {
                w.push(format!("agents.{name}.env.{k}：{{{var}}} 不是已知的變數"));
            }
        }
        if a.status_source == "hooks" && eff != "claude" {
            w.push(format!(
                "agents.{name}.status_source = \"hooks\"，但 adapter 是 {eff:?}：只有 claude 會呼叫 `agt notify`，狀態不會更新"
            ));
        }
        if !a.resume_args.is_empty()
            && a.new_args.is_empty()
            && a.resume_args.iter().any(|s| s.contains("{conversation}"))
        {
            w.push(format!(
                "agents.{name}：resume_args 用了 {{conversation}}，但沒有 new_args 指定這個對話 ID，接續時會找不到對話"
            ));
        }
    }
    for key in cfg.repos.keys() {
        let p = paths.expand(key);
        if !p.is_dir() {
            w.push(format!("repos.{key:?}：目錄 {} 不存在", p.display()));
        }
    }
    for (key, r) in &cfg.repos {
        for f in &r.copy_files {
            if escapes_repo(f) {
                w.push(format!(
                    "repos.{key:?}.copy_files：{f:?} 必須是 repo 內的相對路徑（不能是絕對路徑或含 ..）"
                ));
            }
        }
    }
    w
}

/// `copy_files` 的項目是否可能指到 repo 外面：絕對路徑、以分隔符號開頭、帶磁碟機代號（Windows）或含 `..`。
/// 不能只用 `is_absolute()`：Windows 上 `/etc/passwd` 沒有磁碟機代號，不算絕對路徑，
/// 但接到 repo 路徑後面仍會被解析成目前磁碟機的根目錄。
fn escapes_repo(f: &str) -> bool {
    use std::path::Component;
    let p = Path::new(f);
    p.is_absolute()
        || f.starts_with(['/', '\\'])
        || p.components()
            .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
        || f.split(['/', '\\']).any(|c| c == "..")
}

/// 使用者設定檔裡 `[agents.<名稱>]` 的名稱。
fn user_agents(file: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("agents")?
                .as_table()
                .map(|a| a.keys().cloned().collect())
        })
        .unwrap_or_default()
}

/// 命令列：`--check-config [--file <路徑>] [--repo <目錄>]`。回傳 exit code。
pub fn run(args: &[String]) -> i32 {
    let paths = Paths::detect();
    let mut file: PathBuf = paths.config_file.clone();
    let mut repo: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--file" => match it.next() {
                Some(v) => file = PathBuf::from(v),
                None => return usage("--file 需要路徑"),
            },
            "--repo" => match it.next() {
                Some(v) => repo = Some(PathBuf::from(v)),
                None => return usage("--repo 需要目錄"),
            },
            _ => {}
        }
    }
    if file.exists() {
        println!("設定檔：{}", file.display());
    } else {
        println!("設定檔 {} 不存在，使用內建預設值", file.display());
    }
    let cfg = match Config::load(&file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("錯誤：{e}");
            return 1;
        }
    };
    let mut warnings = lint(&cfg, &user_agents(&file), &paths, &|c| {
        agt_core::platform::which(c).is_some()
    });
    if let Some(root) = repo {
        println!("repo：{}", root.display());
        match cfg.repo(&paths, &root) {
            Ok(r) => {
                for f in &r.copy_files {
                    if escapes_repo(f) {
                        warnings.push(format!("copy_files：{f:?} 必須是 repo 內的相對路徑"));
                    }
                }
            }
            Err(e) => {
                eprintln!("錯誤：{e}");
                return 1;
            }
        }
    }
    println!(
        "worktree 位置：{}",
        paths.expand(&cfg.general.worktree_root).display()
    );
    println!(
        "Agent：{}",
        cfg.agents.keys().cloned().collect::<Vec<_>>().join("、")
    );
    // `extends` 展開後的實際值，auto_commit 沒有生效時從這裡看得出來
    println!("auto_commit（收尾前自動 commit）：");
    for line in auto_commit_lines(&cfg) {
        println!("  {line}");
    }
    if warnings.is_empty() {
        println!("沒有發現問題");
    } else {
        println!("\n{} 個警告：", warnings.len());
        for w in &warnings {
            println!("  ! {w}");
        }
    }
    0
}

/// 每個 Agent 的 `auto_commit`（`extends` 展開後），例如 `claude-docker  on`。
fn auto_commit_lines(cfg: &Config) -> Vec<String> {
    let w = cfg.agents.keys().map(|n| n.chars().count()).max().unwrap_or(0);
    cfg.agents
        .iter()
        .map(|(n, a)| {
            format!("{n:<w$}  {}", if a.auto_commit { "on" } else { "off" })
        })
        .collect()
}

fn usage(msg: &str) -> i32 {
    eprintln!("錯誤：{msg}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(toml: &str) -> (Config, BTreeSet<String>) {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(&f, toml).unwrap();
        (Config::load(&f).unwrap(), user_agents(&f))
    }

    fn paths() -> Paths {
        Paths {
            config_file: PathBuf::from("/nonexistent/config.toml"),
            data_dir: PathBuf::from("/nonexistent/data"),
            home: PathBuf::from("/nonexistent"),
            logs_dir: None,
        }
    }

    #[test]
    fn unknown_variable_scanner() {
        assert_eq!(
            unknown_vars("{cwd}:/w {session}", VARS),
            Vec::<String>::new()
        );
        assert_eq!(unknown_vars("-v {cwdd}:/w", VARS), ["cwdd"]);
        assert_eq!(
            unknown_vars("{{.Server.Version}}", VARS),
            Vec::<String>::new()
        );
        assert_eq!(unknown_vars("{}", VARS), Vec::<String>::new());
        assert_eq!(unknown_vars("a{b", VARS), Vec::<String>::new());
        assert_eq!(unknown_vars("{cwd}", UPDATE_VARS), ["cwd"]);
    }

    #[test]
    fn default_config_has_no_warnings() {
        let (c, mine) = load("");
        assert!(mine.is_empty());
        assert!(lint(&c, &mine, &paths(), &|_| false).is_empty());
    }

    #[test]
    fn warns_about_missing_command_and_typos() {
        let (c, mine) = load(
            r#"
[agents.mine]
command = "no-such-agent"
args = ["--dir", "{cwdd}", "{session}"]
update = ["x", "{cwd}"]
"#,
        );
        let w = lint(&c, &mine, &paths(), &|_| false);
        let all = w.join("\n");
        assert!(all.contains("agents.mine.command"), "{all}");
        assert!(all.contains("{cwdd}"), "{all}");
        assert!(
            all.contains("agents.mine.update：{cwd} 在 update 裡不能用"),
            "{all}"
        );
        assert!(!all.contains("args：{session}"), "已知變數不應警告：{all}");
        // 指令存在時不警告
        let w = lint(&c, &mine, &paths(), &|_| true);
        assert!(!w.join("\n").contains("agents.mine.command"));
    }

    #[test]
    fn only_checks_agents_in_user_config() {
        // 預設的 claude 等 Agent 沒安裝時不警告
        let (c, mine) = load("[agents.mine]\ncommand = \"ok\"\n");
        let w = lint(&c, &mine, &paths(), &|cmd| cmd == "ok");
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn hooks_with_non_claude_adapter_and_resume_without_new() {
        let (c, mine) = load(
            r#"
[agents.a]
command = "x"
status_source = "hooks"
adapter = "codex"
resume_args = ["--session", "{conversation}"]
"#,
        );
        let all = lint(&c, &mine, &paths(), &|_| true).join("\n");
        assert!(all.contains("status_source"), "{all}");
        assert!(all.contains("resume_args"), "{all}");
    }

    #[test]
    fn auto_commit_lines_follow_extends() {
        let (c, _) = load(
            r#"
[agents.base]
command = "x"
auto_commit = true

[agents.child]
extends = "base"

[agents.off]
command = "y"
"#,
        );
        let lines = auto_commit_lines(&c);
        let find = |n: &str| lines.iter().find(|l| l.starts_with(n)).cloned().unwrap();
        assert!(find("base").ends_with("on"), "{lines:?}");
        assert!(find("child").ends_with("on"), "extends 要沿用：{lines:?}");
        assert!(find("off").ends_with("off"), "{lines:?}");
        assert!(c.ui().auto_commit_agents.contains(&"child".to_string()));
        assert!(!c.ui().auto_commit_agents.contains(&"off".to_string()));
    }

    #[test]
    fn escapes_repo_rules() {
        for ok in [".env", "config/local.toml", "a..b", "dir/.hidden"] {
            assert!(!escapes_repo(ok), "{ok}");
        }
        for bad in ["/etc/passwd", "\\x", "../x", "a/../../x", "a\\..\\x"] {
            assert!(escapes_repo(bad), "{bad}");
        }
        // 帶磁碟機代號的路徑只在 Windows 上才有意義（Linux 上 `C:` 只是檔名的一部分）
        if cfg!(windows) {
            for bad in ["C:\\Windows", "C:foo", "\\\\server\\share"] {
                assert!(escapes_repo(bad), "{bad}");
            }
        }
    }

    #[test]
    fn repo_paths_and_copy_files() {
        let (c, mine) = load(
            r#"
[repos."/no/such/dir"]
copy_files = [".env", "../secret", "/etc/passwd"]
"#,
        );
        let all = lint(&c, &mine, &paths(), &|_| true).join("\n");
        assert!(all.contains("不存在"), "{all}");
        assert!(all.contains("../secret"), "{all}");
        assert!(all.contains("/etc/passwd"), "{all}");
        assert!(!all.contains("\".env\"：\".env\""), "{all}");
    }
}
