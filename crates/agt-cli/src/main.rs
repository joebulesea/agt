//! agt：CLI 與 TUI 入口。

mod deps;
mod doctor;
mod log;
mod prompt;
mod setup;
mod stats;
mod statusline;
mod uninstall;
mod update;

use agt_proto::client::{Client, ClientError, Connected, expect_session, expect_sessions};
use agt_proto::transport::{self, ENV_SESSION};
use agt_proto::*;
use anyhow::{Context, bail};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::io::{IsTerminal, Read, Write};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "agt", version, about = "AI Agent 終端機多工器")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 開啟 TUI（預設）
    Tui,
    /// 建立分頁（含 worktree）
    New {
        /// claude | antigravity | copilot | codex | shell，或設定檔中的其他 Agent；
        /// 用 `--template` 時可省略（沿用範本的 Agent）
        agent: Option<String>,
        /// repo 或任意目錄，預設為目前目錄
        #[arg(long, default_value = ".")]
        repo: String,
        /// 任務名稱
        #[arg(long, default_value = "")]
        task: String,
        /// base branch，預設為目前的 HEAD branch
        #[arg(long)]
        base: Option<String>,
        /// 與既有工作階段共用 worktree
        #[arg(long)]
        share: Option<String>,
        /// 建立後開啟 TUI 並切到這個分頁
        #[arg(long)]
        attach: bool,
        /// 不詢問，直接執行 repo 的 setup 指令
        #[arg(long, short)]
        yes: bool,
        /// 套用設定檔中的範本（`[templates.<名稱>]`）：沒填的 Agent、任務、base 用範本的值
        #[arg(long)]
        template: Option<String>,
        /// 加上標籤（可重複，也可以用逗號分隔）
        #[arg(long = "tag", value_name = "標籤")]
        tags: Vec<String>,
    },
    /// 列出分頁
    Ls {
        #[arg(long)]
        json: bool,
        /// 只列出有這個標籤的分頁（不分大小寫）
        #[arg(long = "tag", value_name = "標籤")]
        tag: Option<String>,
    },
    /// 查看或修改分頁的標籤
    Tag {
        id: String,
        /// 加上標籤（可重複，也可以用逗號分隔）
        #[arg(long, value_name = "標籤", conflicts_with_all = ["set", "clear"])]
        add: Vec<String>,
        /// 移除標籤
        #[arg(long, value_name = "標籤", conflicts_with_all = ["set", "clear"])]
        remove: Vec<String>,
        /// 取代成這些標籤（逗號分隔）
        #[arg(long, value_name = "標籤", conflicts_with = "clear")]
        set: Option<String>,
        /// 清除所有標籤
        #[arg(long)]
        clear: bool,
    },
    /// 開啟 TUI 並切到指定分頁
    Attach { id: String },
    /// 統計各分頁花在各狀態的時間（只有時間，沒有 token／費用資料）
    Stats {
        /// 統計最近幾天建立的分頁（最多只保留 30 天）
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// 以 JSON 輸出
        #[arg(long)]
        json: bool,
    },
    /// 匯出分頁的終端機紀錄（去掉控制序列的純文字，或 Markdown）
    Log {
        id: String,
        /// 只輸出最後 N 行
        #[arg(long)]
        tail: Option<usize>,
        /// 輸出格式：text（預設）或 md
        #[arg(long, value_parser = ["text", "md"], default_value = "text")]
        format: String,
        /// 寫到檔案而不是終端機
        #[arg(long, short)]
        output: Option<String>,
        /// 輸出原始位元組（含控制序列），需要搭配 -o
        #[arg(long)]
        raw: bool,
        /// 覆寫已存在的輸出檔
        #[arg(long)]
        force: bool,
    },
    /// 由 Agent 的 hooks 呼叫，回報狀態（失敗時靜默結束）
    Notify {
        #[arg(long)]
        status: String,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        tool: Option<String>,
        /// 預設讀取 AGT_SESSION_ID
        #[arg(long)]
        session: Option<String>,
    },
    /// 由 Claude Code 的 statusLine 呼叫：把 stdin 的 JSON 交給 agt（`.agt/usage.json`），並輸出一行摘要
    Statusline,
    /// 關閉分頁
    Kill {
        id: String,
        /// 一併移除 worktree 與 branch
        #[arg(long)]
        cleanup: bool,
        #[arg(long, short)]
        yes: bool,
    },
    /// 收尾：merge / pr / keep / discard
    Finish {
        id: String,
        action: String,
        /// merge 策略：merge | squash | rebase
        #[arg(long)]
        strategy: Option<String>,
        #[arg(long, short)]
        yes: bool,
    },
    /// 顯示分頁的 diff
    Diff { id: String },
    /// 重新命名任務
    Rename { id: String, task: String },
    /// 在同一個 worktree 重新啟動已結束的 Agent（支援時接續上次的對話）
    Restart {
        id: String,
        /// 不接續上次的對話，開新對話
        #[arg(long)]
        fresh: bool,
    },
    /// 比對 git worktree 與資料庫，列出不一致的項目
    Gc {
        /// 清理：移除乾淨的孤兒 worktree、修正資料庫
        #[arg(long)]
        prune: bool,
        /// 以 JSON 輸出：`{pruned, items: [{kind, repo, path, branch, session}]}`
        #[arg(long)]
        json: bool,
    },
    /// 初始設定：檢查並安裝 git / Docker / Docker Sandboxes，產生設定檔與 Dockerfile（第一次執行 agt 時會自動執行）
    Setup {
        /// 不詢問，全部採用預設答案（不會安裝任何東西，除非加上 --install）
        #[arg(long, short)]
        yes: bool,
        /// 需要的 Docker 映像檔不存在時直接建置（搭配 --yes）
        #[arg(long)]
        build: bool,
        /// 用 winget 安裝缺少的必要項目（搭配 --yes；只限 Windows）
        #[arg(long)]
        install: bool,
    },
    /// 從 GitHub Release 更新 agt（核對 SHA256，並重新啟動 daemon）
    Update {
        /// 只檢查有沒有新版，不下載
        #[arg(long)]
        check: bool,
        /// 搭配 --check：以 JSON 輸出 `{current, latest, tag, status, update_available}`
        #[arg(long, requires = "check")]
        json: bool,
        /// 指定版本（例如 0.3.0），預設最新版
        #[arg(long)]
        version: Option<String>,
        /// 有 Agent 執行中時不詢問，直接結束 daemon
        #[arg(long, short)]
        yes: bool,
    },
    /// 解除安裝：停止 daemon、移除沙箱與容器、刪除執行檔與 PATH 設定
    Uninstall {
        /// 不詢問（設定與資料、worktree 仍需 --purge / --worktrees 才會刪除）
        #[arg(long, short)]
        yes: bool,
        /// 一併刪除設定、分頁資料庫與終端機紀錄
        #[arg(long)]
        purge: bool,
        /// 一併刪除所有 worktree（沒有 commit / merge 的修改會遺失）
        #[arg(long)]
        worktrees: bool,
        /// 只列出會做什麼，不改動任何東西
        #[arg(long)]
        dry_run: bool,
    },
    /// 檢查執行環境（daemon、設定檔、git、Docker、gh 等），並說明每個問題的下一步
    Doctor {
        /// 以 JSON 輸出（有失敗項目時 exit 1）
        #[arg(long)]
        json: bool,
    },
    /// 設定檔工具
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// 輸出 shell 補全腳本（bash、zsh、fish、powershell、elvish），不需要 daemon
    Completions {
        /// 目標 shell
        shell: Shell,
    },
    /// 管理 daemon
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// 檢查設定檔：語法與欄位錯誤（含行號）、找不到的指令、拼錯的變數等；不需要 daemon 在執行
    Check {
        /// 檢查指定的檔案，預設是 agt 使用的設定檔
        #[arg(long)]
        file: Option<String>,
        /// 一併檢查這個 repo 的 `.agt.toml`
        #[arg(long)]
        repo: Option<String>,
        /// 以 JSON 輸出（錯誤時 `{ok: false, error}`，exit 1）
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// 在這個行程中執行 daemon（通常由 agt 自動在背景啟動，不需要手動執行）
    Run {
        /// 日誌輸出到終端機，而不是 agtd.log
        #[arg(long)]
        foreground: bool,
    },
    Start,
    Stop {
        #[arg(long, short)]
        yes: bool,
    },
    Restart {
        #[arg(long, short)]
        yes: bool,
    },
    Status {
        /// 以 JSON 輸出：`daemon_running`、`socket`，執行中時另有 pid、version、sessions、running（執行中的分頁數）等欄位
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    // notify 必須快速且靜默，不建立多執行緒 runtime
    if let Some(Cmd::Notify {
        status,
        message,
        tool,
        session,
    }) = &cli.cmd
    {
        notify(status, message.clone(), tool.clone(), session.clone());
        return;
    }
    // status line 每次回覆都會執行，同樣要快速且靜默
    if let Some(Cmd::Statusline) = &cli.cmd {
        statusline::run();
        return;
    }
    // 補全腳本只需要命令定義，不連 daemon、不建立 runtime
    if let Some(Cmd::Completions { shell }) = &cli.cmd {
        let mut cmd = Cli::command();
        clap_complete::generate(*shell, &mut cmd, "agt", &mut std::io::stdout());
        return;
    }
    // daemon 自己建立 runtime，不能在 agt 的 runtime 內執行
    if let Some(Cmd::Daemon {
        cmd: DaemonCmd::Run { foreground },
    }) = &cli.cmd
    {
        if let Err(e) = agt_daemon::run_daemon(*foreground) {
            eprintln!("agt daemon：{e:#}");
            std::process::exit(1);
        }
        return;
    }
    // 清除上次 `agt update` 留下、當時還在執行所以刪不掉的舊檔
    update::cleanup_old();
    // 從檔案總管雙擊開啟時，結束後視窗會直接關閉；出錯時要先等使用者看完訊息
    let double_click = deps::launched_by_double_click();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("無法建立 tokio runtime");
    if let Err(e) = rt.block_on(run(cli)) {
        // 使用者選擇先結束（例如要重新開機），說明已經顯示過
        let stopped = e.downcast_ref::<deps::Stopped>().is_some();
        if !stopped {
            eprintln!("agt：{e:#}");
        }
        if let Some(ClientError::Remote { code, .. }) = e.downcast_ref::<ClientError>()
            && code == codes::PROTO_MISMATCH
        {
            eprintln!("{}", proto_mismatch_hint());
        }
        if double_click {
            prompt::pause();
        }
        std::process::exit(if stopped { 0 } else { 1 });
    }
}

async fn connect_kind(kind: &str) -> Result<Connected, ClientError> {
    Client::connect(&transport::endpoint(), kind).await
}

/// 連不上就在背景啟動 daemon（`agt daemon run`），最多等 3 秒（規格 4.5）。
async fn ensure_daemon(kind: &str) -> anyhow::Result<Connected> {
    ensure_daemon_with(kind, None).await
}

/// 同 [`ensure_daemon`]，但可指定用哪個執行檔啟動 daemon。
/// `agt update` 換掉自己之後要用新裝好的檔案：Linux 上此時的 `current_exe()` 會是
/// `…/agt.old (deleted)`，無法執行。
async fn ensure_daemon_with(
    kind: &str,
    exe: Option<&std::path::Path>,
) -> anyhow::Result<Connected> {
    match connect_kind(kind).await {
        Ok(c) => return Ok(c),
        Err(e @ ClientError::Remote { .. }) => return Err(e.into()),
        Err(_) => {}
    }
    let exe = match exe {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe().context("找不到目前的執行檔位置")?,
    };
    transport::spawn_detached(&exe, &["daemon", "run"])
        .with_context(|| format!("無法啟動 daemon（{}）", exe.display()))?;
    let start = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        match connect_kind(kind).await {
            Ok(c) => return Ok(c),
            Err(e @ ClientError::Remote { .. }) => return Err(e.into()),
            Err(e) if start.elapsed() > Duration::from_secs(3) => {
                bail!("daemon 啟動後 3 秒內仍無法連線：{e}")
            }
            Err(_) => {}
        }
    }
}

/// 舊版 daemon 還在執行時的處理說明。舊版的 daemon 不認得新協定，`agt daemon restart` 結束不了它，
/// 要在系統層級結束行程（分頁資料保留在資料庫，之後可用 Ctrl+A → R 接續）。
fn proto_mismatch_hint() -> String {
    let (legacy, current) = if cfg!(windows) {
        (
            "taskkill /IM agtd.exe /F",
            "在工作管理員結束「agt.exe」中的背景 daemon（命令列含 `daemon run`）",
        )
    } else {
        ("pkill agtd", "pkill -f 'agt daemon run'")
    };
    format!(
        "提示：正在執行的 daemon 與這個 agt 的版本不同（協定版本不相容），`agt daemon restart` 結束不了它。\n\
         \x20 • 從舊版（有獨立 agtd 的版本）升級：{legacy}\n\
         \x20 • 其他版本：{current}\n\
         \x20 結束後再執行 agt（分頁資料會保留，可用 Ctrl+A → R 接續對話）"
    )
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).is_ok() && matches!(s.trim(), "y" | "Y" | "yes")
}

fn absolute(p: &str) -> String {
    std::path::absolute(p)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string())
}

/// 依顯示寬度補空白（全形字元佔 2 欄）。
fn pad(s: &str, w: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    format!("{s}{}", " ".repeat(w.saturating_sub(s.width())))
}

/// `agt tag` 的修改內容。
#[derive(Default)]
struct TagEdit {
    add: Vec<String>,
    remove: Vec<String>,
    /// 取代成這些標籤
    set: Option<Vec<String>>,
    clear: bool,
}

impl TagEdit {
    fn is_noop(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty() && self.set.is_none() && !self.clear
    }
}

/// 套用修改後的標籤清單（還沒驗證；daemon 會再用 `tags::normalize` 驗證與去重）。
/// 移除不分大小寫；`set` 與 `clear` 優先於 add／remove（clap 已讓它們互斥）。
fn edit_tags(current: &[String], e: &TagEdit) -> Vec<String> {
    if e.clear {
        return Vec::new();
    }
    if let Some(set) = &e.set {
        return set.clone();
    }
    let mut out: Vec<String> = current
        .iter()
        .filter(|t| {
            !e.remove
                .iter()
                .any(|r| r.to_lowercase() == t.to_lowercase())
        })
        .cloned()
        .collect();
    out.extend(e.add.iter().cloned());
    out
}

fn format_tags(tags: &[String]) -> String {
    if tags.is_empty() {
        "（沒有標籤）".into()
    } else {
        tags.iter()
            .map(|t| format!("#{t}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// 只留下有這個標籤的分頁（不分大小寫；可以帶開頭的 `#`）。
fn filter_by_tag(list: Vec<SessionInfo>, tag: &str) -> Vec<SessionInfo> {
    let want = tag.trim().trim_start_matches('#').to_lowercase();
    list.into_iter()
        .filter(|s| s.tags.iter().any(|t| t.to_lowercase() == want))
        .collect()
}

fn print_sessions(list: &[SessionInfo]) {
    if list.is_empty() {
        println!("（沒有分頁）");
        return;
    }
    let rows: Vec<[String; 6]> = list
        .iter()
        .map(|s| {
            let git = match &s.branch {
                Some(b) => format!(
                    "{b}{}{}",
                    if s.ahead > 0 {
                        format!(" +{}", s.ahead)
                    } else {
                        String::new()
                    },
                    if s.dirty { " ✎" } else { "" }
                ),
                None => "無 worktree".into(),
            };
            [
                s.short_id.clone(),
                format!("{} {}", s.status.symbol(), s.status.as_str()),
                s.agent.clone(),
                s.repo_name.clone().unwrap_or_default(),
                git,
                if s.tags.is_empty() {
                    s.task.clone()
                } else {
                    format!("{}  {}", s.task, format_tags(&s.tags))
                },
            ]
        })
        .collect();
    let head = ["ID", "狀態", "Agent", "Repo", "Branch", "任務"];
    let widths: Vec<usize> = (0..6)
        .map(|i| {
            use unicode_width::UnicodeWidthStr;
            rows.iter()
                .map(|r| r[i].width())
                .chain([head[i].width()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: Vec<&str>| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i == 5 {
                    c.to_string()
                } else {
                    pad(c, widths[i])
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    println!("{}", line(head.to_vec()));
    for r in &rows {
        println!("{}", line(r.iter().map(String::as_str).collect()));
    }
}

const NO_GIT: &str = "缺少 git 2.20 以上（建立 worktree 需要），無法開啟 agt。\
安裝後重新執行 agt（或雙擊 agt.exe）會再檢查一次。";

async fn tui(focus: Option<String>) -> anyhow::Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!("TUI 需要在終端機中執行");
    }
    if setup::needs_first_run() {
        setup::run(setup::Options {
            yes: false,
            build: false,
            install: false,
        })?;
        if !setup::has_git_quick() {
            bail!("{NO_GIT}");
        }
        println!();
        let open = prompt::select(
            "要現在開啟 agt 嗎？",
            &[
                prompt::Item {
                    label: "開啟 agt",
                    desc: "",
                },
                prompt::Item {
                    label: "離開",
                    desc: "之後執行 agt 開啟",
                },
            ],
            0,
        )?;
        if open != 0 {
            if deps::launched_by_double_click() {
                prompt::pause();
            }
            return Ok(());
        }
    } else if std::io::stdin().is_terminal() {
        // 每次開啟都快速檢查必要項目（只看 PATH）；缺少時才做完整檢查並顯示安裝選單
        if !setup::has_git_quick() && !setup::install_missing()? {
            bail!("{NO_GIT}");
        }
        // 每次開啟都檢查：有新的可用 Agent（例如剛安裝 Docker Sandboxes）時詢問是否加入
        // 按 Esc 只是略過這次，不影響開啟 agt；其他錯誤顯示後照常開啟
        if let Err(e) = setup::offer_new_agents()
            && e.downcast_ref::<prompt::Cancelled>().is_none()
        {
            eprintln!("agt：檢查可用的 Agent 時發生錯誤：{e:#}");
        }
    }
    let conn = ensure_daemon("tui").await?;
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(msg) = agt_tui::run(conn, agt_tui::Options { focus, cwd }).await? {
        eprintln!("{msg}");
    }
    Ok(())
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.cmd {
        None | Some(Cmd::Tui) => tui(None).await,
        Some(Cmd::Setup {
            yes,
            build,
            install,
        }) => setup::run(setup::Options {
            yes,
            build,
            install,
        }),
        Some(Cmd::Attach { id }) => tui(Some(id)).await,
        Some(Cmd::Stats { days, json }) => {
            let list = stats::collect(days)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&stats::to_json(days, &list))?
                );
            } else {
                print!("{}", stats::to_text(days, &list));
            }
            Ok(())
        }
        Some(Cmd::Log {
            id,
            tail,
            format,
            output,
            raw,
            force,
        }) => {
            log::run(log::Options {
                id,
                tail,
                raw,
                output,
                format,
                force,
            })
            .await
        }
        Some(Cmd::New {
            agent,
            repo,
            task,
            base,
            share,
            attach,
            yes,
            template,
            tags,
        }) => {
            let agent = agent.unwrap_or_default();
            if agent.is_empty() && template.is_none() {
                bail!("請指定 Agent，或用 --template 套用範本");
            }
            let tags = tags
                .iter()
                .flat_map(|t| agt_proto::tags::split_input(t))
                .collect();
            let c = ensure_daemon("cli").await?.client;
            let mut req = CreateSession {
                agent,
                template,
                tags,
                repo: absolute(&repo),
                task,
                base_branch: base,
                share,
                trust_setup: yes,
                cols: None,
                rows: None,
            };
            let info = loop {
                match c.request(ClientMsg::CreateSession(req.clone())).await {
                    Ok(r) => break expect_session(r)?,
                    Err(ClientError::Remote { code, message })
                        if code == codes::SETUP_UNTRUSTED =>
                    {
                        println!(
                            "這個 repo 設定了 setup 指令，第一次執行前需要確認：\n\n  {message}\n"
                        );
                        if !confirm("要執行嗎？") {
                            bail!("已取消");
                        }
                        req.trust_setup = true;
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            println!("已建立分頁 {}（{}）", info.short_id, info.task);
            match (&info.worktree, &info.branch) {
                (Some(w), Some(b)) => println!("  worktree：{w}\n  branch  ：{b}"),
                _ => println!("  目錄：{}（無 worktree）", info.cwd),
            }
            if attach {
                tui(Some(info.id)).await?;
            }
            Ok(())
        }
        Some(Cmd::Tag {
            id,
            add,
            remove,
            set,
            clear,
        }) => {
            let c = ensure_daemon("cli").await?.client;
            let cur = expect_session(c.request(ClientMsg::GetSession { id: id.clone() }).await?)?;
            let split = |v: &[String]| -> Vec<String> {
                v.iter()
                    .flat_map(|t| agt_proto::tags::split_input(t))
                    .collect()
            };
            let edit = TagEdit {
                add: split(&add),
                remove: split(&remove),
                set: set.as_deref().map(agt_proto::tags::split_input),
                clear,
            };
            if edit.is_noop() {
                println!("{}", format_tags(&cur.tags));
                return Ok(());
            }
            let new = edit_tags(&cur.tags, &edit);
            let s = expect_session(
                c.request(ClientMsg::SetTags {
                    id: cur.id.clone(),
                    tags: new,
                })
                .await?,
            )?;
            println!("{}：{}", s.short_id, format_tags(&s.tags));
            Ok(())
        }
        Some(Cmd::Ls { json, tag }) => {
            let c = ensure_daemon("cli").await?.client;
            let mut list = expect_sessions(c.request(ClientMsg::ListSessions).await?)?;
            if let Some(t) = &tag {
                list = filter_by_tag(list, t);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                print_sessions(&list);
            }
            Ok(())
        }
        Some(Cmd::Kill { id, cleanup, yes }) => {
            let c = ensure_daemon("cli").await?.client;
            let mut force = false;
            loop {
                match c
                    .request(ClientMsg::Kill {
                        id: id.clone(),
                        cleanup,
                        force,
                    })
                    .await
                {
                    Ok(_) => break,
                    Err(ClientError::Remote { code, message }) if code == codes::NEEDS_CONFIRM => {
                        if !(yes || confirm(&message)) {
                            bail!("已取消");
                        }
                        force = true;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            println!("已關閉 {id}");
            Ok(())
        }
        Some(Cmd::Finish {
            id,
            action,
            strategy,
            yes,
        }) => {
            let c = ensure_daemon("cli").await?.client;
            let strategy = strategy
                .map(|s| s.parse::<MergeStrategy>())
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let action = match action.as_str() {
                "merge" => FinishAction::Merge { strategy },
                "pr" => FinishAction::Pr,
                "keep" => FinishAction::Keep,
                "discard" => FinishAction::Discard,
                o => bail!("未知的收尾動作：{o}（merge | pr | keep | discard）"),
            };
            let mut force = false;
            loop {
                match c
                    .request(ClientMsg::Finish {
                        id: id.clone(),
                        action: action.clone(),
                        force,
                    })
                    .await
                {
                    Ok(ReplyBody::Text(t)) => {
                        println!("{t}");
                        break;
                    }
                    Ok(_) => break,
                    Err(ClientError::Remote { code, message }) if code == codes::NEEDS_CONFIRM => {
                        if !(yes || confirm(&message)) {
                            bail!("已取消");
                        }
                        force = true;
                    }
                    Err(ClientError::Remote { code, message }) if code == codes::NEED_STRATEGY => {
                        bail!("{message}（使用 --strategy merge|squash|rebase）")
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        }
        Some(Cmd::Diff { id }) => {
            let c = ensure_daemon("cli").await?.client;
            if let ReplyBody::Text(t) = c.request(ClientMsg::Diff { id }).await? {
                // 檔案內容可能含有控制序列（例如改寫剪貼簿的 OSC 52），不能原樣送到終端機
                println!("{}", agt_proto::text::printable(&t));
            }
            Ok(())
        }
        Some(Cmd::Rename { id, task }) => {
            let c = ensure_daemon("cli").await?.client;
            let s = expect_session(c.request(ClientMsg::Rename { id, task }).await?)?;
            println!("{} → {}", s.short_id, s.task);
            Ok(())
        }
        Some(Cmd::Restart { id, fresh }) => {
            let c = ensure_daemon("cli").await?.client;
            let s = expect_session(c.request(ClientMsg::Restart { id, fresh }).await?)?;
            println!("已在 {} 重新啟動 {}", s.cwd, s.agent);
            Ok(())
        }
        Some(Cmd::Gc { prune, json }) => {
            let c = ensure_daemon("cli").await?.client;
            let ReplyBody::Gc(items) = c.request(ClientMsg::Gc { prune }).await? else {
                bail!("回應格式不符預期");
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&gc_json(prune, &items))?);
                return Ok(());
            }
            if items.is_empty() {
                println!("worktree 與資料庫一致");
            }
            for it in &items {
                let what = match it.kind.as_str() {
                    "orphan_worktree" => "孤兒 worktree（git 有、資料庫沒有）",
                    "closed_worktree" => "已關閉分頁留下的 worktree 副本",
                    _ => "遺失的 worktree（資料庫有、git 或磁碟上沒有）",
                };
                println!(
                    "{what}\n  repo  ：{}\n  路徑  ：{}\n  branch：{}",
                    it.repo,
                    it.path,
                    it.branch.clone().unwrap_or_default()
                );
            }
            if !items.is_empty() && !prune {
                println!(
                    "\n執行 `agt gc --prune` 清理（有變更的 worktree 會保留；已關閉分頁的 branch 只在已合併時刪除，其他 branch 不會刪除）"
                );
            }
            Ok(())
        }
        Some(Cmd::Update {
            check,
            json,
            version,
            yes,
        }) => update::run(check, json, version, yes).await,
        Some(Cmd::Uninstall {
            yes,
            purge,
            worktrees,
            dry_run,
        }) => uninstall::run(yes, purge, worktrees, dry_run).await,
        Some(Cmd::Doctor { json }) => doctor::run(json).await,
        Some(Cmd::Config {
            cmd: ConfigCmd::Check { file, repo, json },
        }) => {
            // 設定檔的解析與驗證在 agt-daemon（和 daemon 載入的是同一份程式）
            let mut args: Vec<String> = vec![];
            if let Some(f) = file {
                args.extend(["--file".into(), absolute(&f)]);
            }
            if let Some(r) = repo {
                args.extend(["--repo".into(), absolute(&r)]);
            }
            if json {
                args.push("--json".into());
            }
            std::process::exit(agt_daemon::check_config(&args));
        }
        Some(Cmd::Daemon { cmd }) => daemon(cmd).await,
        Some(Cmd::Notify { .. } | Cmd::Statusline | Cmd::Completions { .. }) => unreachable!(),
    }
}

/// `agt daemon status` 連線 daemon 的結果。
enum DaemonProbe {
    /// 連得上；`None` = 回應不是預期的格式，沒有詳細資訊
    Running(Option<DaemonInfo>),
    /// daemon 在執行但拒絕連線（例如協定版本不同）
    Error(String),
    NotRunning,
}

/// `agt daemon status --json` 的輸出。`daemon_running` 是 daemon 是否在執行；
/// `running` 是 `DaemonInfo` 原有的「執行中的分頁數」，兩者不能混用。
fn daemon_status_json(socket: &str, probe: &DaemonProbe) -> anyhow::Result<serde_json::Value> {
    Ok(match probe {
        DaemonProbe::Running(Some(i)) => {
            let mut v = serde_json::to_value(i)?;
            v["daemon_running"] = true.into();
            v["socket"] = socket.into();
            v
        }
        DaemonProbe::Running(None) => {
            serde_json::json!({"daemon_running": true, "socket": socket})
        }
        DaemonProbe::Error(message) => {
            serde_json::json!({"daemon_running": false, "socket": socket, "error": message})
        }
        DaemonProbe::NotRunning => serde_json::json!({"daemon_running": false, "socket": socket}),
    })
}

/// `agt gc --json` 的輸出：`{pruned, items: [{kind, repo, path, branch, session}]}`。
fn gc_json(pruned: bool, items: &[GcItem]) -> serde_json::Value {
    serde_json::json!({"pruned": pruned, "items": items})
}

async fn stop_daemon(yes: bool) -> anyhow::Result<bool> {
    let conn = match connect_kind("cli").await {
        Ok(c) => c,
        // 舊版 daemon 還在執行：協定版本不同，連不上也沒辦法用協定要求它結束
        Err(e @ ClientError::Remote { .. }) if e.code() == Some(codes::PROTO_MISMATCH) => {
            bail!(
                "有另一個版本的 daemon 還在執行，無法透過協定結束它。\n{}",
                proto_mismatch_hint()
            )
        }
        Err(_) => return Ok(false),
    };
    let c = conn.client;
    let mut pid = None;
    if let ReplyBody::Daemon(info) = c.request(ClientMsg::DaemonStatus).await? {
        pid = Some(info.pid);
        if info.running > 0 && !yes {
            let q = format!(
                "有 {} 個 Agent 正在執行，結束 daemon 會一併結束它們。確定嗎？",
                info.running
            );
            if !confirm(&q) {
                bail!("已取消");
            }
        }
    }
    let _ = c
        .request_timeout(
            ClientMsg::Shutdown { kill_agents: true },
            Duration::from_secs(5),
        )
        .await;
    // 等待 daemon 真的結束
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if transport::connect(&transport::endpoint()).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // named pipe 關閉後，舊 daemon 還會收尾一段時間（結束 Agent、關閉 runtime），期間仍持有 lock 檔，
    // Windows 上它的 exe 也還被鎖住。等程序真的結束，接著啟動的新 daemon 才拿得到 lock，
    // 改名留下的 agt.exe.old 也才刪得掉。
    if let Some(pid) = pid {
        let exited = tokio::task::spawn_blocking(move || {
            transport::wait_pid_exit(pid, Duration::from_secs(10))
        })
        .await
        .unwrap_or(false);
        if !exited {
            eprintln!("警告：daemon（pid {pid}）10 秒內仍未結束");
        }
    }
    Ok(true)
}

async fn daemon(cmd: DaemonCmd) -> anyhow::Result<()> {
    match cmd {
        // main() 在建立 runtime 之前就處理了
        DaemonCmd::Run { .. } => {}
        DaemonCmd::Start => {
            // 先確認是否已在執行，訊息才能分辨「本來就在跑」和「這次才啟動」
            let running = connect_kind("cli").await.is_ok();
            ensure_daemon("cli").await?;
            let msg = if running {
                "daemon 已在執行"
            } else {
                "daemon 已啟動"
            };
            println!("{msg}");
        }
        DaemonCmd::Stop { yes } => {
            if stop_daemon(yes).await? {
                println!("daemon 已結束");
            } else {
                println!("daemon 沒有在執行");
            }
        }
        DaemonCmd::Restart { yes } => {
            stop_daemon(yes).await?;
            ensure_daemon("cli").await?;
            println!("daemon 已重新啟動");
        }
        DaemonCmd::Status { json: true } => {
            let socket = transport::endpoint();
            let probe = match connect_kind("cli").await {
                Ok(conn) => match conn.client.request(ClientMsg::DaemonStatus).await? {
                    ReplyBody::Daemon(i) => DaemonProbe::Running(Some(i)),
                    _ => DaemonProbe::Running(None),
                },
                Err(ClientError::Remote { message, .. }) => DaemonProbe::Error(message),
                Err(_) => DaemonProbe::NotRunning,
            };
            let v = daemon_status_json(&socket, &probe)?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        DaemonCmd::Status { json: false } => match connect_kind("cli").await {
            Ok(conn) => {
                if let ReplyBody::Daemon(i) = conn.client.request(ClientMsg::DaemonStatus).await? {
                    println!("agt daemon {}（pid {}）", i.version, i.pid);
                    println!("  socket    ：{}", transport::endpoint());
                    println!("  工作階段  ：{}（執行中 {}）", i.sessions, i.running);
                    println!("  UI 連線   ：{}", i.clients);
                    println!("  資料目錄  ：{}", i.data_dir);
                    println!("  設定檔    ：{}", i.config_path);
                }
            }
            Err(ClientError::Remote { message, .. }) => println!("daemon 回應錯誤：{message}"),
            Err(_) => println!("daemon 沒有在執行（{}）", transport::endpoint()),
        },
    }
    Ok(())
}

/// `agt notify`：讀取 AGT_SESSION_ID / AGT_SOCKET 送出 Notify；任何失敗都靜默結束（exit 0）。
fn notify(status: &str, message: Option<String>, tool: Option<String>, session: Option<String>) {
    let Ok(status) = status.parse::<AgentStatus>() else {
        return;
    };
    let Some(session_id) = session
        .or_else(|| std::env::var(ENV_SESSION).ok())
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    // Claude Code 會把 hook 的 JSON 從 stdin 傳進來，從中取出工具名稱與訊息。
    let (mut tool, mut message) = (tool, message);
    if !std::io::stdin().is_terminal() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::stdin().take(1 << 20).read_to_string(&mut s);
            let _ = tx.send(s);
        });
        if let Ok(s) = rx.recv_timeout(Duration::from_millis(100))
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
        {
            if tool.is_none() {
                tool = v["tool_name"].as_str().map(str::to_string);
            }
            if message.is_none() {
                message = v["message"].as_str().map(str::to_string);
            }
        }
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    rt.block_on(async {
        let fut = async {
            let conn = Client::connect(&transport::endpoint(), "notify")
                .await
                .ok()?;
            conn.client
                .request(ClientMsg::Notify {
                    session_id,
                    status,
                    message,
                    tool,
                })
                .await
                .ok()
        };
        let _ = tokio::time::timeout(Duration::from_secs(2), fut).await;
    });
}

#[cfg(test)]
mod daemon_hint_tests {
    use super::*;

    #[test]
    fn mismatch_hint_explains_manual_stop() {
        let h = proto_mismatch_hint();
        assert!(h.contains("結束不了它") && h.contains("agtd"), "{h}");
        assert!(
            h.contains(if cfg!(windows) { "taskkill" } else { "pkill" }),
            "{h}"
        );
    }
}

#[cfg(test)]
mod gc_json_tests {
    use super::*;

    fn item(kind: &str, branch: Option<&str>, session: Option<&str>) -> GcItem {
        GcItem {
            kind: kind.into(),
            repo: "/r/proj".into(),
            path: "/w/x".into(),
            branch: branch.map(Into::into),
            session: session.map(Into::into),
        }
    }

    #[test]
    fn empty_list() {
        let v = gc_json(false, &[]);
        assert_eq!(v["pruned"], false);
        assert_eq!(v["items"].as_array().unwrap().len(), 0);
        // 一定有這兩個欄位，腳本才不用判斷是否存在
        assert_eq!(v.as_object().unwrap().len(), 2);
    }

    #[test]
    fn items_keep_all_fields() {
        let v = gc_json(
            true,
            &[
                item("orphan_worktree", Some("agt/x/orphan"), None),
                item("closed_worktree", None, Some("ab12cd34")),
            ],
        );
        assert_eq!(v["pruned"], true);
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["kind"], "orphan_worktree");
        assert_eq!(items[0]["repo"], "/r/proj");
        assert_eq!(items[0]["path"], "/w/x");
        assert_eq!(items[0]["branch"], "agt/x/orphan");
        assert!(items[0]["session"].is_null());
        assert_eq!(items[1]["kind"], "closed_worktree");
        assert!(items[1]["branch"].is_null());
        assert_eq!(items[1]["session"], "ab12cd34");
        // 每個項目都有完整的五個欄位（沒有值時是 null，不是缺少）
        for it in items {
            assert_eq!(it.as_object().unwrap().len(), 5, "{it}");
        }
    }

    #[test]
    fn exact_shape_snapshot() {
        // 欄位名稱是對外的格式：改名或增減欄位會破壞腳本，所以整份比對
        let v = gc_json(
            true,
            &[
                item("orphan_worktree", Some("agt/x/o"), None),
                item("closed_worktree", None, Some("ab12cd34")),
            ],
        );
        let want = serde_json::json!({
            "pruned": true,
            "items": [
                {"kind": "orphan_worktree", "repo": "/r/proj", "path": "/w/x",
                 "branch": "agt/x/o", "session": null},
                {"kind": "closed_worktree", "repo": "/r/proj", "path": "/w/x",
                 "branch": null, "session": "ab12cd34"},
            ],
        });
        assert_eq!(v, want);
    }

    #[test]
    fn order_is_preserved_and_all_kinds_pass_through() {
        let kinds = ["orphan_worktree", "missing_worktree", "closed_worktree"];
        let items: Vec<GcItem> = kinds.iter().map(|k| item(k, None, None)).collect();
        let v = gc_json(false, &items);
        let got: Vec<&str> = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["kind"].as_str().unwrap())
            .collect();
        assert_eq!(got, kinds);
    }

    #[test]
    fn pruned_flag_does_not_depend_on_items() {
        for pruned in [false, true] {
            assert_eq!(gc_json(pruned, &[])["pruned"], pruned);
            let one = [item("orphan_worktree", None, None)];
            assert_eq!(gc_json(pruned, &one)["pruned"], pruned);
        }
    }

    #[test]
    fn special_characters_survive_round_trip() {
        // Windows 反斜線、中文、空白、引號、換行都要能正確跳脫與還原
        let weird = "C:\\Users\\測試 user\\\"wt\"\n新行";
        let it = GcItem {
            kind: "orphan_worktree".into(),
            repo: weird.into(),
            path: weird.into(),
            branch: Some("agt/測試/分支".into()),
            session: None,
        };
        let v = gc_json(false, std::slice::from_ref(&it));
        assert_eq!(v["items"][0]["path"], weird);
        let text = serde_json::to_string(&v).unwrap();
        // 序列化後是單行（換行被跳脫），腳本可以逐行處理
        assert!(!text.contains('\n'));
        let back: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["items"][0]["repo"], weird);
        assert_eq!(back["items"][0]["branch"], "agt/測試/分支");
        // 還原成 GcItem 後與原本相同
        let item_back: GcItem = serde_json::from_value(back["items"][0].clone()).unwrap();
        assert_eq!(item_back, it);
    }

    #[test]
    fn items_deserialize_without_optional_fields() {
        // 協定上 branch / session 有預設值：舊版 daemon 沒送這兩欄也要能讀
        let it: GcItem =
            serde_json::from_str(r#"{"kind":"orphan_worktree","repo":"/r","path":"/p"}"#).unwrap();
        assert_eq!((&it.branch, &it.session), (&None, &None));
        // 再輸出時補成 null，欄位不會缺
        let v = gc_json(false, &[it]);
        assert!(v["items"][0]["branch"].is_null());
        assert!(v["items"][0]["session"].is_null());
    }

    #[test]
    fn pretty_output_is_multiline_and_deterministic() {
        let v = gc_json(false, &[item("orphan_worktree", Some("b"), None)]);
        let a = serde_json::to_string_pretty(&v).unwrap();
        let b = serde_json::to_string_pretty(&v).unwrap();
        assert_eq!(a, b);
        assert!(a.lines().count() > 1);
    }

    #[test]
    fn output_is_valid_json_text() {
        let v = gc_json(false, &[item("missing_worktree", None, None)]);
        let text = serde_json::to_string_pretty(&v).unwrap();
        let back: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, v);
    }
}

#[cfg(test)]
mod daemon_status_json_tests {
    use super::*;

    fn info() -> DaemonInfo {
        DaemonInfo {
            pid: 4242,
            version: "0.2.1".into(),
            sessions: 3,
            running: 2,
            clients: 1,
            data_dir: "/d".into(),
            config_path: "/d/config.toml".into(),
        }
    }

    #[test]
    fn running_keeps_session_count_separate_from_daemon_running() {
        let v = daemon_status_json("/s.sock", &DaemonProbe::Running(Some(info()))).unwrap();
        assert_eq!(v["daemon_running"], true);
        // `running` 是執行中的分頁數，不能被布林蓋掉
        assert_eq!(v["running"], 2);
        assert_eq!(v["sessions"], 3);
        assert_eq!(v["pid"], 4242);
        assert_eq!(v["version"], "0.2.1");
        assert_eq!(v["clients"], 1);
        assert_eq!(v["data_dir"], "/d");
        assert_eq!(v["config_path"], "/d/config.toml");
        assert_eq!(v["socket"], "/s.sock");
        assert!(v.get("error").is_none());
    }

    #[test]
    fn running_without_details() {
        let v = daemon_status_json("/s.sock", &DaemonProbe::Running(None)).unwrap();
        assert_eq!(v["daemon_running"], true);
        assert_eq!(v["socket"], "/s.sock");
        assert!(v.get("pid").is_none());
    }

    #[test]
    fn not_running() {
        let v = daemon_status_json("/s.sock", &DaemonProbe::NotRunning).unwrap();
        assert_eq!(v["daemon_running"], false);
        assert_eq!(v["socket"], "/s.sock");
        assert!(v.get("pid").is_none() && v.get("error").is_none());
        assert_eq!(v.as_object().unwrap().len(), 2);
    }

    #[test]
    fn refused_connection_reports_error() {
        let v = daemon_status_json("/s.sock", &DaemonProbe::Error("協定版本不同".into())).unwrap();
        assert_eq!(v["daemon_running"], false);
        assert_eq!(v["error"], "協定版本不同");
        assert_eq!(v["socket"], "/s.sock");
    }
}

#[cfg(test)]
mod tag_tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_and_remove() {
        let cur = v(&["bug", "ui"]);
        let e = TagEdit {
            add: v(&["urgent"]),
            remove: v(&["UI"]),
            ..Default::default()
        };
        assert_eq!(edit_tags(&cur, &e), ["bug", "urgent"]);
    }

    #[test]
    fn set_and_clear_replace_everything() {
        let cur = v(&["a", "b"]);
        let e = TagEdit {
            set: Some(v(&["x"])),
            ..Default::default()
        };
        assert_eq!(edit_tags(&cur, &e), ["x"]);
        let e = TagEdit {
            clear: true,
            ..Default::default()
        };
        assert!(edit_tags(&cur, &e).is_empty());
    }

    #[test]
    fn noop_is_detected() {
        assert!(TagEdit::default().is_noop());
        assert!(
            !TagEdit {
                clear: true,
                ..Default::default()
            }
            .is_noop()
        );
        assert!(
            !TagEdit {
                set: Some(vec![]),
                ..Default::default()
            }
            .is_noop()
        );
    }

    #[test]
    fn formats_tags() {
        assert_eq!(format_tags(&[]), "（沒有標籤）");
        assert_eq!(format_tags(&v(&["a", "b"])), "#a #b");
    }

    fn sess(id: &str, tags: &[&str]) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            short_id: id.into(),
            agent: "shell".into(),
            repo_path: None,
            repo_name: None,
            task: "t".into(),
            worktree: None,
            branch: None,
            base_branch: None,
            cwd: ".".into(),
            status: AgentStatus::Idle,
            exit_code: None,
            ahead: 0,
            behind: 0,
            dirty: false,
            created_at: 0,
            status_since: 0,
            shared: false,
            tags: v(tags),
            usage: None,
        }
    }

    #[test]
    fn filters_by_tag_ignoring_case_and_hash() {
        let list = vec![sess("a", &["Bug"]), sess("b", &["ui"]), sess("c", &[])];
        let ids = |l: Vec<SessionInfo>| l.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids(filter_by_tag(list.clone(), "bug")), ["a"]);
        assert_eq!(ids(filter_by_tag(list.clone(), "#UI")), ["b"]);
        assert!(filter_by_tag(list, "none").is_empty());
    }
}
