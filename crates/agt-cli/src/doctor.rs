//! `agt doctor`：一次檢查 agt 的執行環境，並對每個問題給出下一步。
//!
//! 只讀取、不修改任何東西（除了在資料目錄建立再刪除一個測試檔）。有「失敗」項目時 exit 1。

use crate::setup::{Docker, Sbx, capture, detect, on_path};
use agt_proto::client::ClientError;
use agt_proto::transport;
use agt_proto::{ClientMsg, ReplyBody, codes};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Level {
    Ok,
    /// 只是資訊（例如沒安裝的選用工具）
    Info,
    Warn,
    Fail,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

struct Item {
    level: Level,
    text: String,
    hint: Option<String>,
}

#[derive(Default)]
struct Report(Vec<Item>);

impl Report {
    fn add(&mut self, level: Level, text: impl Into<String>, hint: Option<&str>) {
        self.0.push(Item {
            level,
            text: text.into(),
            hint: hint.map(str::to_string),
        });
    }
    fn ok(&mut self, text: impl Into<String>) {
        self.add(Level::Ok, text, None);
    }
    fn info(&mut self, text: impl Into<String>) {
        self.add(Level::Info, text, None);
    }
    fn warn(&mut self, text: impl Into<String>, hint: &str) {
        self.add(Level::Warn, text, Some(hint));
    }
    fn fail(&mut self, text: impl Into<String>, hint: &str) {
        self.add(Level::Fail, text, Some(hint));
    }
    fn count(&self, l: Level) -> usize {
        self.0.iter().filter(|i| i.level == l).count()
    }
}

/// `agt doctor --json` 的輸出：`{ok, failures, warnings, items: [{level, text, hint}]}`。
/// `level` 是 ok | info | warn | fail；`ok` 在沒有 fail 時為 true。
fn to_json(r: &Report) -> serde_json::Value {
    let items: Vec<_> =
        r.0.iter()
            .map(|i| {
                serde_json::json!({
                    "level": i.level.as_str(),
                    "text": i.text,
                    "hint": i.hint,
                })
            })
            .collect();
    serde_json::json!({
        "ok": r.count(Level::Fail) == 0,
        "failures": r.count(Level::Fail),
        "warnings": r.count(Level::Warn),
        "items": items,
    })
}

/// 從 `git --version` 的輸出取出 (major, minor)。
fn git_version(s: &str) -> Option<(u32, u32)> {
    let v = s.split_whitespace().next()?;
    let mut it = v.split('.');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// 日誌最後 `n` 行中的 ERROR / panic 行（取最新一行）與數量。
fn recent_errors(log: &str, n: usize) -> (usize, Option<String>) {
    let lines: Vec<&str> = log.lines().rev().take(n).collect();
    let errs: Vec<&&str> = lines
        .iter()
        .filter(|l| l.contains(" ERROR ") || l.contains("panic"))
        .collect();
    (errs.len(), errs.first().map(|l| l.trim().to_string()))
}

fn check_binaries(r: &mut Report) {
    let me = std::env::current_exe().unwrap_or_default();
    r.ok(format!(
        "agt {}（{}）",
        env!("CARGO_PKG_VERSION"),
        me.display()
    ));
    // 舊版有獨立的 agtd；已併入 agt（`agt daemon run`），留著的檔案不再使用
    let legacy = transport::sibling_exe("agtd");
    if legacy.is_file() {
        r.warn(
            format!("舊版的 agtd 還在（{}），已不再使用", legacy.display()),
            "可以刪除；agt update 或重新安裝時也會一併刪除",
        );
    }
}

async fn check_daemon(r: &mut Report) {
    match crate::connect_kind("cli").await {
        Ok(conn) => {
            if let ReplyBody::Hello {
                daemon_version,
                config_error,
                ..
            } = &conn.hello
            {
                if daemon_version != env!("CARGO_PKG_VERSION") {
                    r.warn(
                        format!(
                            "daemon 是 {daemon_version}，agt 是 {}",
                            env!("CARGO_PKG_VERSION")
                        ),
                        "執行 agt daemon restart（會結束所有 Agent，分頁保留）",
                    );
                }
                if config_error.is_some() {
                    // 錯誤原因由下面的「設定檔」檢查顯示，這裡不重複
                    r.fail(
                        "daemon 沒有使用你的設定檔（載入失敗，目前是預設值）",
                        "修正設定檔後 daemon 會自動重新載入；仍然不行就 agt daemon restart",
                    );
                }
            }
            match conn.client.request(ClientMsg::DaemonStatus).await {
                Ok(ReplyBody::Daemon(i)) => r.ok(format!(
                    "daemon 執行中（pid {}，版本 {}；{} 個分頁，{} 個 Agent 執行中）",
                    i.pid, i.version, i.sessions, i.running
                )),
                _ => r.warn("daemon 有回應，但查詢狀態失敗", "agt daemon restart"),
            }
        }
        Err(ClientError::Remote { code, message }) if code == codes::PROTO_MISMATCH => r.fail(
            format!("daemon 的通訊協定版本不同：{message}"),
            "執行 agt daemon restart（會結束所有 Agent，分頁保留）",
        ),
        Err(ClientError::Remote { message, .. }) => r.fail(
            format!("daemon 回應錯誤：{message}"),
            "查看 agtd.log；必要時 agt daemon restart",
        ),
        Err(_) => r.info("daemon 沒有在執行（需要時會自動啟動）"),
    }
}

fn check_config(r: &mut Report) {
    let Ok(exe) = std::env::current_exe() else {
        r.warn("無法執行設定檢查", "找不到目前的執行檔位置");
        return;
    };
    let Ok(out) = std::process::Command::new(&exe)
        .args(["config", "check"])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        r.warn("無法執行設定檢查", "確認 agt 可執行");
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        let msg = stderr
            .lines()
            .find(|l| l.starts_with("錯誤："))
            .unwrap_or("設定檔無法載入")
            .trim_start_matches("錯誤：");
        r.fail(
            format!("設定檔有錯誤：{msg}"),
            "修正後再執行 agt config check 確認",
        );
        return;
    }
    let warns: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("  ! "))
        .collect();
    if warns.is_empty() {
        r.ok("設定檔沒有問題");
    } else {
        for w in warns {
            r.warn(format!("設定：{w}"), "agt config check 可以重新檢查");
        }
    }
}

fn check_data(r: &mut Report) {
    let d = crate::uninstall::dirs();
    if let Err(e) = std::fs::create_dir_all(&d.data) {
        r.fail(
            format!("資料目錄 {} 無法建立：{e}", d.data.display()),
            "檢查資料夾權限，或用 AGT_HOME 指定其他位置",
        );
        return;
    }
    let probe = d.data.join(".doctor-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            r.ok(format!("資料目錄可寫入（{}）", d.data.display()));
        }
        Err(e) => r.fail(
            format!("資料目錄 {} 無法寫入：{e}", d.data.display()),
            "檢查資料夾權限、磁碟空間",
        ),
    }
    if let Ok(log) = std::fs::read_to_string(d.data.join("agtd.log")) {
        // 只看最後 200 行：舊的錯誤不代表現在還有問題
        let (n, last) = recent_errors(&log, 200);
        if n > 0 {
            r.warn(
                format!(
                    "agtd.log 最近 200 行有 {n} 筆錯誤，最新一筆：{}",
                    last.unwrap_or_default()
                ),
                &format!("完整紀錄：{}", d.data.join("agtd.log").display()),
            );
        }
    }
    let wt: &Path = &d.worktrees;
    if wt.exists() {
        r.ok(format!("worktree 位置：{}", wt.display()));
    } else {
        r.info(format!(
            "worktree 位置：{}（還沒有建立過分頁）",
            wt.display()
        ));
    }
}

fn check_tools(r: &mut Report) {
    let env = detect();
    match &env.git {
        None => r.fail(
            "找不到 git",
            "安裝 git（agt setup 可以協助），所有分頁都依賴它",
        ),
        Some(v) => {
            match git_version(v) {
                Some(ver) if ver < (2, 25) => r.warn(
                    format!("git {v} 偏舊"),
                    "建議 2.25 以上（worktree 功能在舊版有已知問題）",
                ),
                _ => r.ok(format!("git {v}")),
            }
            let cfg = |k: &str| {
                capture("git", &["config", "--get", k])
                    .filter(|(ok, o)| *ok && !o.is_empty())
                    .is_some()
            };
            if !(cfg("user.name") && cfg("user.email")) {
                r.warn(
                    "git 沒有設定 user.name / user.email",
                    "git config --global user.name \"你的名字\"；user.email 同理（auto_commit 與收尾 commit 需要）",
                );
            }
        }
    }
    // gh：收尾選 pr 才需要
    if on_path("gh") {
        match capture("gh", &["auth", "status"]) {
            Some((true, _)) => r.ok("GitHub CLI（gh）已登入"),
            _ => r.warn(
                "GitHub CLI（gh）沒有登入",
                "執行 gh auth login（收尾選 pr 需要）",
            ),
        }
    } else {
        r.info("沒有安裝 GitHub CLI（gh）：收尾選 pr 時只會 commit，不會建立 PR");
    }
    match &env.docker {
        Docker::Running(v) => r.ok(format!("Docker {v}")),
        Docker::NotRunning => r.warn(
            "有 docker 指令，但 Docker 沒有在執行",
            "啟動 Docker Desktop / Engine（只有 *-docker 的 Agent 需要）",
        ),
        Docker::Missing => r.info("沒有安裝 Docker（只有 *-docker 的 Agent 需要）"),
    }
    match &env.sbx {
        Sbx::Ready(v, policy) => {
            if *policy {
                r.ok(format!("Docker Sandboxes {v}"));
            } else {
                r.warn(
                    format!("Docker Sandboxes {v} 沒有設定網路政策"),
                    "執行 sbx policy 設定可連線的網域",
                );
            }
        }
        Sbx::NotSignedIn(v) => r.warn(
            format!("Docker Sandboxes {v} 還沒登入"),
            "執行 sbx login（只有 *-sbx 的 Agent 需要）",
        ),
        Sbx::Missing => r.info("沒有安裝 Docker Sandboxes（只有 *-sbx 的 Agent 需要）"),
    }
    let agents: Vec<&str> = [
        ("claude", env.claude),
        ("agy", env.agy),
        ("copilot", env.copilot),
        ("codex", env.codex),
    ]
    .iter()
    .filter(|(_, f)| *f)
    .map(|(n, _)| *n)
    .collect();
    if agents.is_empty() {
        r.info("主機上沒有找到 claude / agy / copilot / codex（只用沙箱版的 Agent 時不需要）");
    } else {
        r.ok(format!("主機上的 Agent：{}", agents.join("、")));
    }
    let missing: Vec<&str> = ["curl", "tar"]
        .into_iter()
        .filter(|t| !on_path(t))
        .collect();
    if !missing.is_empty() {
        r.warn(
            format!("找不到 {}", missing.join("、")),
            "agt update 需要 curl 與 tar（Windows 10 以後內建）",
        );
    }
}

pub async fn run(json: bool) -> anyhow::Result<()> {
    let mut r = Report::default();
    check_binaries(&mut r);
    check_daemon(&mut r).await;
    check_config(&mut r);
    check_data(&mut r);
    check_tools(&mut r);

    if json {
        println!("{}", serde_json::to_string_pretty(&to_json(&r))?);
        if r.count(Level::Fail) > 0 {
            std::process::exit(1);
        }
        return Ok(());
    }
    for i in &r.0 {
        let mark = match i.level {
            Level::Ok => "✓",
            Level::Info => "-",
            Level::Warn => "!",
            Level::Fail => "✗",
        };
        println!(" {mark} {}", i.text);
        if let Some(h) = &i.hint {
            println!("     → {h}");
        }
    }
    let (w, f) = (r.count(Level::Warn), r.count(Level::Fail));
    println!();
    if f > 0 {
        println!("{f} 個失敗、{w} 個警告");
        std::process::exit(1);
    } else if w > 0 {
        println!("沒有失敗，{w} 個警告");
    } else {
        println!("一切正常");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_versions() {
        assert_eq!(git_version("2.43.0"), Some((2, 43)));
        assert_eq!(git_version("2.39.3.windows.1"), Some((2, 39)));
        assert!(git_version("2.24.1").unwrap() < (2, 25));
        assert_eq!(git_version("abc"), None);
        assert_eq!(git_version(""), None);
    }

    #[test]
    fn finds_recent_errors_only() {
        let log = "2026 ERROR 舊的\n".to_string() + &"2026 INFO ok\n".repeat(10);
        assert_eq!(recent_errors(&log, 5), (0, None));
        let log2 = log + "2026 ERROR panic：boom\n2026 INFO ok\n";
        let (n, last) = recent_errors(&log2, 5);
        assert_eq!(n, 1);
        assert!(last.unwrap().contains("boom"));
    }

    #[test]
    fn json_report_shape() {
        let mut r = Report::default();
        r.ok("a");
        r.warn("c", "h");
        let v = to_json(&r);
        assert_eq!(v["ok"], true);
        assert_eq!(v["warnings"], 1);
        assert_eq!(v["items"][0]["level"], "ok");
        assert!(v["items"][0]["hint"].is_null());
        assert_eq!(v["items"][1]["hint"], "h");
        r.fail("d", "h");
        assert_eq!(to_json(&r)["ok"], false);
    }

    #[test]
    fn report_counts_levels() {
        let mut r = Report::default();
        r.ok("a");
        r.info("b");
        r.warn("c", "h");
        r.fail("d", "h");
        r.fail("e", "h");
        assert_eq!(
            (
                r.count(Level::Ok),
                r.count(Level::Warn),
                r.count(Level::Fail)
            ),
            (1, 1, 2)
        );
    }
}
