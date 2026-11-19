//! 工作階段管理：建立、查詢、結束工作階段；協調 Worktree 管理與資料儲存。

use crate::config::{Config, Paths};
use agt_core::{LaunchCtx, Session, SessionEvent, SessionOptions, StatusSource, platform};
use agt_git::GitError;
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
        shared: r.shared,
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
            events,
            ui_clients: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
            shutdown: tokio::sync::Notify::new(),
        }))
    }

    pub fn cfg(&self) -> Arc<Config> {
        self.cfg.read().unwrap().clone()
    }

    pub fn cfg_error(&self) -> Option<String> {
        self.cfg_error.lock().unwrap().clone()
    }

    /// 重新載入設定；失敗時保留上一份有效設定（規格 9）。已在執行的分頁不受影響。
    pub fn reload_config(&self) {
        let result = Config::load(&self.paths.config_file);
        let (err, ui) = match result {
            Ok(c) => {
                let ui = c.ui();
                *self.cfg.write().unwrap() = Arc::new(c);
                tracing::info!("設定已重新載入");
                (None, Some(ui))
            }
            Err(e) => {
                tracing::warn!("設定檔驗證失敗：{e}");
                (Some(e), None)
            }
        };
        *self.cfg_error.lock().unwrap() = err.clone();
        let _ = self.events.send(ServerMsg::ConfigStatus { error: err, ui });
    }

    pub fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap()
    }

    fn record(&self, id: &str, kind: &str, data: serde_json::Value) {
        if let Err(e) = self.store().add_event(id, kind, Some(&data.to_string())) {
            tracing::warn!("寫入 events 失敗：{e}");
        }
    }

    /// 接受完整 UUID、短 id 或唯一的前綴。
    pub fn resolve(&self, id: &str) -> OpResult<String> {
        let entries = self.entries.lock().unwrap();
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
            .lock()
            .unwrap()
            .values()
            .map(|e| e.info.clone())
            .collect();
        v.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        v
    }

    pub fn get(&self, id: &str) -> OpResult<SessionInfo> {
        let id = self.resolve(id)?;
        self.entries
            .lock()
            .unwrap()
            .get(&id)
            .map(|e| e.info.clone())
            .ok_or_else(|| OpError::not_found(&id))
    }

    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .and_then(|e| e.session.clone())
    }

    pub fn running_count(&self) -> usize {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.session.as_ref().is_some_and(|s| s.is_running()))
            .count()
    }

    fn update_info(&self, id: &str, f: impl FnOnce(&mut SessionInfo)) -> Option<SessionInfo> {
        let mut entries = self.entries.lock().unwrap();
        let e = entries.get_mut(id)?;
        f(&mut e.info);
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
        let mut entries = self.entries.lock().unwrap();
        let e = entries.get_mut(id).filter(|e| e.generation == generation)?;
        f(&mut e.info);
        Some(e.info.clone())
    }

    fn publish(&self, info: SessionInfo) {
        let _ = self
            .events
            .send(ServerMsg::SessionUpdated { session: info });
    }

    // ───────────────────────── 建立 ─────────────────────────

    /// 建立工作階段（規格 7.1）。任何一步失敗都會回滾已完成的步驟（setup 除外）。
    pub fn create(self: &Arc<Self>, req: CreateSession) -> OpResult<SessionInfo> {
        let cfg = self.cfg();
        let settings = cfg.agents.get(&req.agent).cloned().ok_or_else(|| {
            OpError::new(
                codes::BAD_REQUEST,
                format!(
                    "未知的 Agent：{}（可用：{}）",
                    req.agent,
                    cfg.agents.keys().cloned().collect::<Vec<_>>().join("、")
                ),
            )
        })?;
        let dir = PathBuf::from(&req.repo);
        if !dir.is_dir() {
            return Err(OpError::new(
                codes::BAD_REQUEST,
                format!("目錄不存在：{}", req.repo),
            ));
        }
        let dir = agt_git::normalize(&dir);
        let id = uuid::Uuid::new_v4().to_string();
        let short_id = loop {
            let s = agt_git::short_id(random_u64());
            if !self.store().short_id_exists(&s)? {
                break s;
            }
        };
        let task = if req.task.trim().is_empty() {
            req.agent.clone()
        } else {
            req.task.trim().to_string()
        };

        let mut info = SessionInfo {
            id: id.clone(),
            short_id: short_id.clone(),
            agent: req.agent.clone(),
            repo_path: None,
            repo_name: None,
            task: task.clone(),
            worktree: None,
            branch: None,
            base_branch: None,
            cwd: dir.to_string_lossy().into_owned(),
            status: AgentStatus::Starting,
            exit_code: None,
            ahead: 0,
            behind: 0,
            dirty: false,
            created_at: agt_store::now(),
            shared: false,
        };
        let mut created_wt: Option<(PathBuf, PathBuf, String)> = None; // (repo, worktree, branch)
        let mut setup: Option<String> = None;

        if let Some(share) = &req.share {
            // 共用 worktree：只有明確加上 --share 才允許（規格 7.5）
            let other = self.get(share)?;
            let Some(wt) = other.worktree.clone() else {
                return Err(OpError::new(
                    codes::BAD_REQUEST,
                    format!("工作階段 {share} 沒有 worktree 可以共用"),
                ));
            };
            info.repo_path = other.repo_path.clone();
            info.repo_name = other.repo_name.clone();
            info.worktree = Some(wt.clone());
            info.branch = other.branch.clone();
            info.base_branch = other.base_branch.clone();
            info.cwd = wt;
            info.shared = true;
        } else {
            match agt_git::repo_info(&dir)? {
                None => {
                    // 不是 git repo：退化成一般分頁
                    info.repo_name = dir.file_name().map(|s| s.to_string_lossy().into_owned());
                    info.repo_path = Some(dir.to_string_lossy().into_owned());
                }
                Some(repo) => {
                    let root_s = repo.root.to_string_lossy().into_owned();
                    info.repo_path = Some(root_s.clone());
                    info.repo_name = Some(repo.name.clone());
                    let rcfg = cfg
                        .repo(&self.paths, &repo.root)
                        .map_err(|e| OpError::new(codes::BAD_REQUEST, e))?;
                    if settings.worktree {
                        let base = req
                            .base_branch
                            .clone()
                            .or(rcfg.base_branch.clone())
                            .or(repo.head_branch.clone())
                            .ok_or_else(|| {
                                OpError::new(
                                    codes::BAD_REQUEST,
                                    "目前是 detached HEAD，請用 --base 指定 base branch",
                                )
                            })?;
                        if !agt_git::ref_exists(&repo.root, &base) {
                            return Err(OpError::new(
                                codes::BAD_REQUEST,
                                format!("找不到 base branch：{base}"),
                            ));
                        }
                        // setup 來自 repo，第一次執行前要求使用者確認（規格 11.4）
                        if let Some(s) = rcfg.setup.clone().filter(|s| !s.trim().is_empty()) {
                            let trusted = self.store().is_setup_trusted(&root_s, &s)?;
                            if !trusted && !req.trust_setup {
                                return Err(OpError::new(codes::SETUP_UNTRUSTED, s));
                            }
                            if !trusted {
                                self.store().trust_setup(&root_s, &s)?;
                            }
                            setup = Some(s);
                        }
                        let branch = agt_git::branch_name(&req.agent, &task, &short_id);
                        let root = self.paths.expand(&cfg.general.worktree_root);
                        let wt = root
                            .join(agt_git::repo_dir_name(&repo.root))
                            .join(&short_id);
                        if platform::path_too_long(&wt) && !agt_git::longpaths_enabled(&repo.root) {
                            return Err(OpError::new(
                                codes::BAD_REQUEST,
                                format!(
                                    "worktree 路徑可能超過 Windows 的 260 字元限制：{}\n請執行 `git config core.longpaths true` 或在設定檔改用較短的 worktree_root",
                                    wt.display()
                                ),
                            ));
                        }
                        agt_git::add_worktree(&repo.root, &wt, &branch, &base)?;
                        let wt = agt_git::normalize(&wt);
                        created_wt = Some((repo.root.clone(), wt.clone(), branch.clone()));
                        if let Err(e) = agt_git::copy_files(&repo.root, &wt, &rcfg.copy_files) {
                            rollback(&created_wt);
                            return Err(e.into());
                        }
                        info.worktree = Some(wt.to_string_lossy().into_owned());
                        info.branch = Some(branch);
                        info.base_branch = Some(base);
                        info.cwd = wt.to_string_lossy().into_owned();
                    }
                }
            }
        }

        // 新分頁先執行更新指令（例如把 claude-docker 的 Claude Code 更新到最新版），再執行 repo 的 setup
        if settings.update_on_new {
            if let Some(u) = platform::update_script(&settings.update) {
                setup = Some(match setup {
                    Some(s) => format!("{u}\n{s}"),
                    None => u,
                });
            }
        }

        let conversation = new_conversation_id();
        let (session, events) = match self.start_session(
            &info,
            &conversation,
            false,
            false,
            setup.as_deref(),
            req.cols,
            req.rows,
        ) {
            Ok(s) => s,
            Err(e) => {
                rollback(&created_wt);
                return Err(e);
            }
        };

        let row = SessionRow {
            id: info.id.clone(),
            short_id: info.short_id.clone(),
            agent: info.agent.clone(),
            repo_path: info.repo_path.clone(),
            repo_name: info.repo_name.clone(),
            task: info.task.clone(),
            worktree_path: info.worktree.clone(),
            branch: info.branch.clone(),
            base_branch: info.base_branch.clone(),
            status: info.status.as_str().into(),
            pid: session.pid(),
            created_at: info.created_at,
            cwd: Some(info.cwd.clone()),
            shared: info.shared,
            conversation_id: Some(conversation.clone()),
            ..Default::default()
        };
        if let Err(e) = self.store().insert_session(&row) {
            session.kill();
            rollback(&created_wt);
            return Err(e.into());
        }
        self.record(
            &info.id,
            "status",
            json!({"to": "starting", "source": "daemon"}),
        );
        self.entries.lock().unwrap().insert(
            info.id.clone(),
            Entry {
                info: info.clone(),
                session: Some(session.clone()),
                conversation: Some(conversation),
                generation: 1,
                resume_attempt: None,
            },
        );
        self.watch(info.id.clone(), 1, session, events);
        self.publish(info.clone());
        self.touch();
        Ok(info)
    }

    fn launch_ctx(
        &self,
        info: &SessionInfo,
        settings: &agt_agents::AgentSettings,
        conversation: &str,
        resume: bool,
        restarted: bool,
    ) -> LaunchCtx {
        // 只有設定用到 {gitdir}（要把主 repo 的 .git 掛進容器 / 沙箱）時，
        // 才把 worktree 的 .git 改成相對路徑並取得主 repo 的 .git 位置
        let wants_gitdir = settings
            .args
            .iter()
            .chain(&settings.restart_args)
            .chain(&settings.new_args)
            .chain(&settings.resume_args)
            .any(|a| a.contains("{gitdir}"));
        // 重新啟動時 worktree 的 .git 可能被上一次的 Agent 改過：先確認它指向主 repo 登記的 gitdir，
        // 否則 {gitdir} 會把 Agent 指定的任意目錄掛進新的沙箱。
        let trusted = info.repo_path.as_deref().is_some_and(|repo| {
            match agt_git::trusted_gitdir(Path::new(repo), Path::new(&info.cwd)) {
                Ok(_) => true,
                Err(e) => {
                    tracing::warn!("不掛載 {{gitdir}}：{e}");
                    false
                }
            }
        });
        let gitdir = if wants_gitdir && info.worktree.is_some() && trusted {
            agt_git::prepare_container_gitdir(Path::new(&info.cwd))
        } else {
            None
        };
        LaunchCtx {
            session_id: info.id.clone(),
            cwd: PathBuf::from(&info.cwd),
            agt_exe: self.agt_exe.clone(),
            socket: self.endpoint.clone(),
            conversation_id: conversation.to_string(),
            resume,
            restarted,
            gitdir,
        }
    }

    /// 關閉分頁時執行 Agent 設定的 `on_close`（例如刪除沙箱），在背景執行、不阻塞。
    fn run_on_close(&self, id: &str) {
        let Some(info) = self.entries.lock().unwrap().get(id).map(|e| e.info.clone()) else {
            return;
        };
        let cfg = self.cfg();
        let Some(settings) = cfg.agents.get(&info.agent) else {
            return;
        };
        if settings.on_close.is_empty() {
            return;
        }
        let ctx = LaunchCtx {
            session_id: info.id.clone(),
            cwd: PathBuf::from(&info.cwd),
            agt_exe: self.agt_exe.clone(),
            socket: self.endpoint.clone(),
            conversation_id: String::new(),
            resume: false,
            restarted: false,
            gitdir: None,
        };
        let cmd = agt_agents::on_close_command(settings, &ctx);
        tracing::debug!(%id, ?cmd, "執行 on_close");
        let _ = std::thread::Builder::new()
            .name("agt-on-close".into())
            .spawn(move || platform::run_cleanup(&cmd));
    }

    /// 安裝 hooks 並在 cwd 啟動 Agent。`resume` = 以 `resume_args` 接續 `conversation`；
    /// `restarted` = 這個分頁之前啟動過（`R` / `N` 重新啟動），有設定 `restart_args` 時用它取代 `args`。
    #[allow(clippy::too_many_arguments)]
    fn start_session(
        &self,
        info: &SessionInfo,
        conversation: &str,
        resume: bool,
        restarted: bool,
        setup: Option<&str>,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> OpResult<(Arc<Session>, broadcast::Receiver<SessionEvent>)> {
        let cfg = self.cfg();
        let settings = cfg.agents.get(&info.agent).cloned().ok_or_else(|| {
            OpError::new(codes::BAD_REQUEST, format!("未知的 Agent：{}", info.agent))
        })?;
        let adapter = agt_agents::build(&info.agent, &settings)
            .map_err(|e| OpError::new(codes::BAD_REQUEST, e))?;
        let ctx = self.launch_ctx(info, &settings, conversation, resume, restarted);
        adapter.install_hooks(&ctx)?;
        let mut spec = adapter.launch(&ctx);
        if let Some(s) = setup {
            spec = platform::wrap_with_setup(s, spec);
        }
        let log: Option<Box<dyn std::io::Write + Send>> = if cfg.general.log_output {
            match LogWriter::open(&self.paths.logs(), &info.id, LOG_MAX_BYTES) {
                Ok(w) => Some(Box::new(w)),
                Err(e) => {
                    tracing::warn!("無法開啟紀錄檔：{e}");
                    None
                }
            }
        } else {
            None
        };
        let started = Session::spawn(SessionOptions {
            id: info.id.clone(),
            cols: cols.unwrap_or(100),
            rows: rows.unwrap_or(30),
            scrollback: cfg.general.scrollback_lines,
            spec,
            adapter,
            log,
        })
        .map_err(|e| OpError::new(codes::BAD_REQUEST, e.to_string()))?;
        Ok(started)
    }

    /// 監聽工作階段事件：更新資料庫、推播 `StatusChanged`、發出系統通知。
    fn watch(
        self: &Arc<Self>,
        id: String,
        generation: u64,
        session: Arc<Session>,
        mut rx: broadcast::Receiver<SessionEvent>,
    ) {
        let d = self.clone();
        tokio::spawn(async move {
            loop {
                let ev = match rx.recv().await {
                    Ok(ev) => ev,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                match ev {
                    SessionEvent::Status {
                        from,
                        to,
                        source,
                        message,
                    } => {
                        let Some(info) = d.update_if_current(&id, generation, |i| i.status = to)
                        else {
                            break; // 分頁已關閉，或已重新啟動（這是舊 Agent 的事件）
                        };
                        if let Err(e) = d.store().update_status(&id, to.as_str()) {
                            tracing::warn!("更新狀態失敗：{e}");
                        }
                        d.record(
                            &id,
                            "status",
                            json!({
                                "from": from.as_str(), "to": to.as_str(),
                                "source": format!("{source:?}").to_lowercase(), "message": message,
                            }),
                        );
                        let _ = d.events.send(ServerMsg::StatusChanged {
                            id: id.clone(),
                            from,
                            to,
                        });
                        // 使用者目前正在看的分頁不發系統通知
                        if matches!(to, AgentStatus::WaitingInput | AgentStatus::Done)
                            && d.cfg().general.notify
                            && session.viewers() == 0
                        {
                            let what = if to == AgentStatus::WaitingInput {
                                "等待你的回覆"
                            } else {
                                "已完成"
                            };
                            let body = message.unwrap_or_else(|| format!("{} {what}", info.agent));
                            platform::notify(&format!("agt · {}", info.task), &body);
                        }
                    }
                    SessionEvent::Notify { title, body } => {
                        d.record(
                            &id,
                            "notify",
                            json!({"title": title, "body": body, "source": "osc"}),
                        );
                    }
                    SessionEvent::Title(_) => {}
                    SessionEvent::Clipboard(text) => {
                        // 上限 64 KiB：一般複製用不到更多，也避免異常輸出塞爆連線。
                        // 設定為 off 時直接丟棄；ask / on 由 TUI 處理（ask 先詢問使用者）。
                        if text.len() <= CLIPBOARD_MAX && d.cfg().general.osc52 != "off" {
                            let _ = d.events.send(ServerMsg::Clipboard {
                                id: id.clone(),
                                text,
                            });
                        }
                    }
                    SessionEvent::Exited { code } => {
                        let st = session.status();
                        if let Some(info) = d.update_if_current(&id, generation, |i| {
                            i.exit_code = code;
                            i.status = st;
                        }) {
                            let _ = d.store().update_process(&id, session.pid(), code);
                            let _ = d.store().update_status(&id, st.as_str());
                            d.publish(info);
                        }
                        d.touch();
                        if d.resume_failed(&id, generation, code) {
                            tracing::info!("{id}：接續對話失敗（exit {code:?}），改開新對話");
                            d.record(
                                &id,
                                "status",
                                json!({"to": "starting", "source": "resume_fallback"}),
                            );
                            let d = d.clone();
                            tokio::task::spawn_blocking(move || {
                                if let Err(e) = d.restart(&id, true) {
                                    tracing::warn!("{id}：改開新對話失敗：{}", e.message);
                                }
                            });
                        }
                        break;
                    }
                }
            }
        });
    }

    // ───────────────────────── 操作 ─────────────────────────

    /// hooks 回報狀態（`agt notify`）。
    pub fn notify(
        &self,
        id: &str,
        status: AgentStatus,
        message: Option<String>,
        tool: Option<String>,
    ) -> OpResult<()> {
        let id = self.resolve(id)?;
        let session = self.session(&id).ok_or_else(|| OpError::not_found(&id))?;
        if tool.is_some() || message.is_some() {
            self.record(
                &id,
                "notify",
                json!({"status": status.as_str(), "tool": tool, "message": message}),
            );
        }
        session.set_status(status, StatusSource::Hook, message);
        Ok(())
    }

    pub fn rename(&self, id: &str, task: &str) -> OpResult<SessionInfo> {
        let id = self.resolve(id)?;
        let task = task.trim();
        if task.is_empty() {
            return Err(OpError::new(codes::BAD_REQUEST, "任務名稱不能是空的"));
        }
        self.store().rename(&id, task)?;
        let info = self
            .update_info(&id, |i| i.task = task.into())
            .ok_or_else(|| OpError::not_found(&id))?;
        self.publish(info.clone());
        Ok(info)
    }

    /// Agent 結束時呼叫：剛以 `resume_args` 接續、很快就以錯誤結束 → true（應改開新對話）。
    fn resume_failed(&self, id: &str, generation: u64, code: Option<i32>) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let Some(e) = entries.get_mut(id) else {
            return false;
        };
        match e.resume_attempt {
            Some((g, t)) if g == generation => {
                e.resume_attempt = None;
                t.elapsed() < RESUME_GRACE && code != Some(0)
            }
            _ => false,
        }
    }

    /// 在同一個 worktree 重新啟動 Agent。Agent 設定了 `resume_args` 時接續上次的對話；
    /// `fresh` 或是沒有可接續的對話時開新對話。
    pub fn restart(self: &Arc<Self>, id: &str, fresh: bool) -> OpResult<SessionInfo> {
        let _one_at_a_time = self.restart_lock.lock().unwrap_or_else(|e| e.into_inner());
        let id = self.resolve(id)?;
        let (info, old, conversation) = {
            let entries = self.entries.lock().unwrap();
            let e = entries.get(&id).ok_or_else(|| OpError::not_found(&id))?;
            (e.info.clone(), e.session.clone(), e.conversation.clone())
        };
        let can_resume = self
            .cfg()
            .agents
            .get(&info.agent)
            .is_some_and(|a| !a.resume_args.is_empty());
        let (conversation, resume) = match conversation {
            Some(c) if can_resume && !fresh => (c, true),
            _ => (new_conversation_id(), false),
        };
        if old.as_ref().is_some_and(|s| s.is_running()) {
            return Err(OpError::new(codes::BAD_REQUEST, "Agent 仍在執行中"));
        }
        if !Path::new(&info.cwd).is_dir() {
            return Err(OpError::new(
                codes::BAD_REQUEST,
                format!("工作目錄已不存在：{}", info.cwd),
            ));
        }
        let (cols, rows) = old.map(|s| s.size()).unzip();
        // 先登記這次啟動，再啟動程序：程序可能立刻結束
        let generation = {
            let mut entries = self.entries.lock().unwrap();
            let e = entries
                .get_mut(&id)
                .ok_or_else(|| OpError::not_found(&id))?;
            e.generation += 1;
            e.resume_attempt = resume.then(|| (e.generation, Instant::now()));
            e.generation
        };
        let (session, events) =
            self.start_session(&info, &conversation, resume, true, None, cols, rows)?;
        let info = {
            let mut entries = self.entries.lock().unwrap();
            let e = entries
                .get_mut(&id)
                .ok_or_else(|| OpError::not_found(&id))?;
            e.session = Some(session.clone());
            e.info.status = AgentStatus::Starting;
            e.info.exit_code = None;
            e.conversation = Some(conversation.clone());
            e.info.clone()
        };
        let _ = self.store().update_process(&id, session.pid(), None);
        let _ = self.store().update_status(&id, "starting");
        if !resume {
            let _ = self.store().set_conversation(&id, &conversation);
        }
        self.record(
            &id,
            "status",
            json!({"to": "starting", "source": "restart", "resume": resume}),
        );
        self.watch(id, generation, session, events);
        self.publish(info.clone());
        Ok(info)
    }

    fn remove_entry(&self, id: &str, finish_action: Option<&str>) -> OpResult<()> {
        self.store().end_session(id, finish_action)?;
        self.run_on_close(id);
        self.entries.lock().unwrap().remove(id);
        self.tamper_warned.lock().unwrap().remove(id);
        // 分頁關閉後紀錄檔就沒有人會看了（每個最多約 100 MB，且可能含有機密）。
        // Windows 上 Agent 剛被結束時檔案可能還開著，刪不掉就留著，只記錄。
        let logs = self.paths.logs();
        for name in [format!("{id}.log"), format!("{id}.log.1")] {
            match std::fs::remove_file(logs.join(&name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::debug!("無法刪除紀錄檔 {name}：{e}"),
            }
        }
        let _ = self
            .events
            .send(ServerMsg::SessionRemoved { id: id.to_string() });
        self.touch();
        Ok(())
    }

    fn others_share(&self, id: &str, worktree: &str) -> bool {
        self.entries
            .lock()
            .unwrap()
            .values()
            .any(|e| e.info.id != id && e.info.worktree.as_deref() == Some(worktree))
    }

    /// discard 前的安全檢查（規格 7.4）。
    fn check_discard(&self, info: &SessionInfo, force: bool) -> OpResult<()> {
        let (Some(wt), Some(base)) = (&info.worktree, &info.base_branch) else {
            return Ok(());
        };
        if self.others_share(&info.id, wt) {
            return Err(OpError::new(
                codes::BAD_REQUEST,
                "還有其他分頁共用這個 worktree，不能移除",
            ));
        }
        if force || !Path::new(wt).exists() {
            return Ok(());
        }
        let Some(repo) = &info.repo_path else {
            return Ok(());
        };
        let (repo, wt) = (Path::new(repo), Path::new(wt));
        // .git 被改動過：無法檢查內容，但仍要讓使用者能確認後丟棄
        if let Err(e) = agt_git::trusted_gitdir(repo, wt) {
            return Err(OpError::new(
                codes::NEEDS_CONFIRM,
                format!("{e}\n\n無法檢查是否有未 commit 的變更，確定要丟棄嗎？"),
            ));
        }
        let dirty = agt_git::is_dirty_wt(repo, wt, true)?;
        let unpushed = agt_git::has_unpushed(repo, wt, base)?;
        if dirty || unpushed {
            let mut why = vec![];
            if dirty {
                why.push("未 commit 的變更");
            }
            if unpushed {
                why.push("未 push / 未合併的 commit");
            }
            return Err(OpError::new(
                codes::NEEDS_CONFIRM,
                format!("worktree 有{}，確定要丟棄嗎？", why.join("與")),
            ));
        }
        Ok(())
    }

    fn discard_worktree(&self, info: &SessionInfo) -> OpResult<()> {
        if info.shared {
            return Ok(()); // 共用者不負責移除
        }
        let (Some(wt), Some(repo)) = (&info.worktree, &info.repo_path) else {
            return Ok(());
        };
        agt_git::remove_worktree(Path::new(repo), Path::new(wt), true)?;
        if let Some(b) = &info.branch {
            agt_git::delete_branch(Path::new(repo), b)?;
        }
        self.store().clear_worktree(&info.id)?;
        Ok(())
    }

    fn stop_agent(&self, id: &str) {
        if let Some(s) = self.session(id)
            && s.is_running()
        {
            s.kill();
        }
    }

    /// 關閉分頁。`cleanup` 時一併移除 worktree（需通過 discard 的安全檢查）。
    pub fn kill(&self, id: &str, cleanup: bool, force: bool) -> OpResult<()> {
        let id = self.resolve(id)?;
        let info = self.get(&id)?;
        if cleanup {
            self.check_discard(&info, force)?;
        }
        tracing::debug!(%id, "kill：結束 Agent");
        self.stop_agent(&id);
        tracing::debug!(%id, "kill：Agent 已送出結束訊號");
        if cleanup {
            self.discard_worktree(&info)?;
        }
        // 註記留下的副本：之後 `agt gc` 與開啟 TUI 時會提示清理（closed_worktree）
        let left = (!cleanup && !info.shared)
            .then_some(info.worktree.as_deref())
            .flatten();
        self.record(
            &id,
            "status",
            json!({"to": "closed", "cleanup": cleanup, "worktree_left": left}),
        );
        self.remove_entry(&id, if cleanup { Some("discard") } else { None })
    }

    /// 收尾動作（規格 7.3）。
    pub fn finish(&self, id: &str, action: FinishAction, force: bool) -> OpResult<String> {
        let id = self.resolve(id)?;
        let info = self.get(&id)?;
        match action {
            FinishAction::Keep => {
                self.stop_agent(&id);
                self.record(&id, "git", json!({"action": "keep"}));
                self.remove_entry(&id, Some("keep"))?;
                Ok(match &info.worktree {
                    Some(wt) => format!(
                        "已結束 Agent，保留 worktree {wt} 與 branch {}",
                        info.branch.unwrap_or_default()
                    ),
                    None => "已結束 Agent".into(),
                })
            }
            FinishAction::Discard => {
                self.check_discard(&info, force)?;
                self.stop_agent(&id);
                self.discard_worktree(&info)?;
                self.record(&id, "git", json!({"action": "discard"}));
                self.remove_entry(&id, Some("discard"))?;
                Ok(if info.shared {
                    "已關閉分頁；worktree 屬於其他分頁，已保留".into()
                } else {
                    "已移除 worktree 並刪除 branch".into()
                })
            }
            FinishAction::Merge { strategy } => {
                let (wt, branch, base, repo) = need_worktree(&info)?;
                self.auto_commit(&id, &info, &repo, &wt)?;
                let cfg_strategy = self.cfg().general.merge_strategy.clone();
                let strategy = match strategy {
                    Some(s) => s,
                    None if cfg_strategy != "ask" => cfg_strategy
                        .parse()
                        .map_err(|e: String| OpError::new(codes::BAD_REQUEST, e))?,
                    None => {
                        return Err(OpError::new(
                            codes::NEED_STRATEGY,
                            "請選擇 merge 策略：merge、squash 或 rebase",
                        ));
                    }
                };
                let extra = self.cfg().general.sensitive_paths.clone();
                confirm_sensitive(&repo, &wt, &base, &extra, "merge", force)?;
                let s = match strategy {
                    MergeStrategy::Merge => agt_git::Strategy::Merge,
                    MergeStrategy::Squash => agt_git::Strategy::Squash,
                    MergeStrategy::Rebase => agt_git::Strategy::Rebase,
                };
                let msg = agt_git::merge_into_base(
                    Path::new(&repo),
                    Path::new(&wt),
                    &branch,
                    &base,
                    s,
                    &info.task,
                );
                self.record(&id, "git", json!({"action": "merge", "strategy": format!("{strategy:?}").to_lowercase(), "ok": msg.is_ok()}));
                let msg = msg?;
                self.store().set_finish_action(&id, "merge")?;
                Ok(msg)
            }
            FinishAction::Pr => {
                let (wt, branch, base, repo) = need_worktree(&info)?;
                self.auto_commit(&id, &info, &repo, &wt)?;
                let extra = self.cfg().general.sensitive_paths.clone();
                confirm_sensitive(&repo, &wt, &base, &extra, "建立 PR", force)?;
                let out = agt_git::create_pr(
                    Path::new(&repo),
                    Path::new(&wt),
                    &branch,
                    &base,
                    &info.task,
                )?;
                self.record(&id, "git", json!({"action": "pr", "result": out}));
                self.store().set_finish_action(&id, "pr")?;
                Ok(out)
            }
        }
    }

    /// Agent 設定了 `auto_commit` 時，收尾前先 commit worktree 中的所有變更。
    fn auto_commit(&self, id: &str, info: &SessionInfo, repo: &str, wt: &str) -> OpResult<()> {
        let enabled = self
            .cfg()
            .agents
            .get(&info.agent)
            .is_some_and(|a| a.auto_commit);
        if enabled && agt_git::commit_all(Path::new(repo), Path::new(wt), &info.task)? {
            self.record(id, "git", json!({"action": "auto_commit"}));
        }
        Ok(())
    }

    pub fn diff(&self, id: &str) -> OpResult<String> {
        let info = self.get(id)?;
        let (wt, _, base, repo) = need_worktree(&info)?;
        Ok(agt_git::diff(Path::new(&repo), Path::new(&wt), &base)?)
    }

    /// 孤兒偵測（規格 7.5）：比對 `git worktree list` 與資料庫。
    pub fn gc(&self, prune: bool) -> OpResult<Vec<GcItem>> {
        let rows = self.store().sessions_with_worktree()?;
        let mut repos: Vec<String> = rows.iter().filter_map(|r| r.repo_path.clone()).collect();
        repos.sort();
        repos.dedup();
        let known: Vec<&SessionRow> = rows.iter().collect();
        let mut items = Vec::new();
        for repo in &repos {
            let repo_p = Path::new(repo);
            if !repo_p.is_dir() {
                continue;
            }
            let list = match agt_git::list_worktrees(repo_p) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("無法列出 {repo} 的 worktree：{e}");
                    continue;
                }
            };
            for wt in &list {
                let Some(branch) = &wt.branch else { continue };
                if !branch.starts_with(agt_git::BRANCH_PREFIX) {
                    continue;
                }
                let tracked = known.iter().any(|r| {
                    r.worktree_path
                        .as_deref()
                        .is_some_and(|p| agt_git::same_path(Path::new(p), &wt.path))
                });
                if !tracked {
                    items.push(GcItem {
                        kind: "orphan_worktree".into(),
                        repo: repo.clone(),
                        path: wt.path.to_string_lossy().into_owned(),
                        branch: Some(branch.clone()),
                        session: None,
                    });
                }
            }
            for r in known
                .iter()
                .filter(|r| r.repo_path.as_deref() == Some(repo.as_str()))
            {
                let Some(p) = &r.worktree_path else { continue };
                let in_git = list
                    .iter()
                    .any(|w| agt_git::same_path(&w.path, Path::new(p)));
                if !in_git || !Path::new(p).exists() {
                    items.push(GcItem {
                        kind: "missing_worktree".into(),
                        repo: repo.clone(),
                        path: p.clone(),
                        branch: r.branch.clone(),
                        session: Some(r.short_id.clone()),
                    });
                } else if r.ended_at.is_some() && !r.shared && !in_use(&known, r, p) {
                    // 分頁已關閉，但 worktree 副本還留在磁碟上（關閉時選了保留）
                    items.push(GcItem {
                        kind: "closed_worktree".into(),
                        repo: repo.clone(),
                        path: p.clone(),
                        branch: r.branch.clone(),
                        session: Some(r.short_id.clone()),
                    });
                }
            }
        }
        if prune {
            for it in &items {
                let repo = Path::new(&it.repo);
                match it.kind.as_str() {
                    // 只移除乾淨的 worktree；有變更時 git 會拒絕，保留給使用者決定。branch 保留。
                    "orphan_worktree" => {
                        if let Err(e) = agt_git::remove_worktree(repo, Path::new(&it.path), false) {
                            tracing::warn!("無法移除 {}：{e}", it.path);
                        }
                    }
                    "closed_worktree" => self.prune_closed_worktree(it)?,
                    _ => {
                        let _ = agt_git::prune(repo);
                        if let Some(sid) = &it.session {
                            let row = self.store().get_session(sid)?;
                            if let Some(row) = row {
                                self.store().clear_worktree(&row.id)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(items)
    }

    /// 移除已關閉分頁留下的 worktree 副本。只移除乾淨的副本（有變更時 git 會拒絕，保留給使用者決定）；
    /// branch 只在沒有 base 以外、也沒有 push 的 commit 時刪除，否則保留。
    fn prune_closed_worktree(&self, it: &GcItem) -> OpResult<()> {
        let (repo, wt) = (Path::new(&it.repo), Path::new(&it.path));
        let Some(row) = it
            .session
            .as_deref()
            .map(|sid| self.store().get_session(sid))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        // .git 被改動過就不在裡面執行 git，留給使用者處理
        if let Err(e) = agt_git::trusted_gitdir(repo, wt) {
            tracing::warn!("不清理 {}：{e}", it.path);
            return Ok(());
        }
        let keep_branch = match &row.base_branch {
            Some(base) => agt_git::has_unpushed(repo, wt, base).unwrap_or(true),
            None => true,
        };
        if let Err(e) = agt_git::remove_worktree(repo, wt, false) {
            tracing::warn!("無法移除 {}：{e}", it.path);
            return Ok(());
        }
        if !keep_branch
            && let Some(b) = &row.branch
            && let Err(e) = agt_git::delete_branch(repo, b)
        {
            tracing::warn!("無法刪除 branch {b}：{e}");
        }
        self.store().clear_worktree(&row.id)?;
        Ok(())
    }

    /// 定期更新 git 資訊，有變化時推播 `WorktreeChanged`。
    pub fn poll_git(&self) {
        let targets: Vec<(String, String, String, String)> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter_map(|e| {
                Some((
                    e.info.id.clone(),
                    e.info.repo_path.clone()?,
                    e.info.worktree.clone()?,
                    e.info.base_branch.clone()?,
                ))
            })
            .collect();
        for (id, repo, wt, base) in targets {
            if !Path::new(&wt).is_dir() {
                continue;
            }
            let st = match agt_git::status(Path::new(&repo), Path::new(&wt), &base) {
                Ok(st) => st,
                Err(e) => {
                    // 沙箱裡的 Agent 改了 .git：不執行 git，並提醒使用者一次
                    if agt_git::trusted_gitdir(Path::new(&repo), Path::new(&wt)).is_err()
                        && self.tamper_warned.lock().unwrap().insert(id.clone())
                    {
                        tracing::warn!("{id}：{e}");
                        let _ = self.events.send(ServerMsg::Error {
                            code: codes::GIT.into(),
                            message: e.to_string(),
                        });
                    }
                    continue;
                }
            };
            let changed = {
                let mut entries = self.entries.lock().unwrap();
                match entries.get_mut(&id) {
                    Some(e)
                        if (e.info.ahead, e.info.behind, e.info.dirty)
                            != (st.ahead, st.behind, st.dirty) =>
                    {
                        e.info.ahead = st.ahead;
                        e.info.behind = st.behind;
                        e.info.dirty = st.dirty;
                        true
                    }
                    _ => false,
                }
            };
            if changed {
                let _ = self.events.send(ServerMsg::WorktreeChanged {
                    id,
                    ahead: st.ahead,
                    behind: st.behind,
                    dirty: st.dirty,
                });
            }
        }
    }

    /// 所有工作階段結束且沒有 client 連線，經過 `idle_shutdown_secs` 後回傳 true。
    pub fn should_idle_shutdown(&self) -> bool {
        let secs = self.cfg().general.idle_shutdown_secs;
        secs > 0
            && self.ui_clients.load(Ordering::Relaxed) == 0
            && self.running_count() == 0
            && self.last_activity.lock().unwrap().elapsed() >= Duration::from_secs(secs)
    }

    pub fn kill_all(&self) {
        let sessions: Vec<Arc<Session>> = self
            .entries
            .lock()
            .unwrap()
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
        let sessions = self.entries.lock().unwrap().len();
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
        d.entries.lock().unwrap().insert(
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
}
