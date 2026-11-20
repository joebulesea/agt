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
