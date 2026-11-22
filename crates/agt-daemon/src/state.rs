//! 工作階段管理：建立、查詢、結束工作階段；協調 Worktree 管理與資料儲存。

use crate::config::{Config, Paths};
use agt_core::{LaunchCtx, Session, SessionEvent, SessionOptions, StatusSource, platform};
use agt_git::GitError;
use agt_proto::sync::{MutexExt, RwLockExt};
use agt_proto::{
    AgentStatus, CreateSession, FinishAction, GcItem, MergeStrategy, ServerMsg, SessionInfo, codes,
};
use agt_store::{LOG_MAX_BYTES, LogWriter, SessionRow, Store};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

mod busy;
mod close;
mod create;
mod finish;
mod gc;
mod session;
mod usage;

#[derive(Debug)]
pub struct OpError {
    pub code: &'static str,
    pub message: String,
}

impl OpError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        OpError {
            code,
            message: message.into(),
        }
    }
    pub fn not_found(id: &str) -> Self {
        OpError::new(codes::NOT_FOUND, format!("找不到工作階段：{id}"))
    }
}

impl From<GitError> for OpError {
    fn from(e: GitError) -> Self {
        let code = match e {
            GitError::Conflict(_) => codes::CONFLICT,
            GitError::Dirty(_) => codes::BAD_REQUEST,
            _ => codes::GIT,
        };
        OpError::new(code, e.to_string())
    }
}

impl From<agt_store::StoreError> for OpError {
    fn from(e: agt_store::StoreError) -> Self {
        OpError::new(codes::INTERNAL, e.to_string())
    }
}

impl From<agt_core::Error> for OpError {
    fn from(e: agt_core::Error) -> Self {
        OpError::new(codes::INTERNAL, e.to_string())
    }
}

pub type OpResult<T> = Result<T, OpError>;

pub struct Entry {
    pub info: SessionInfo,
    pub session: Option<Arc<Session>>,
    /// 目前的對話 ID；None = 建立於支援接續對話之前的分頁（重新啟動時一律開新對話）。
    pub conversation: Option<String>,
    /// 第幾次啟動 Agent；結束事件用它分辨是哪一次啟動（前一次的 watcher 可能晚到）。
    pub generation: u64,
    /// 以 `resume_args` 接續對話的那次啟動（generation）與時間；
    /// 在 [`RESUME_GRACE`] 內以錯誤結束就改開新對話。
    pub resume_attempt: Option<(u64, Instant)>,
}

/// 已關閉分頁的事件與資料列保留多久。
const CLOSED_RETENTION_SECS: i64 = 30 * 24 * 3600;

/// 刪除不屬於任何開啟中分頁的終端機紀錄檔（`<id>.log`、`<id>.log.1`）。
/// 分頁關閉時本來就會刪；這裡處理 daemon 異常結束、或刪除時檔案還被鎖住而留下的檔案。
/// 回傳刪除的檔案數。
fn remove_orphan_logs(dir: &Path, open: &HashMap<String, Entry>) -> usize {
    let Ok(list) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for e in list.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(id) = name
            .strip_suffix(".log")
            .or_else(|| name.strip_suffix(".log.1"))
        else {
            continue;
        };
        // 只處理 agt 自己的檔名（UUID），不碰使用者放進來的其他檔案
        if uuid::Uuid::parse_str(id).is_err() || open.contains_key(id) {
            continue;
        }
        match std::fs::remove_file(e.path()) {
            Ok(()) => removed += 1,
            Err(err) => tracing::debug!("無法刪除紀錄檔 {name}：{err}"),
        }
    }
    removed
}

/// 分頁以 OSC 52 寫入剪貼簿的內容上限。
const CLIPBOARD_MAX: usize = 64 * 1024;

/// 接續對話後在這段時間內以非 0 結束，視為接續失敗（例如找不到對話），自動改開新對話。
const RESUME_GRACE: Duration = Duration::from_secs(60);

fn new_conversation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub struct Daemon {
    pub paths: Paths,
    pub endpoint: String,
    pub agt_exe: PathBuf,
    cfg: RwLock<Arc<Config>>,
    cfg_error: Mutex<Option<String>>,
    store: Mutex<Store>,
    entries: Mutex<HashMap<String, Entry>>,
    /// 讓「檢查舊 Agent 已結束 → 啟動新的 → 登記」成為一整步；
    /// 否則連按兩次重新啟動會在同一個 worktree 啟動兩個 Agent。
    restart_lock: Mutex<()>,
    /// 已經警告過「worktree 的 .git 被改動」的分頁（每個分頁只警告一次）。
    tamper_warned: Mutex<std::collections::HashSet<String>>,
    /// 連續忙碌（執行中／思考中）的計時，用來提醒長時間執行的分頁。
    busy: Mutex<busy::BusyTracker>,
    /// 每個分頁上次輪詢 git 的時間與當時的狀態（見 [`gc::should_poll_git`]）。
    git_polls: Mutex<HashMap<String, (Instant, AgentStatus)>>,
    /// 每個分頁上次讀到的 `.agt/usage.json` 修改時間（見 [`usage`]）。
    usage_seen: Mutex<HashMap<String, std::time::SystemTime>>,
    /// 用量達到 80% 的通知紀錄（每個分頁、每個額度時間窗只通知一次）。
    usage_alerts: Mutex<usage::AlertState>,
    pub events: broadcast::Sender<ServerMsg>,
    pub ui_clients: AtomicUsize,
    last_activity: Mutex<Instant>,
    pub shutdown: tokio::sync::Notify,
}

fn row_to_info(r: &SessionRow) -> SessionInfo {
    SessionInfo {
        id: r.id.clone(),
        short_id: r.short_id.clone(),
        agent: r.agent.clone(),
        repo_path: r.repo_path.clone(),
        repo_name: r.repo_name.clone(),
        task: r.task.clone(),
        worktree: r.worktree_path.clone(),
        branch: r.branch.clone(),
        base_branch: r.base_branch.clone(),
        cwd: r
            .cwd
            .clone()
            .or_else(|| r.worktree_path.clone())
            .unwrap_or_default(),
        status: r.status.parse().unwrap_or(AgentStatus::Exited),
        exit_code: r.exit_code,
        ahead: 0,
        behind: 0,
        dirty: false,
        created_at: r.created_at,
        status_since: agt_store::now(),
        shared: r.shared,
        tags: r.tags.clone(),
        usage: None,
    }
}

fn random_u64() -> u64 {
    let b = *uuid::Uuid::new_v4().as_bytes();
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

impl Daemon {
    pub fn new(
        paths: Paths,
        endpoint: String,
        agt_exe: PathBuf,
        cfg: Config,
        cfg_error: Option<String>,
    ) -> anyhow::Result<Arc<Daemon>> {
        let mut store = Store::open(&paths.db())?;
        platform::restrict_permissions(&paths.data_dir);
        let mut entries = HashMap::new();
        // 規格 10：daemon 重啟後無法接回已結束的 PTY，標為 Exited。
        for row in store.open_sessions()? {
            let mut info = row_to_info(&row);
            if info.status != AgentStatus::Exited && info.status != AgentStatus::Error {
                info.status = AgentStatus::Exited;
                store.update_status(&info.id, AgentStatus::Exited.as_str())?;
            }
            entries.insert(
                info.id.clone(),
                Entry {
                    info,
                    session: None,
                    conversation: row.conversation_id.clone(),
                    generation: 0,
                    resume_attempt: None,
                },
            );
        }
        // 已關閉分頁的事件與紀錄保留一段時間後清掉（可能含有 Agent 回報的內容）
        match store.prune_closed(agt_store::now() - CLOSED_RETENTION_SECS) {
            Ok(n) if n > 0 => tracing::info!("已清除 {n} 個關閉超過 30 天的分頁紀錄"),
            Ok(_) => {}
            Err(e) => tracing::warn!("清除舊分頁紀錄失敗：{e}"),
        }
        let removed = remove_orphan_logs(&paths.logs(), &entries);
        if removed > 0 {
            tracing::info!("已刪除 {removed} 個不屬於任何分頁的終端機紀錄檔");
        }
        let (events, _) = broadcast::channel(1024);
        Ok(Arc::new(Daemon {
            paths,
            endpoint,
            agt_exe,
            cfg: RwLock::new(Arc::new(cfg)),
            cfg_error: Mutex::new(cfg_error),
            store: Mutex::new(store),
            entries: Mutex::new(entries),
            restart_lock: Mutex::new(()),
            tamper_warned: Mutex::new(Default::default()),
            busy: Mutex::new(Default::default()),
            git_polls: Mutex::new(Default::default()),
            usage_seen: Mutex::new(Default::default()),
            usage_alerts: Mutex::new(Default::default()),
            events,
            ui_clients: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
            shutdown: tokio::sync::Notify::new(),
        }))
    }

    pub fn cfg(&self) -> Arc<Config> {
        self.cfg.read_or_recover().clone()
    }

    pub fn cfg_error(&self) -> Option<String> {
        self.cfg_error.lock_or_recover().clone()
    }

    /// 重新載入設定；失敗時保留上一份有效設定（規格 9）。已在執行的分頁不受影響。
    pub fn reload_config(&self) {
        let result = Config::load(&self.paths.config_file);
        let (err, ui) = match result {
            Ok(c) => {
                let ui = c.ui();
                *self.cfg.write_or_recover() = Arc::new(c);
                tracing::info!("設定已重新載入");
                (None, Some(ui))
            }
            Err(e) => {
                tracing::warn!("設定檔驗證失敗：{e}");
                (Some(e), None)
            }
        };
        *self.cfg_error.lock_or_recover() = err.clone();
        let _ = self.events.send(ServerMsg::ConfigStatus { error: err, ui });
    }

    pub fn touch(&self) {
        *self.last_activity.lock_or_recover() = Instant::now();
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock_or_recover()
    }

    fn record(&self, id: &str, kind: &str, data: serde_json::Value) {
        if let Err(e) = self.store().add_event(id, kind, Some(&data.to_string())) {
            tracing::warn!("寫入 events 失敗：{e}");
        }
    }

    /// 接受完整 UUID、短 id 或唯一的前綴。
    pub fn resolve(&self, id: &str) -> OpResult<String> {
        let entries = self.entries.lock_or_recover();
        if entries.contains_key(id) {
            return Ok(id.to_string());
        }
        let matches: Vec<&String> = entries
            .iter()
            .filter(|(k, e)| e.info.short_id == id || (id.len() >= 3 && k.starts_with(id)))
            .map(|(k, _)| k)
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(OpError::not_found(id)),
            _ => Err(OpError::new(codes::BAD_REQUEST, format!("id {id} 不唯一"))),
        }
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut v: Vec<SessionInfo> = self
            .entries
            .lock_or_recover()
            .values()
            .map(|e| e.info.clone())
            .collect();
        v.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        v
    }

    pub fn get(&self, id: &str) -> OpResult<SessionInfo> {
        let id = self.resolve(id)?;
        self.entries
            .lock_or_recover()
            .get(&id)
            .map(|e| e.info.clone())
            .ok_or_else(|| OpError::not_found(&id))
    }

    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.entries
            .lock_or_recover()
            .get(id)
            .and_then(|e| e.session.clone())
    }

    pub fn running_count(&self) -> usize {
        self.entries
            .lock_or_recover()
            .values()
            .filter(|e| e.session.as_ref().is_some_and(|s| s.is_running()))
            .count()
    }

    fn update_info(&self, id: &str, f: impl FnOnce(&mut SessionInfo)) -> Option<SessionInfo> {
        let mut entries = self.entries.lock_or_recover();
        let e = entries.get_mut(id)?;
        let before = e.info.status;
        f(&mut e.info);
        e.info.note_status_change(before);
        Some(e.info.clone())
    }

    /// 只在分頁仍是第 `generation` 次啟動時更新。重新啟動後，前一次啟動的事件可能晚到，
    /// 不能讓它把新 Agent 的狀態改回 Exited。分頁已移除或已重新啟動時回傳 None。
    fn update_if_current(
        &self,
        id: &str,
        generation: u64,
        f: impl FnOnce(&mut SessionInfo),
    ) -> Option<SessionInfo> {
        let mut entries = self.entries.lock_or_recover();
        let e = entries.get_mut(id).filter(|e| e.generation == generation)?;
        let before = e.info.status;
        f(&mut e.info);
        e.info.note_status_change(before);
        Some(e.info.clone())
    }

    fn publish(&self, info: SessionInfo) {
        let _ = self
            .events
            .send(ServerMsg::SessionUpdated { session: info });
    }

    /// 所有工作階段結束且沒有 client 連線，經過 `idle_shutdown_secs` 後回傳 true。
    pub fn should_idle_shutdown(&self) -> bool {
        let secs = self.cfg().general.idle_shutdown_secs;
        secs > 0
            && self.ui_clients.load(Ordering::Relaxed) == 0
            && self.running_count() == 0
            && self.last_activity.lock_or_recover().elapsed() >= Duration::from_secs(secs)
    }

    pub fn kill_all(&self) {
        let sessions: Vec<Arc<Session>> = self
            .entries
            .lock_or_recover()
            .values()
            .filter_map(|e| e.session.clone())
            .collect();
        for s in sessions {
            if s.is_running() {
                s.kill();
            }
        }
    }

    pub fn info(&self) -> agt_proto::DaemonInfo {
        // 注意：不能在同一個敘述裡兩次鎖 entries（暫時的 guard 會活到敘述結束 → 死結）
        let sessions = self.entries.lock_or_recover().len();
        let running = self.running_count();
        agt_proto::DaemonInfo {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            sessions,
            running,
            clients: self.ui_clients.load(Ordering::Relaxed),
            data_dir: self.paths.data_dir.to_string_lossy().into_owned(),
            config_path: self.paths.config_file.to_string_lossy().into_owned(),
        }
    }
}

/// merge / pr 前：branch 改到會在主機或 CI 上自動執行的設定檔時，要求使用者確認（`force`）。
/// Agent 在沙箱裡改不到主機，但這些檔案合併後就會在沙箱外生效。
fn confirm_sensitive(
    repo: &str,
    wt: &str,
    base: &str,
    extra: &[String],
    what: &str,
    force: bool,
) -> OpResult<()> {
    if force {
        return Ok(());
    }
    let files = agt_git::sensitive_changes(Path::new(repo), Path::new(wt), base, extra)?;
    if files.is_empty() {
        return Ok(());
    }
    const MAX: usize = 12;
    let mut list: Vec<String> = files.iter().take(MAX).map(|f| format!("  {f}")).collect();
    if files.len() > MAX {
        list.push(format!("  …（共 {} 個）", files.len()));
    }
    Err(OpError::new(
        codes::NEEDS_CONFIRM,
        format!(
            "這個分頁修改了合併後會自動執行的設定檔（hooks、CI、setup 等）：\n{}\n\n請先用 diff 確認內容。確定要{what}嗎？",
            list.join("\n")
        ),
    ))
}

/// 還有尚未關閉的分頁使用這個 worktree（例如共用者還開著）。
fn in_use(rows: &[&SessionRow], me: &SessionRow, path: &str) -> bool {
    rows.iter().any(|o| {
        o.id != me.id
            && o.ended_at.is_none()
            && o.worktree_path
                .as_deref()
                .is_some_and(|q| agt_git::same_path(Path::new(q), Path::new(path)))
    })
}

fn need_worktree(info: &SessionInfo) -> OpResult<(String, String, String, String)> {
    match (
        &info.worktree,
        &info.branch,
        &info.base_branch,
        &info.repo_path,
    ) {
        (Some(w), Some(b), Some(base), Some(r)) => {
            Ok((w.clone(), b.clone(), base.clone(), r.clone()))
        }
        _ => Err(OpError::new(codes::BAD_REQUEST, "這個分頁沒有 worktree")),
    }
}

fn rollback(created: &Option<(PathBuf, PathBuf, String)>) {
    if let Some((repo, wt, branch)) = created {
        if let Err(e) = agt_git::remove_worktree(repo, wt, true) {
            tracing::warn!("回滾 worktree 失敗：{e}");
        }
        if let Err(e) = agt_git::delete_branch(repo, branch) {
            tracing::warn!("回滾 branch 失敗：{e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_logs_are_removed() {
        let d = tempfile::tempdir().unwrap();
        let open_id = uuid::Uuid::new_v4().to_string();
        let gone_id = uuid::Uuid::new_v4().to_string();
        for name in [
            format!("{open_id}.log"),
            format!("{gone_id}.log"),
            format!("{gone_id}.log.1"),
            "notes.log".to_string(), // 不是 agt 的檔名：不動
        ] {
            std::fs::write(d.path().join(name), "x").unwrap();
        }
        let mut open = HashMap::new();
        open.insert(
            open_id.clone(),
            Entry {
                info: row_to_info(&SessionRow::default()),
                session: None,
                conversation: None,
                generation: 0,
                resume_attempt: None,
            },
        );
        assert_eq!(remove_orphan_logs(d.path(), &open), 2);
        assert!(d.path().join(format!("{open_id}.log")).exists());
        assert!(d.path().join("notes.log").exists());
        assert!(!d.path().join(format!("{gone_id}.log")).exists());
        assert_eq!(remove_orphan_logs(&d.path().join("missing"), &open), 0);
    }

    #[test]
    fn stale_generation_cannot_overwrite_restarted_tab() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_file: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            home: dir.path().to_path_buf(),
            logs_dir: None,
        };
        let d = Daemon::new(paths, "test".into(), "agt".into(), Config::defaults(), None).unwrap();
        let mut info = row_to_info(&SessionRow {
            id: "s1".into(),
            short_id: "aaaaaa".into(),
            status: "running".into(),
            ..Default::default()
        });
        info.status = AgentStatus::Running;
        d.entries.lock_or_recover().insert(
            "s1".into(),
            Entry {
                info,
                session: None,
                conversation: None,
                generation: 2, // 已重新啟動過一次
                resume_attempt: None,
            },
        );
        // 第 1 次啟動的結束事件晚到：不能改到目前（第 2 次）的狀態
        assert!(
            d.update_if_current("s1", 1, |i| i.status = AgentStatus::Exited)
                .is_none()
        );
        assert_eq!(d.get("s1").unwrap().status, AgentStatus::Running);
        // 目前這一次的事件照常套用
        assert!(
            d.update_if_current("s1", 2, |i| i.status = AgentStatus::Done)
                .is_some()
        );
        assert_eq!(d.get("s1").unwrap().status, AgentStatus::Done);
        // 分頁已移除
        assert!(d.update_if_current("nope", 2, |_| {}).is_none());
    }

    #[test]
    fn status_since_changes_only_when_status_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_file: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            home: dir.path().to_path_buf(),
            logs_dir: None,
        };
        let d = Daemon::new(paths, "test".into(), "agt".into(), Config::defaults(), None).unwrap();
        let mut info = row_to_info(&SessionRow {
            id: "s1".into(),
            short_id: "aaaaaa".into(),
            status: "running".into(),
            ..Default::default()
        });
        info.status_since = 1;
        d.entries.lock_or_recover().insert(
            "s1".into(),
            Entry {
                info,
                session: None,
                conversation: None,
                generation: 1,
                resume_attempt: None,
            },
        );
        // 狀態沒變：維持原值
        d.update_if_current("s1", 1, |i| i.exit_code = Some(0));
        assert_eq!(d.get("s1").unwrap().status_since, 1);
        // 狀態改變：更新成現在
        d.update_if_current("s1", 1, |i| i.status = AgentStatus::WaitingInput);
        assert!(d.get("s1").unwrap().status_since > 1);
    }
}
