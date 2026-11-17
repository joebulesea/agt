//! Session：一個 PTY、一個 VT 模擬器、一個 AgentAdapter 與狀態機；產生畫面快照與差異。

use crate::adapter::{AgentAdapter, CommandSpec, OscEvent, ScreenView};
use crate::osc::OscScanner;
use crate::platform;
use crate::term::{TermEvent, Terminal};
use crate::{Error, Result};
use agt_proto::{AgentStatus, Screen, ScreenDiff, ScreenUpdate};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, oneshot};

/// 收到 hooks 狀態後，規則比對結果被忽略的時間（規格 6.5）。
pub const HOOK_PRIORITY_WINDOW: Duration = Duration::from_secs(30);
/// 畫面差異最多每 16 ms 送一次（60 fps）。
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// 規則比對最多每 250 ms 一次（規格 6.1）。
const CHECK_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSource {
    Daemon,
    Hook,
    Osc,
    Rule,
    Output,
}

impl StatusSource {
    fn is_heuristic(self) -> bool {
        matches!(
            self,
            StatusSource::Osc | StatusSource::Rule | StatusSource::Output
        )
    }
}

/// 狀態相關事件（給 daemon）。
#[derive(Debug, Clone)]
pub enum SessionEvent {
    Status {
        from: AgentStatus,
        to: AgentStatus,
        source: StatusSource,
        message: Option<String>,
    },
    Notify {
        title: Option<String>,
        body: String,
    },
    Title(String),
    /// 要複製到使用者剪貼簿的文字（OSC 52），由 UI 轉給外層終端機。
    Clipboard(String),
    Exited {
        code: Option<i32>,
    },
}

pub struct SessionOptions {
    pub id: String,
    pub cols: u16,
    pub rows: u16,
    pub scrollback: usize,
    pub spec: CommandSpec,
    pub adapter: Box<dyn AgentAdapter>,
    /// 終端機紀錄檔（由 `agt-store` 提供，可輪替）。
    pub log: Option<Box<dyn Write + Send>>,
}

struct Inner {
    term: Terminal,
    osc: OscScanner,
    adapter: Box<dyn AgentAdapter>,
    status: AgentStatus,
    last_frame: Option<Screen>,
    dirty: bool,
    last_output: Instant,
    hook_at: Option<Instant>,
    log: Option<Box<dyn Write + Send>>,
    exit_code: Option<i32>,
}

pub struct Session {
    pub id: String,
    inner: Mutex<Inner>,
    input: Mutex<Option<std_mpsc::Sender<Vec<u8>>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>,
    pid: Option<u32>,
    /// 收尾指令與「已執行過」旗標：Agent 結束與被關閉都會觸發，只執行一次。
    cleanup: Vec<String>,
    cleaned: AtomicBool,
    events: broadcast::Sender<SessionEvent>,
    screens: broadcast::Sender<Arc<ScreenUpdate>>,
}

fn pty_err(e: impl std::fmt::Display) -> Error {
    Error::msg(e.to_string())
}

/// 呼叫端 Claude Code 工作階段的標記，不應傳給分頁中的 Agent。
/// 使用者自己的設定（例如 `CLAUDE_CODE_USE_BEDROCK`）不在此列，會照常傳遞。
const INHERITED_SESSION_MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
];

impl Session {
    /// 建立 PTY 並啟動指令。必須在 tokio runtime 中呼叫。
    /// 同時回傳在啟動前就訂閱好的事件接收端，不會漏掉最早的狀態變化。
    pub fn spawn(
        opts: SessionOptions,
    ) -> Result<(Arc<Session>, broadcast::Receiver<SessionEvent>)> {
        let size = PtySize {
            rows: opts.rows.clamp(1, agt_proto::MAX_ROWS),
            cols: opts.cols.clamp(2, agt_proto::MAX_COLS),
            pixel_width: 0,
            pixel_height: 0,
        };
        let spec = &opts.spec;
        // 先清掉上次可能殘留的外部資源（例如 daemon 異常結束後留下的同名容器）
        platform::run_cleanup(&spec.cleanup);
        let pair = native_pty_system().openpty(size).map_err(pty_err)?;
        let (program, pre_args) = crate::platform::resolve_program(&spec.program);
        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&pre_args);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        // daemon 可能是從某個 Claude Code 工作階段內被自動啟動的，
        // 不清掉這些標記的話，分頁裡的 claude 會把自己當成子工作階段（不儲存 transcript）。
        for k in INHERITED_SESSION_MARKERS {
            cmd.env_remove(k);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::msg(format!("無法啟動 `{}`：{e}", spec.program)))?;
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(pty_err)?;
        let mut writer = pair.master.take_writer().map_err(pty_err)?;

        let (events, first_rx) = broadcast::channel(256);
        let (screens, _) = broadcast::channel(512);
        let (in_tx, in_rx) = std_mpsc::channel::<Vec<u8>>();

        let session = Arc::new(Session {
            id: opts.id,
            inner: Mutex::new(Inner {
                term: Terminal::new(size.cols, size.rows, opts.scrollback),
                osc: OscScanner::default(),
                adapter: opts.adapter,
                status: AgentStatus::Starting,
                last_frame: None,
                dirty: true,
                last_output: Instant::now(),
                hook_at: None,
                log: opts.log,
                exit_code: None,
            }),
            input: Mutex::new(Some(in_tx)),
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(Some(killer)),
            pid,
            cleanup: spec.cleanup.clone(),
            cleaned: AtomicBool::new(false),
            events,
            screens,
        });

        // 寫入執行緒：避免 PTY 緩衝區滿時卡住 async runtime。
        std::thread::Builder::new()
            .name("agt-pty-writer".into())
            .spawn(move || {
                while let Ok(buf) = in_rx.recv() {
                    if writer.write_all(&buf).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
            })?;

        // 讀取執行緒
        let (out_tx, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("agt-pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if out_tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })?;

        // 等待子行程結束
        let (exit_tx, exit_rx) = oneshot::channel::<Option<i32>>();
        std::thread::Builder::new()
            .name("agt-pty-wait".into())
            .spawn(move || {
                let code = child.wait().ok().map(|s| s.exit_code() as i32);
                let _ = exit_tx.send(code);
            })?;

        let s2 = session.clone();
        tokio::spawn(async move { s2.pump(out_rx, exit_rx).await });
        Ok((session, first_rx))
    }

    async fn pump(
        self: Arc<Self>,
        mut out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        mut exit_rx: oneshot::Receiver<Option<i32>>,
    ) {
        let mut frame = tokio::time::interval(FRAME_INTERVAL);
        frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut check = tokio::time::interval(CHECK_INTERVAL);
        check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut exit_code: Option<Option<i32>> = None;
        loop {
            tokio::select! {
                chunk = out_rx.recv() => match chunk {
                    Some(c) => self.on_output(&c),
                    None => break,
                },
                code = &mut exit_rx, if exit_code.is_none() => {
                    exit_code = Some(code.unwrap_or(None));
                    // ConPTY 在子行程結束後不一定會關閉輸出管線：先把剩下的輸出讀完再收尾。
                    while let Ok(Some(c)) =
                        tokio::time::timeout(Duration::from_millis(300), out_rx.recv()).await
                    {
                        self.on_output(&c);
                    }
                    break;
                }
                _ = frame.tick() => self.flush_frame(),
                _ = check.tick() => self.periodic(),
            }
        }
        tracing::debug!(id = %self.id, "pump：輸出迴圈結束");
        let code = match exit_code {
            Some(c) => c,
            None => exit_rx.await.unwrap_or(None),
        };
        tracing::debug!(id = %self.id, ?code, "pump：子行程已結束");
        // 關閉 PTY。Windows 的 ClosePseudoConsole 會阻塞，不能在 async 執行緒上做。
        self.input.lock().unwrap().take();
        let master = self.master.lock().unwrap().take();
        if let Some(m) = master {
            let _ = std::thread::Builder::new()
                .name("agt-pty-close".into())
                .spawn(move || {
                    tracing::debug!("關閉 PTY 中");
                    drop(m);
                    tracing::debug!("PTY 已關閉");
                });
        }
        tracing::debug!(id = %self.id, "pump：PTY 已交給關閉執行緒");
        if !self.cleanup.is_empty() && !self.cleaned.swap(true, Ordering::SeqCst) {
            let cmd = self.cleanup.clone();
            let _ = std::thread::Builder::new()
                .name("agt-cleanup".into())
                .spawn(move || platform::run_cleanup(&cmd));
        }
        self.flush_frame();
        let mut inner = self.inner.lock().unwrap();
        inner.exit_code = code;
        if let Some(log) = inner.log.as_mut() {
            let _ = log.flush();
        }
        let to = if code == Some(platform::SETUP_FAILED_EXIT as i32) {
            AgentStatus::Error
        } else {
            AgentStatus::Exited
        };
        let from = inner.status;
        inner.status = to;
        drop(inner);
        if from != to {
            let _ = self.events.send(SessionEvent::Status {
                from,
                to,
                source: StatusSource::Daemon,
                message: None,
            });
        }
        let _ = self.events.send(SessionEvent::Exited { code });
        tracing::debug!(id = %self.id, "pump：結束");
    }

    fn on_output(&self, chunk: &[u8]) {
        let mut writes = Vec::new();
        let mut inner = self.inner.lock().unwrap();
        if let Some(log) = inner.log.as_mut()
            && log.write_all(chunk).is_err()
        {
            inner.log = None;
        }
        let osc_events = inner.osc.feed(chunk);
        for ev in inner.term.advance(chunk) {
            match ev {
                TermEvent::PtyWrite(s) => writes.push(s.into_bytes()),
                TermEvent::Title(t) => {
                    let _ = self.events.send(SessionEvent::Title(t));
                }
                TermEvent::Bell => {} // BEL 已由 OscScanner 處理
                TermEvent::Clipboard(text) => {
                    let _ = self.events.send(SessionEvent::Clipboard(text));
                }
            }
        }
        inner.dirty = true;
        inner.last_output = Instant::now();

        if inner.status == AgentStatus::Starting {
            self.set_status_locked(&mut inner, AgentStatus::Running, StatusSource::Output, None);
        } else {
            let cur = inner.status;
            if let Some(s) = inner.adapter.on_output(cur) {
                self.set_status_locked(&mut inner, s, StatusSource::Output, None);
            }
        }
        for ev in osc_events {
            if let OscEvent::Notify { title, body } = &ev {
                let _ = self.events.send(SessionEvent::Notify {
                    title: title.clone(),
                    body: body.clone(),
                });
            }
            if let Some(s) = inner.adapter.on_osc(&ev) {
                self.set_status_locked(&mut inner, s, StatusSource::Osc, None);
            }
        }
        drop(inner);
        for w in writes {
            self.send_input(w);
        }
    }

    fn periodic(&self) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.status.is_alive() || inner.exit_code.is_some() {
            return;
        }
        let quiet = inner.last_output.elapsed();
        if quiet < Duration::from_secs(30) {
            let view = ScreenView {
                lines: inner.term.last_lines(10),
                quiet_for: quiet,
            };
            if let Some(s) = inner.adapter.on_screen(&view) {
                self.set_status_locked(&mut inner, s, StatusSource::Rule, None);
            }
        }
        if let Some(s) = inner.adapter.on_idle(quiet) {
            self.set_status_locked(&mut inner, s, StatusSource::Rule, None);
        }
    }

    fn set_status_locked(
        &self,
        inner: &mut Inner,
        to: AgentStatus,
        source: StatusSource,
        message: Option<String>,
    ) -> bool {
        if inner.exit_code.is_some() || inner.status == AgentStatus::Exited {
            return false;
        }
        let recent_hook = inner
            .hook_at
            .is_some_and(|at| at.elapsed() < HOOK_PRIORITY_WINDOW);
        let first_output = inner.status == AgentStatus::Starting && to == AgentStatus::Running;
        if source.is_heuristic() && recent_hook && !first_output {
            return false;
        }
        if source == StatusSource::Hook {
            inner.hook_at = Some(Instant::now());
        }
        let from = inner.status;
        if from == to {
            return false;
        }
        inner.status = to;
        let _ = self.events.send(SessionEvent::Status {
            from,
            to,
            source,
            message,
        });
        true
    }

    /// 由 hooks（`agt notify`）或 daemon 更新狀態。
    pub fn set_status(
        &self,
        to: AgentStatus,
        source: StatusSource,
        message: Option<String>,
    ) -> bool {
        let mut inner = self.inner.lock().unwrap();
        self.set_status_locked(&mut inner, to, source, message)
    }

    fn flush_frame(&self) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.dirty {
            return;
        }
        inner.dirty = false;
        if self.screens.receiver_count() == 0 {
            // 沒有人在看：不產生畫面，下次 attach 會拿到完整快照。
            inner.last_frame = None;
            return;
        }
        let screen = inner.term.screen();
        let update = match inner.last_frame.as_ref().and_then(|old| {
            screen
                .diff_from(old)
                .map(|rows| (rows, old.cursor, old.modes, old.display_offset))
        }) {
            Some((rows, cursor, modes, offset)) => {
                if rows.is_empty()
                    && cursor == screen.cursor
                    && modes == screen.modes
                    && offset == screen.display_offset
                {
                    None
                } else {
                    Some(ScreenUpdate::Diff {
                        id: self.id.clone(),
                        diff: ScreenDiff {
                            rows,
                            cursor: screen.cursor,
                            modes: screen.modes,
                            display_offset: screen.display_offset,
                            history: screen.history,
                        },
                    })
                }
            }
            None => Some(ScreenUpdate::Snapshot {
                id: self.id.clone(),
                screen: screen.clone(),
            }),
        };
        inner.last_frame = Some(screen);
        drop(inner);
        if let Some(u) = update {
            let _ = self.screens.send(Arc::new(u));
        }
    }

    /// 訂閱畫面更新並取得目前的完整快照（attach 或切換分頁時）。
    /// 先訂閱再產生快照，之後收到的差異都可以安全地套用在快照上。
    pub fn attach(&self) -> (broadcast::Receiver<Arc<ScreenUpdate>>, Screen) {
        let rx = self.screens.subscribe();
        (rx, self.snapshot())
    }

    /// 完整快照；會先把待送的差異送給其他觀看者。
    pub fn snapshot(&self) -> Screen {
        self.flush_frame();
        let mut inner = self.inner.lock().unwrap();
        let screen = inner.term.screen();
        if self.screens.receiver_count() > 0 {
            inner.last_frame = Some(screen.clone());
        }
        screen
    }

    pub fn viewers(&self) -> usize {
        self.screens.receiver_count()
    }

    fn send_input(&self, bytes: Vec<u8>) {
        if let Some(tx) = self.input.lock().unwrap().as_ref() {
            let _ = tx.send(bytes);
        }
    }

    /// 使用者輸入：原樣送給 PTY。若正在捲動歷史，先回到底部。
    pub fn write(&self, bytes: &[u8]) {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.term.display_offset() > 0 {
                inner.term.scroll(i32::MIN);
                inner.dirty = true;
            }
        }
        self.send_input(bytes.to_vec());
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let cols = cols.clamp(2, agt_proto::MAX_COLS);
        let rows = rows.clamp(1, agt_proto::MAX_ROWS);
        let mut inner = self.inner.lock().unwrap();
        if inner.term.cols() == cols && inner.term.rows() == rows {
            return;
        }
        inner.term.resize(cols, rows);
        inner.dirty = true;
        drop(inner);
        if let Some(m) = self.master.lock().unwrap().as_ref() {
            let _ = m.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    pub fn size(&self) -> (u16, u16) {
        let inner = self.inner.lock().unwrap();
        (inner.term.cols(), inner.term.rows())
    }

    pub fn scroll(&self, delta: i32) {
        let mut inner = self.inner.lock().unwrap();
        inner.term.scroll(delta);
        inner.dirty = true;
    }

    pub fn search(&self, query: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let found = inner.term.search_up(query);
        inner.dirty = true;
        found
    }

    pub fn status(&self) -> AgentStatus {
        self.inner.lock().unwrap().status
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.inner.lock().unwrap().exit_code
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn is_running(&self) -> bool {
        self.inner.lock().unwrap().exit_code.is_none() && self.input.lock().unwrap().is_some()
    }

    /// 結束整個行程樹。
    pub fn kill(&self) {
        if let Some(pid) = self.pid {
            platform::kill_tree(pid);
        }
        if let Some(k) = self.killer.lock().unwrap().as_mut() {
            let _ = k.kill();
        }
        // 同步執行：daemon 關閉時也要確定外部資源（例如容器）已清掉
        if !self.cleanup.is_empty() && !self.cleaned.swap(true, Ordering::SeqCst) {
            platform::run_cleanup(&self.cleanup);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::LaunchCtx;

    struct Plain;
    impl AgentAdapter for Plain {
        fn id(&self) -> &'static str {
            "plain"
        }
        fn launch(&self, _: &LaunchCtx) -> CommandSpec {
            unreachable!()
        }
        fn install_hooks(&self, _: &LaunchCtx) -> Result<()> {
            Ok(())
        }
        fn on_osc(&mut self, _: &OscEvent) -> Option<AgentStatus> {
            None
        }
        fn on_screen(&mut self, s: &ScreenView) -> Option<AgentStatus> {
            (s.text().contains("(y/n)") && s.quiet_for > Duration::from_millis(500))
                .then_some(AgentStatus::WaitingInput)
        }
        fn on_idle(&mut self, _: Duration) -> Option<AgentStatus> {
            None
        }
    }

    fn shell_spec(script: &str) -> CommandSpec {
        if cfg!(windows) {
            CommandSpec {
                program: "cmd".into(),
                args: vec!["/C".into(), script.into()],
                cwd: std::env::temp_dir(),
                ..Default::default()
            }
        } else {
            CommandSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                cwd: std::env::temp_dir(),
                ..Default::default()
            }
        }
    }

    async fn wait_for<F: Fn(&Session) -> bool>(s: &Session, f: F) -> bool {
        for _ in 0..100 {
            if f(s) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    #[tokio::test]
    async fn runs_detects_waiting_and_exits() {
        let (s, mut ev) = Session::spawn(SessionOptions {
            id: "t1".into(),
            cols: 40,
            rows: 10,
            scrollback: 100,
            spec: shell_spec(if cfg!(windows) {
                "echo 中文 hello& echo Proceed? (y/n)& set /p x=& echo got"
            } else {
                "echo 中文 hello; echo 'Proceed? (y/n)'; read x; echo got $x"
            }),
            adapter: Box::new(Plain),
            log: None,
        })
        .unwrap();
        assert!(wait_for(&s, |s| s.status() == AgentStatus::WaitingInput).await);
        let snap = s.snapshot();
        assert!((0..10).any(|y| snap.row_text(y).contains("中文 hello")));
        s.write(b"y\r");
        assert!(wait_for(&s, |s| s.exit_code().is_some()).await);
        assert_eq!(s.status(), AgentStatus::Exited);
        let mut saw_exit = false;
        while let Ok(e) = ev.try_recv() {
            if matches!(e, SessionEvent::Exited { code: Some(0) }) {
                saw_exit = true;
            }
        }
        assert!(saw_exit);
    }

    #[tokio::test]
    async fn hooks_override_rules() {
        let (s, _ev) = Session::spawn(SessionOptions {
            id: "t2".into(),
            cols: 40,
            rows: 10,
            scrollback: 100,
            // cmd 沒有 sleep：ping 4 次約 3 秒
            spec: shell_spec(if cfg!(windows) {
                "echo x (y/n)& ping -n 4 127.0.0.1 >nul"
            } else {
                "echo 'x (y/n)'; sleep 3"
            }),
            adapter: Box::new(Plain),
            log: None,
        })
        .unwrap();
        assert!(wait_for(&s, |s| s.status() == AgentStatus::Running).await);
        s.set_status(AgentStatus::Done, StatusSource::Hook, None);
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(s.status(), AgentStatus::Done, "規則比對不應覆蓋 hooks 狀態");
        s.kill();
    }

    /// Windows 的 setup 包裝走 PowerShell（與 Unix 的 /bin/sh 不同），要實際跑過一次。
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_setup_wrapper() {
        let spawn = |setup: &str, id: &str| {
            Session::spawn(SessionOptions {
                id: id.into(),
                cols: 60,
                rows: 10,
                scrollback: 100,
                spec: platform::wrap_with_setup(setup, shell_spec("echo agent-ok")),
                adapter: Box::new(Plain),
                log: None,
            })
            .unwrap()
            .0
        };
        // setup 失敗：不啟動 Agent，以 SETUP_FAILED_EXIT 結束並進入 Error
        let s = spawn("cmd /c exit 1", "w1");
        assert!(wait_for(&s, |s| s.exit_code().is_some()).await);
        assert_eq!(s.exit_code(), Some(platform::SETUP_FAILED_EXIT as i32));
        assert_eq!(s.status(), AgentStatus::Error);
        // setup 成功：接著啟動 Agent
        let s = spawn("Write-Host setup-ok", "w2");
        assert!(wait_for(&s, |s| s.exit_code().is_some()).await);
        assert_eq!(s.exit_code(), Some(0));
        let snap = s.snapshot();
        assert!((0..10).any(|y| snap.row_text(y).contains("agent-ok")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_runs_before_start_and_on_kill() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("cleanup.log");
        let mut spec = shell_spec("sleep 30");
        spec.cleanup = vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("echo x >> '{}'", log.display()),
        ];
        let (s, _ev) = Session::spawn(SessionOptions {
            id: "t3".into(),
            cols: 40,
            rows: 10,
            scrollback: 100,
            spec,
            adapter: Box::new(Plain),
            log: None,
        })
        .unwrap();
        let count = || {
            std::fs::read_to_string(&log)
                .map(|t| t.lines().count())
                .unwrap_or(0)
        };
        assert_eq!(count(), 1, "啟動前應先執行一次收尾指令");
        s.kill();
        assert_eq!(count(), 2, "關閉時應同步執行收尾指令");
        assert!(wait_for(&s, |s| s.exit_code().is_some()).await);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(count(), 2, "結束後不應重複執行");
    }
}
