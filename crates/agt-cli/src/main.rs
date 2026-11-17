//! agt：CLI 與 TUI 入口。

mod prompt;
mod setup;

use agt_proto::client::{Client, ClientError, Connected, expect_session, expect_sessions};
use agt_proto::transport::{self, ENV_SESSION};
use agt_proto::*;
use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
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
        /// claude | antigravity | copilot | shell，或設定檔中的其他 Agent
        agent: String,
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
    },
    /// 列出分頁
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// 開啟 TUI 並切到指定分頁
    Attach { id: String },
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
    },
    /// 初始設定：偵測 Docker / Docker Sandboxes，產生設定檔與 Dockerfile（第一次執行 agt 時會自動執行）
    Setup {
        /// 不詢問，全部採用預設答案
        #[arg(long, short)]
        yes: bool,
        /// 需要的 Docker 映像檔不存在時直接建置（搭配 --yes）
        #[arg(long)]
        build: bool,
    },
    /// 管理 daemon
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
}

#[derive(Subcommand)]
enum DaemonCmd {
    Start,
    Stop {
        #[arg(long, short)]
        yes: bool,
    },
    Restart {
        #[arg(long, short)]
        yes: bool,
    },
    Status,
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
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("無法建立 tokio runtime");
    if let Err(e) = rt.block_on(run(cli)) {
        eprintln!("agt：{e:#}");
        if let Some(ClientError::Remote { code, .. }) = e.downcast_ref::<ClientError>()
            && code == codes::PROTO_MISMATCH
        {
            eprintln!("提示：daemon 版本不同，請執行 `agt daemon restart`");
        }
        std::process::exit(1);
    }
}

async fn connect_kind(kind: &str) -> Result<Connected, ClientError> {
    Client::connect(&transport::endpoint(), kind).await
}

/// 連不上就在背景啟動 agtd，最多等 3 秒（規格 4.5）。
async fn ensure_daemon(kind: &str) -> anyhow::Result<Connected> {
    match connect_kind(kind).await {
        Ok(c) => return Ok(c),
        Err(e @ ClientError::Remote { .. }) => return Err(e.into()),
        Err(_) => {}
    }
    let exe = transport::sibling_exe("agtd");
    transport::spawn_detached(&exe, &[])
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
                s.task.clone(),
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

async fn tui(focus: Option<String>) -> anyhow::Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!("TUI 需要在終端機中執行");
    }
    if setup::needs_first_run() {
        setup::run(setup::Options {
            yes: false,
            build: false,
        })?;
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
            return Ok(());
        }
    } else if std::io::stdin().is_terminal() {
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
        Some(Cmd::Setup { yes, build }) => setup::run(setup::Options { yes, build }),
        Some(Cmd::Attach { id }) => tui(Some(id)).await,
        Some(Cmd::New {
            agent,
            repo,
            task,
            base,
            share,
            attach,
            yes,
        }) => {
            let c = ensure_daemon("cli").await?.client;
            let mut req = CreateSession {
                agent,
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
        Some(Cmd::Ls { json }) => {
            let c = ensure_daemon("cli").await?.client;
            let list = expect_sessions(c.request(ClientMsg::ListSessions).await?)?;
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
        Some(Cmd::Gc { prune }) => {
            let c = ensure_daemon("cli").await?.client;
            let ReplyBody::Gc(items) = c.request(ClientMsg::Gc { prune }).await? else {
                bail!("回應格式不符預期");
            };
            if items.is_empty() {
                println!("worktree 與資料庫一致");
            }
            for it in &items {
                let what = match it.kind.as_str() {
                    "orphan_worktree" => "孤兒 worktree（git 有、資料庫沒有）",
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
                    "\n執行 `agt gc --prune` 清理（有變更的 worktree 會保留，branch 不會刪除）"
                );
            }
            Ok(())
        }
        Some(Cmd::Daemon { cmd }) => daemon(cmd).await,
        Some(Cmd::Notify { .. }) => unreachable!(),
    }
}

async fn stop_daemon(yes: bool) -> anyhow::Result<bool> {
    let Ok(conn) = connect_kind("cli").await else {
        return Ok(false);
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
    // 改名留下的 agtd.exe.old 也才刪得掉。
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
        DaemonCmd::Start => {
            ensure_daemon("cli").await?;
            println!("daemon 已在執行");
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
        DaemonCmd::Status => match connect_kind("cli").await {
            Ok(conn) => {
                if let ReplyBody::Daemon(i) = conn.client.request(ClientMsg::DaemonStatus).await? {
                    println!("agtd {}（pid {}）", i.version, i.pid);
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
