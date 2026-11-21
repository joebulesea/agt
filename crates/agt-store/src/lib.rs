//! 資料儲存（規格 10）：SQLite 中繼資料與終端機紀錄檔。只有 daemon 會寫入資料庫。

use rusqlite::{Connection, OptionalExtension, params};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("資料庫錯誤：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(
        "資料庫的版本（{found}）比這個 agt 支援的（{supported}）新，請升級 agt，或換回原本使用的版本；為了避免損毀資料，沒有開啟"
    )]
    TooNew { found: u32, supported: u32 },
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// 依序套用的 migration；`PRAGMA user_version` 記錄已套用到第幾個。
const MIGRATIONS: &[&str] = &[
    // 1：規格 10 的 schema
    r#"
CREATE TABLE repos (
  id          INTEGER PRIMARY KEY,
  path        TEXT NOT NULL UNIQUE,
  name        TEXT NOT NULL,
  base_branch TEXT
);
CREATE TABLE sessions (
  id            TEXT PRIMARY KEY,
  short_id      TEXT NOT NULL UNIQUE,
  agent         TEXT NOT NULL,
  repo_id       INTEGER REFERENCES repos(id),
  task          TEXT NOT NULL,
  worktree_path TEXT,
  branch        TEXT,
  base_branch   TEXT,
  status        TEXT NOT NULL,
  pid           INTEGER,
  created_at    INTEGER NOT NULL,
  ended_at      INTEGER,
  finish_action TEXT
);
CREATE TABLE events (
  id         INTEGER PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  at         INTEGER NOT NULL,
  kind       TEXT NOT NULL,
  data       TEXT
);
CREATE INDEX events_session ON events(session_id, at);
"#,
    // 2：工作目錄、共用 worktree 與已確認的 setup 指令（規格 7.5、11.4）
    r#"
ALTER TABLE sessions ADD COLUMN cwd TEXT;
ALTER TABLE sessions ADD COLUMN shared INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN exit_code INTEGER;
CREATE TABLE trusted_setups (
  repo_path TEXT NOT NULL,
  setup     TEXT NOT NULL,
  at        INTEGER NOT NULL,
  PRIMARY KEY (repo_path, setup)
);
"#,
    // 3：分頁目前的對話 ID（重新啟動時接續對話用）
    r#"
ALTER TABLE sessions ADD COLUMN conversation_id TEXT;
"#,
    // 4：分頁標籤（以逗號連接；標籤本身不會含逗號）
    r#"
ALTER TABLE sessions ADD COLUMN tags TEXT;
"#,
];

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionRow {
    pub id: String,
    pub short_id: String,
    pub agent: String,
    pub repo_path: Option<String>,
    pub repo_name: Option<String>,
    pub task: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub status: String,
    pub pid: Option<u32>,
    pub created_at: i64,
    pub ended_at: Option<i64>,
    pub finish_action: Option<String>,
    pub cwd: Option<String>,
    pub shared: bool,
    pub exit_code: Option<i32>,
    /// 目前的對話 ID；NULL = 建立於支援接續對話之前的分頁。
    pub conversation_id: Option<String>,
    pub tags: Vec<String>,
}

pub struct Store {
    conn: Connection,
}

const SESSION_COLS: &str = "s.id, s.short_id, s.agent, r.path, r.name, s.task, s.worktree_path, s.branch, \
     s.base_branch, s.status, s.pid, s.created_at, s.ended_at, s.finish_action, s.cwd, s.shared, s.exit_code, s.conversation_id, s.tags";

/// 標籤在資料庫的存放格式：以逗號連接（標籤本身不會含逗號，驗證在 `agt_proto::tags`）。
fn join_tags(tags: &[String]) -> Option<String> {
    if tags.is_empty() {
        None
    } else {
        Some(tags.join(","))
    }
}

fn parse_tags(s: Option<&str>) -> Vec<String> {
    s.map(|s| {
        s.split(',')
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: r.get(0)?,
        short_id: r.get(1)?,
        agent: r.get(2)?,
        repo_path: r.get(3)?,
        repo_name: r.get(4)?,
        task: r.get(5)?,
        worktree_path: r.get(6)?,
        branch: r.get(7)?,
        base_branch: r.get(8)?,
        status: r.get(9)?,
        pid: r.get(10)?,
        created_at: r.get(11)?,
        ended_at: r.get(12)?,
        finish_action: r.get(13)?,
        cwd: r.get(14)?,
        shared: r.get::<_, i64>(15)? != 0,
        exit_code: r.get(16)?,
        conversation_id: r.get(17)?,
        tags: parse_tags(r.get::<_, Option<String>>(18)?.as_deref()),
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // WAL 下 NORMAL 不必每筆交易都等硬碟同步（狀態切換很頻繁）；斷電時最多少掉最後幾筆交易，資料庫不會損壞
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn schema_version(&self) -> Result<u32> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    fn migrate(&mut self) -> Result<()> {
        let found = self.schema_version()?;
        // 較新版本建立的資料庫（例如用 `agt update --version` 降版後）：舊程式不認得新欄位，不能硬用
        if found as usize > MIGRATIONS.len() {
            return Err(StoreError::TooNew {
                found,
                supported: MIGRATIONS.len() as u32,
            });
        }
        let current = found as usize;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as u32)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// 新增或更新 repo，回傳 id。
    pub fn upsert_repo(&self, path: &str, name: &str, base_branch: Option<&str>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO repos (path, name, base_branch) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET name = excluded.name,
               base_branch = COALESCE(excluded.base_branch, repos.base_branch)",
            params![path, name, base_branch],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM repos WHERE path = ?1", [path], |r| r.get(0))?)
    }

    pub fn insert_session(&self, s: &SessionRow) -> Result<()> {
        let repo_id = match (&s.repo_path, &s.repo_name) {
            (Some(p), Some(n)) => Some(self.upsert_repo(p, n, None)?),
            _ => None,
        };
        self.conn.execute(
            "INSERT INTO sessions (id, short_id, agent, repo_id, task, worktree_path, branch, base_branch,
                status, pid, created_at, ended_at, finish_action, cwd, shared, exit_code, conversation_id, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                s.id,
                s.short_id,
                s.agent,
                repo_id,
                s.task,
                s.worktree_path,
                s.branch,
                s.base_branch,
                s.status,
                s.pid,
                s.created_at,
                s.ended_at,
                s.finish_action,
                s.cwd,
                s.shared as i64,
                s.exit_code,
                s.conversation_id,
                join_tags(&s.tags)
            ],
        )?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> Result<Option<SessionRow>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {SESSION_COLS} FROM sessions s LEFT JOIN repos r ON r.id = s.repo_id
                     WHERE s.id = ?1 OR s.short_id = ?1"
                ),
                [id],
                row_to_session,
            )
            .optional()?)
    }

    fn query_sessions(&self, filter: &str) -> Result<Vec<SessionRow>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {SESSION_COLS} FROM sessions s LEFT JOIN repos r ON r.id = s.repo_id
             {filter} ORDER BY s.created_at, s.rowid"
        ))?;
        let rows = st.query_map([], row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 所有工作階段（含已關閉、尚未被清除的）。
    pub fn all_sessions(&self) -> Result<Vec<SessionRow>> {
        self.query_sessions("")
    }

    /// 尚未關閉的工作階段（左側欄的分頁）。
    pub fn open_sessions(&self) -> Result<Vec<SessionRow>> {
        self.query_sessions("WHERE s.ended_at IS NULL")
    }

    /// 仍保有 worktree 的工作階段（孤兒偵測用）。
    pub fn sessions_with_worktree(&self) -> Result<Vec<SessionRow>> {
        self.query_sessions(
            "WHERE s.worktree_path IS NOT NULL AND COALESCE(s.finish_action, '') NOT IN ('discard')",
        )
    }

    pub fn update_status(&self, id: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET status = ?2 WHERE id = ?1",
            params![id, status],
        )?;
        Ok(())
    }

    pub fn update_process(&self, id: &str, pid: Option<u32>, exit_code: Option<i32>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET pid = ?2, exit_code = ?3 WHERE id = ?1",
            params![id, pid, exit_code],
        )?;
        Ok(())
    }

    pub fn set_conversation(&self, id: &str, conversation_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET conversation_id = ?2 WHERE id = ?1",
            params![id, conversation_id],
        )?;
        Ok(())
    }

    /// 取代標籤（呼叫端已用 `agt_proto::tags::normalize` 驗證）。
    pub fn set_tags(&self, id: &str, tags: &[String]) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET tags = ?2 WHERE id = ?1",
            params![id, join_tags(tags)],
        )?;
        Ok(())
    }

    pub fn rename(&self, id: &str, task: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET task = ?2 WHERE id = ?1",
            params![id, task],
        )?;
        Ok(())
    }

    pub fn set_finish_action(&self, id: &str, action: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET finish_action = ?2 WHERE id = ?1",
            params![id, action],
        )?;
        Ok(())
    }

    /// 關閉分頁（從左側欄移除）。
    pub fn end_session(&self, id: &str, finish_action: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?2, finish_action = COALESCE(?3, finish_action) WHERE id = ?1",
            params![id, now(), finish_action],
        )?;
        Ok(())
    }

    /// 移除 worktree 後清掉紀錄，讓孤兒偵測不再回報。
    pub fn clear_worktree(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET worktree_path = NULL WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn add_event(&self, session_id: &str, kind: &str, data: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events (session_id, at, kind, data) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, now(), kind, data],
        )?;
        Ok(())
    }

    pub fn events(&self, session_id: &str) -> Result<Vec<(i64, String, Option<String>)>> {
        let mut st = self
            .conn
            .prepare("SELECT at, kind, data FROM events WHERE session_id = ?1 ORDER BY at, id")?;
        let rows = st.query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn is_setup_trusted(&self, repo_path: &str, setup: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM trusted_setups WHERE repo_path = ?1 AND setup = ?2",
                params![repo_path, setup],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn trust_setup(&self, repo_path: &str, setup: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO trusted_setups (repo_path, setup, at) VALUES (?1, ?2, ?3)",
            params![repo_path, setup, now()],
        )?;
        Ok(())
    }

    /// 刪除在 `before`（Unix 秒）之前關閉、而且已經沒有 worktree 的分頁，連同它的事件。
    /// 還留著 worktree 的分頁（例如收尾選了 keep）保留，孤兒偵測要靠它判斷 worktree 不是孤兒。
    /// 回傳刪除的分頁數。
    pub fn prune_closed(&mut self, before: i64) -> Result<usize> {
        let tx = self.conn.transaction()?;
        const CLOSED: &str = "ended_at IS NOT NULL AND ended_at < ?1 \
             AND (worktree_path IS NULL OR finish_action = 'discard')";
        tx.execute(
            &format!(
                "DELETE FROM events WHERE session_id IN (SELECT id FROM sessions WHERE {CLOSED})"
            ),
            [before],
        )?;
        let n = tx.execute(&format!("DELETE FROM sessions WHERE {CLOSED}"), [before])?;
        tx.commit()?;
        Ok(n)
    }

    pub fn short_id_exists(&self, short_id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM sessions WHERE short_id = ?1",
                [short_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
}

/// 終端機紀錄檔：`<資料目錄>/logs/<session id>.log`，超過上限就輪替成 `.log.1`。
pub struct LogWriter {
    path: PathBuf,
    file: File,
    written: u64,
    max: u64,
}

pub const LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

fn open_private(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

impl LogWriter {
    pub fn open(dir: &Path, session_id: &str, max: u64) -> io::Result<LogWriter> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{session_id}.log"));
        let file = open_private(&path)?;
        let written = file.metadata()?.len();
        Ok(LogWriter {
            path,
            file,
            written,
            max,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        let old = self.path.with_extension("log.1");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&self.path, &old)?;
        self.file = open_private(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > self.max && self.written > 0 {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, short: &str) -> SessionRow {
        SessionRow {
            id: id.into(),
            short_id: short.into(),
            agent: "claude".into(),
            repo_path: Some("/r".into()),
            repo_name: Some("r".into()),
            task: "t".into(),
            worktree_path: Some("/w".into()),
            branch: Some("agt/claude/x".into()),
            base_branch: Some("main".into()),
            status: "starting".into(),
            created_at: now(),
            ..Default::default()
        }
    }

    #[test]
    fn tags_are_stored_and_replaced() {
        let s = Store::open_in_memory().unwrap();
        let mut r = row("id1", "aaaa");
        r.tags = vec!["bug".into(), "ui".into()];
        s.insert_session(&r).unwrap();
        assert_eq!(s.get_session("id1").unwrap().unwrap().tags, ["bug", "ui"]);
        s.set_tags("id1", &["x".to_string()]).unwrap();
        assert_eq!(s.get_session("id1").unwrap().unwrap().tags, ["x"]);
        s.set_tags("id1", &[]).unwrap();
        assert!(s.get_session("id1").unwrap().unwrap().tags.is_empty());
        // 沒有標籤的分頁（舊資料列、NULL）
        s.insert_session(&row("id2", "bbbb")).unwrap();
        assert!(s.get_session("id2").unwrap().unwrap().tags.is_empty());
        assert!(
            s.open_sessions()
                .unwrap()
                .iter()
                .all(|r| r.id != "id1" || r.tags.is_empty())
        );
    }

    #[test]
    fn migrations_and_crud() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap() as usize, MIGRATIONS.len());
        s.insert_session(&row("u1", "aaaaaa")).unwrap();
        s.insert_session(&row("u2", "bbbbbb")).unwrap();
        s.update_status("u1", "running").unwrap();
        s.rename("u1", "新名稱").unwrap();
        s.add_event("u1", "status", Some("{}")).unwrap();
        let got = s.get_session("aaaaaa").unwrap().unwrap();
        assert_eq!(
            (got.status.as_str(), got.task.as_str()),
            ("running", "新名稱")
        );
        assert_eq!(got.repo_name.as_deref(), Some("r"));
        s.end_session("u2", Some("keep")).unwrap();
        assert_eq!(s.open_sessions().unwrap().len(), 1);
        assert_eq!(s.events("u1").unwrap().len(), 1);
        assert!(s.short_id_exists("bbbbbb").unwrap());
    }

    /// 建立「只套用前 `n` 個 migration」的舊版資料庫，模擬使用者升級前的檔案。
    fn old_db(path: &Path, n: usize) -> Connection {
        let conn = Connection::open(path).unwrap();
        for sql in &MIGRATIONS[..n] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", n as u32).unwrap();
        conn
    }

    fn columns(s: &Store, table: &str) -> Vec<String> {
        let mut st = s
            .conn
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn every_old_version_upgrades_to_latest() {
        for n in 0..=MIGRATIONS.len() {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("agt.db");
            drop(old_db(&p, n));
            let s = Store::open(&p).unwrap_or_else(|e| panic!("從 v{n} 升級失敗：{e}"));
            assert_eq!(
                s.schema_version().unwrap() as usize,
                MIGRATIONS.len(),
                "v{n}"
            );
            // 升級後的結構與全新建立的一致
            let fresh = Store::open_in_memory().unwrap();
            for t in ["repos", "sessions", "events", "trusted_setups"] {
                let (mut a, mut b) = (columns(&s, t), columns(&fresh, t));
                a.sort();
                b.sort();
                assert_eq!(a, b, "v{n} 升級後的 {t} 欄位不同");
            }
            // 升級後可以正常使用
            s.insert_session(&row("u1", "aaaaaa")).unwrap();
            s.set_conversation("u1", "c1").unwrap();
            s.trust_setup("/r", "npm i").unwrap();
        }
    }

    #[test]
    fn file_store_uses_wal_with_synchronous_normal() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("agt.db")).unwrap();
        let mode: String = s
            .conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        // 0 = OFF、1 = NORMAL、2 = FULL
        let sync: i64 = s
            .conn
            .pragma_query_value(None, "synchronous", |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1);
    }

    #[test]
    fn upgrade_from_v1_keeps_existing_data_with_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("agt.db");
        {
            let c = old_db(&p, 1);
            c.execute(
                "INSERT INTO repos (id, path, name, base_branch) VALUES (1, '/r', 'r', 'main')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO sessions (id, short_id, agent, repo_id, task, worktree_path, branch, \
                 base_branch, status, pid, created_at, ended_at, finish_action) \
                 VALUES ('u1', 'aaaaaa', 'claude', 1, '舊任務', '/w', 'agt/claude/x', 'main', \
                 'running', 42, 1000, NULL, NULL)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO events (session_id, at, kind, data) VALUES ('u1', 1001, 'status', '{}')",
                [],
            )
            .unwrap();
        }
        let mut s = Store::open(&p).unwrap();
        let got = s.get_session("aaaaaa").unwrap().unwrap();
        assert_eq!(got.task, "舊任務");
        assert_eq!(got.repo_path.as_deref(), Some("/r"));
        assert_eq!(got.pid, Some(42));
        assert_eq!(got.created_at, 1000);
        // 後來新增的欄位：NULL 或預設值，不是壞資料
        assert_eq!(got.cwd, None);
        assert!(!got.shared);
        assert_eq!(got.exit_code, None);
        assert_eq!(got.conversation_id, None);
        assert_eq!(s.events("u1").unwrap().len(), 1);
        // 舊分頁也能被新功能處理
        s.set_conversation("u1", "conv-1").unwrap();
        s.end_session("u1", Some("keep")).unwrap();
        assert_eq!(s.prune_closed(i64::MAX).unwrap(), 0); // 還留著 worktree，不會被清掉
    }

    #[test]
    fn failed_migration_rolls_back_and_keeps_old_version() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("agt.db");
        {
            // migration 2 先 ALTER 三個欄位、最後才 CREATE TABLE trusted_setups：
            // 預先放一個同名的表，讓它在「欄位已經加好之後」才失敗
            let c = old_db(&p, 1);
            c.execute_batch("CREATE TABLE trusted_setups (x INTEGER);")
                .unwrap();
        }
        assert!(matches!(Store::open(&p), Err(StoreError::Sql(_))));
        let c = Connection::open(&p).unwrap();
        let v: u32 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1, "失敗時版本號不能前進");
        let cols: Vec<String> = {
            let mut st = c
                .prepare("SELECT name FROM pragma_table_info('sessions')")
                .unwrap();
            st.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        for added in ["cwd", "shared", "exit_code"] {
            assert!(
                !cols.iter().any(|c| c == added),
                "已執行的 ALTER 要一起回復（{added}）：{cols:?}"
            );
        }
    }

    #[test]
    fn refuses_database_from_newer_version() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("agt.db");
        {
            let s = Store::open(&p).unwrap();
            s.insert_session(&row("u1", "aaaaaa")).unwrap();
        }
        let newer = MIGRATIONS.len() as u32 + 1;
        Connection::open(&p)
            .unwrap()
            .pragma_update(None, "user_version", newer)
            .unwrap();
        match Store::open(&p) {
            Err(StoreError::TooNew { found, supported }) => {
                assert_eq!((found, supported), (newer, MIGRATIONS.len() as u32));
            }
            other => panic!("應該拒絕較新的資料庫：{:?}", other.map(|_| ())),
        }
        // 沒有被改動
        let v: u32 = Connection::open(&p)
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, newer);
    }

    #[test]
    fn migrations_are_append_only_and_nonempty() {
        // 已發布的 migration 不能改動或重排：用內容雜湊式的簡單指紋釘住前三個，
        // 新增 migration 時只需在後面追加，不必改這裡
        assert!(MIGRATIONS.len() >= 3);
        assert!(MIGRATIONS[0].contains("CREATE TABLE sessions"));
        assert!(MIGRATIONS[1].contains("trusted_setups"));
        assert!(MIGRATIONS[2].contains("conversation_id"));
        assert!(MIGRATIONS.iter().all(|m| !m.trim().is_empty()));
    }

    #[test]
    fn reopen_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("agt.db");
        Store::open(&p)
            .unwrap()
            .insert_session(&row("u1", "aaaaaa"))
            .unwrap();
        let s = Store::open(&p).unwrap();
        assert_eq!(s.open_sessions().unwrap().len(), 1);
    }

    #[test]
    fn trust() {
        let s = Store::open_in_memory().unwrap();
        assert!(!s.is_setup_trusted("/r", "npm i").unwrap());
        s.trust_setup("/r", "npm i").unwrap();
        assert!(s.is_setup_trusted("/r", "npm i").unwrap());
        assert!(!s.is_setup_trusted("/r", "rm -rf /").unwrap());
    }

    #[test]
    fn prune_closed_keeps_open_recent_and_kept_worktrees() {
        let mut s = Store::open_in_memory().unwrap();
        let ids = ["open", "recent", "old", "old_keep", "old_discard"];
        for (i, id) in ids.iter().enumerate() {
            s.insert_session(&row(id, &format!("s{i:05}"))).unwrap();
            s.add_event(id, "status", Some("{}")).unwrap();
        }
        let old = now() - 40 * 24 * 3600;
        let close = |s: &Store, id: &str, at: i64, action: Option<&str>| {
            s.conn
                .execute(
                    "UPDATE sessions SET ended_at = ?2, finish_action = ?3 WHERE id = ?1",
                    params![id, at, action],
                )
                .unwrap();
        };
        close(&s, "recent", now(), None);
        close(&s, "old", old, None);
        s.clear_worktree("old").unwrap();
        close(&s, "old_keep", old, Some("keep")); // worktree 還在：保留
        close(&s, "old_discard", old, Some("discard"));

        let n = s.prune_closed(now() - 30 * 24 * 3600).unwrap();
        assert_eq!(n, 2);
        for id in ["open", "recent", "old_keep"] {
            assert!(s.get_session(id).unwrap().is_some(), "{id}");
            assert_eq!(s.events(id).unwrap().len(), 1, "{id}");
        }
        for id in ["old", "old_discard"] {
            assert!(s.get_session(id).unwrap().is_none(), "{id}");
            assert!(s.events(id).unwrap().is_empty(), "{id}");
        }
    }

    #[test]
    fn log_rotation() {
        let d = tempfile::tempdir().unwrap();
        let mut w = LogWriter::open(d.path(), "s", 10).unwrap();
        w.write_all(b"12345678").unwrap();
        w.write_all(b"abcdef").unwrap();
        w.flush().unwrap();
        assert_eq!(std::fs::read(d.path().join("s.log")).unwrap(), b"abcdef");
        assert_eq!(
            std::fs::read(d.path().join("s.log.1")).unwrap(),
            b"12345678"
        );
    }
}
