//! daemon：持有所有 PTY 與畫面狀態；TUI 只是一個可以隨時關閉、重新連線的顯示端。
//! 以 `agt daemon run` 執行（不再有獨立的 agtd 執行檔）。

mod check;
mod config;
mod server;
mod state;

use agt_proto::transport;
use config::{Config, Paths};
use state::Daemon;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 同時連線數上限（TUI、CLI、hook 的 `agt notify` 都是短連線，正常用量遠低於此）。
const MAX_CONNECTIONS: usize = 64;

/// 資料庫的位置（`agt stats` 直接讀取）。
pub fn db_path() -> std::path::PathBuf {
    Paths::detect().db()
}

/// 分頁的終端機紀錄檔（輪替出去的舊檔在前），不存在的略過。`session_id` 必須是 daemon 回報的完整 id。
pub fn session_log_files(session_id: &str) -> Vec<std::path::PathBuf> {
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Vec::new();
    }
    let logs = Paths::detect().logs();
    [format!("{session_id}.log.1"), format!("{session_id}.log")]
        .iter()
        .map(|n| logs.join(n))
        .filter(|p| p.is_file())
        .collect()
}

/// `agt config check`：只檢查設定檔，不取得 lock、不啟動 daemon。回傳結束碼。
pub fn check_config(args: &[String]) -> i32 {
    check::run(args)
}

/// 執行 daemon 直到結束（`agt daemon run`）。`foreground` 時日誌輸出到終端機，否則寫入 `agtd.log`。
/// 必須在還沒有 tokio runtime 的執行緒上呼叫；結束時直接結束行程。
pub fn run_daemon(foreground: bool) -> anyhow::Result<()> {
    let mut paths = Paths::detect();
    // 舊版把資料放在 %LOCALAPPDATA%\agt：第一次用新版啟動時搬到 %APPDATA%\agt\data
    let migrated = Paths::legacy_data_dir().and_then(|old| paths.migrate_from(&old));
    std::fs::create_dir_all(&paths.data_dir)?;

    // lock 檔確保每位使用者只有一個 daemon（規格 4.5）
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.lock())?;
    if lock.try_lock().is_err() {
        eprintln!("daemon 已在執行中");
        return Ok(());
    }

    let appender = tracing_appender::rolling::never(&paths.data_dir, "agtd.log");
    // 同步寫入：daemon 日誌量小，且異常結束時不能遺失最後幾行
    let writer = std::sync::Mutex::new(appender);
    let filter =
        tracing_subscriber::EnvFilter::try_from_env("AGT_LOG").unwrap_or_else(|_| "info".into());
    if foreground {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(writer)
            .init();
    }
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic：{info}");
    }));
    if let Some(msg) = migrated {
        tracing::info!("{msg}");
    }
    // 取得 lock 之後才搬：這時沒有 Agent 在寫紀錄檔
    if let Some(msg) = paths.migrate_logs() {
        tracing::info!("{msg}");
    }
    // 啟動時舊版 daemon / TUI 可能還沒完全結束（exe 仍被鎖住），之後在背景再試幾次
    if remove_stale_exes() > 0 {
        std::thread::spawn(|| {
            for secs in [3, 15, 60, 300] {
                std::thread::sleep(Duration::from_secs(secs));
                if remove_stale_exes() == 0 {
                    break;
                }
            }
        });
    }

    let (cfg, cfg_err) = match Config::load(&paths.config_file) {
        Ok(c) => (c, None),
        Err(e) => {
            tracing::warn!("設定檔驗證失敗，使用預設值：{e}");
            (Config::defaults(), Some(e))
        }
    };
    let endpoint = transport::endpoint();
    // daemon 就是 agt 本身；Agent 的 hooks 用同一個執行檔呼叫 `agt notify`
    let agt_exe = std::env::current_exe()?;
    let agt_exe = std::fs::canonicalize(&agt_exe)
        .map(|p| agt_git::normalize(&p))
        .unwrap_or(agt_exe);
    if let Err(e) = agt_git::check_version() {
        tracing::warn!("{e}");
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(async move {
        let d = Daemon::new(paths, endpoint, agt_exe, cfg, cfg_err)?;
        serve(d).await
    });
    // 不無限等待背景的阻塞工作（Windows 的 named pipe 等待連線會佔住 blocking 執行緒）
    tracing::debug!("shutdown：關閉 runtime");
    rt.shutdown_timeout(Duration::from_secs(1));
    tracing::debug!("shutdown：runtime 已關閉");
    drop(lock);
    if let Err(e) = &result {
        tracing::error!("daemon 異常結束：{e:#}");
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}

async fn serve(d: Arc<Daemon>) -> anyhow::Result<()> {
    let listener = transport::listen(&d.endpoint)?;
    tracing::info!(
        "agt daemon {} 啟動，pid {}，監聽 {}",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        d.endpoint
    );

    let _watcher = watch_config(d.clone());

    // 啟動時的孤兒偵測（規格 7.5）：只記錄，讓使用者用 `agt gc` 決定
    {
        let d = d.clone();
        tokio::task::spawn_blocking(move || match d.gc(false) {
            Ok(items) if !items.is_empty() => {
                tracing::warn!(
                    "發現 {} 個 worktree 與資料庫不一致，請執行 `agt gc` 檢查",
                    items.len()
                )
            }
            Err(e) => tracing::warn!("孤兒偵測失敗：{}", e.message),
            _ => {}
        });
    }

    // 啟動時執行各 Agent 的更新指令（設定 `update`），各自在背景跑，不擋住啟動。
    // 相同的指令只執行一次：用 `extends` 繼承的 Agent 會沿用同一個 update。
    // 重複執行不會出錯（映像檔的 entrypoint 用鎖檔讓更新排隊，後面的發現已是最新版就略過），
    // 只是多跑一個容器、多查一次 npm。
    let mut updates: BTreeMap<Vec<String>, String> = BTreeMap::new();
    for (name, a) in d.cfg().agents.iter().filter(|(_, a)| !a.update.is_empty()) {
        updates
            .entry(a.update.clone())
            .or_insert_with(|| name.clone());
    }
    for (cmd, name) in updates {
        tokio::task::spawn_blocking(move || {
            tracing::info!("更新 Agent {name}：{cmd:?}");
            agt_core::platform::run_update(&cmd);
            tracing::info!("Agent {name} 的更新指令已結束");
        });
    }

    // git 資訊
    {
        let d = d.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(3));
            loop {
                t.tick().await;
                let d = d.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    d.poll_git();
                    d.check_long_running();
                })
                .await;
            }
        });
    }

    // 閒置自動關閉
    {
        let d = d.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(5));
            loop {
                t.tick().await;
                if d.should_idle_shutdown() {
                    tracing::info!("閒置逾時，自動關閉");
                    d.shutdown.notify_one();
                    break;
                }
            }
        });
    }

    let conns = Arc::new(AtomicUsize::new(0));
    loop {
        tokio::select! {
            conn = transport::accept(&listener) => match conn {
                Ok(c) => {
                    // 連線數上限：每條連線都有自己的佇列與 task，不設限的話卡住的 client 可以無限累積
                    if conns.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                        tracing::warn!("連線數已達上限 {MAX_CONNECTIONS}，拒絕新連線");
                        drop(c);
                        continue;
                    }
                    tracing::debug!("新連線");
                    let d = d.clone();
                    let conns = conns.clone();
                    conns.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        server::serve_conn(d, c).await;
                        conns.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(e) => {
                    tracing::warn!("accept 失敗：{e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            _ = d.shutdown.notified() => break,
        }
    }
    tracing::info!("daemon 結束");
    d.kill_all();
    tracing::debug!("shutdown：已結束所有 Agent");
    tokio::time::sleep(Duration::from_millis(200)).await;
    #[cfg(unix)]
    let _ = std::fs::remove_file(&d.endpoint);
    Ok(())
}

/// 更新執行檔時，執行中的 exe 無法覆蓋，會先被改名為 `<名稱>.old`（見 `docker/publish.sh`）。
/// 新版 daemon 啟動時順手刪掉同一個資料夾裡的這些舊檔；還在使用中（例如舊版 TUI 還開著）
/// 就刪不掉，由呼叫端稍後再試。回傳還留著（刪不掉）的檔案數。
fn remove_stale_exes() -> usize {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut remaining = 0;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // agt.exe.old、agtd.exe.old，以及撞名時的 agt.exe.<pid>.old
        let ours = ["agt", "agtd"]
            .iter()
            .any(|n| name.starts_with(&format!("{n}{}.", std::env::consts::EXE_SUFFIX)));
        if !(ours && name.ends_with(".old")) {
            continue;
        }
        let old = e.path();
        match std::fs::remove_file(&old) {
            Ok(()) => tracing::info!("已刪除更新留下的舊檔 {}", old.display()),
            Err(err) => {
                remaining += 1;
                tracing::info!("{} 仍在使用中，稍後再刪除：{err}", old.display());
            }
        }
    }
    remaining
}

/// 設定檔變更後自動重新載入（規格 9）。
fn watch_config(d: Arc<Daemon>) -> Option<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let dir = d.paths.config_file.parent()?.to_path_buf();
    let _ = std::fs::create_dir_all(&dir);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let name = d.paths.config_file.file_name()?.to_os_string();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && ev
                .paths
                .iter()
                .any(|p| p.file_name() == Some(name.as_os_str()))
        {
            let _ = tx.send(());
        }
    })
    .map_err(|e| tracing::warn!("無法監看設定檔：{e}"))
    .ok()?;
    if let Err(e) = w.watch(&dir, RecursiveMode::NonRecursive) {
        tracing::warn!("無法監看設定檔：{e}");
        return None;
    }
    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            tokio::time::sleep(Duration::from_millis(300)).await;
            while rx.try_recv().is_ok() {}
            d.reload_config();
        }
    });
    Some(w)
}
