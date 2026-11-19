//! 端對端測試：啟動真正的 agtd，以假 Agent 腳本 + 暫存 git repo 走完整流程。
//! （規格 12 的 tests/ 目錄；放在 agt-daemon 底下才能取得 agtd 執行檔路徑。）
//!
//! Windows 上假 Agent 由 Git for Windows 附的 `sh.exe` 執行（agt 本來就需要 git），
//! 同一份腳本在兩個平台上都能用。

use agt_proto::client::{Client, Connected, Incoming, expect_session, expect_sessions};
use agt_proto::*;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Env {
    _dir: tempfile::TempDir,
    home: PathBuf,
    sock: String,
    repo: PathBuf,
    daemon: Option<Child>,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// 執行假 Agent 腳本的 sh（設定檔中以正斜線表示）。
fn sh() -> String {
    if cfg!(windows) {
        // git --exec-path = <Git>/mingw64/libexec/git-core；<Git>/bin/sh.exe 會設好 PATH（git、sleep 等）
        let exec = git(Path::new("."), &["--exec-path"]);
        let root = Path::new(&exec).ancestors().nth(3).unwrap().to_path_buf();
        let sh = root.join("bin").join("sh.exe");
        assert!(
            sh.is_file(),
            "找不到 Git for Windows 的 sh.exe：{}",
            sh.display()
        );
        sh.to_string_lossy().replace('\\', "/")
    } else {
        "/bin/sh".into()
    }
}

/// 測試用的 agtd。交叉編譯後在別的機器上執行時，編譯時記下的路徑不存在，
/// 改找與測試執行檔相鄰的 agtd（和 agt 找 agtd 的方式相同）。
fn agtd_exe() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_agtd"));
    if built.is_file() {
        built
    } else {
        agt_proto::transport::sibling_exe("agtd")
    }
}

const FAKE_AGENT: &str = r#"
echo "agent in $(basename "$PWD") 中文輸出"
echo "work from $AGT_SESSION_ID" > "work-$(basename "$PWD").txt"
git add -A && git commit -qm "agent work"
printf 'Proceed? (y/n) '
read x
echo "answer=$x"
sleep 60
"#;

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agt-home");
        std::fs::create_dir_all(&home).unwrap();
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        std::fs::write(repo.join(".env"), "SECRET=1\n").unwrap();
        std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);

        // 路徑用 TOML 的 literal string（'...'），Windows 的反斜線才不會被當成跳脫字元
        let sh = sh();
        let config = format!(
            r#"
[general]
notify = false
idle_shutdown_secs = 0

[agents.fake]
command = '{sh}'
args = ["-c", '''{FAKE_AGENT}''']
status_source = "pattern"
waiting_patterns = ['\(y/n\)']
idle_timeout_secs = 30

[agents.conv]
command = '{sh}'
args = ["-c", '''{CONV_AGENT}''', "conv"]
status_source = "none"
new_args = ["new", "{{conversation}}"]
resume_args = ["resume", "{{conversation}}"]

[repos.'{}']
copy_files = [".env"]
"#,
            repo.display()
        );
        std::fs::write(home.join("config.toml"), config).unwrap();
        let sock = if cfg!(windows) {
            // 每個測試各用一個 named pipe，可以平行執行
            let name = dir.path().file_name().unwrap().to_string_lossy();
            format!(r"\\.\pipe\agt-e2e-{name}")
        } else {
            dir.path()
                .join("run")
                .join("agt.sock")
                .to_string_lossy()
                .into_owned()
        };
        let mut e = Env {
            _dir: dir,
            home,
            sock,
            repo,
            daemon: None,
        };
        e.start();
        e
    }

    fn start(&mut self) {
        let child = Command::new(agtd_exe())
            .env("AGT_HOME", &self.home)
            .env("AGT_SOCKET", &self.sock)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.daemon = Some(child);
    }

    async fn connect(&self, kind: &str) -> Connected {
        let start = Instant::now();
        loop {
            match Client::connect(&self.sock, kind).await {
                Ok(c) => return c,
                Err(e) if start.elapsed() > Duration::from_secs(5) => {
                    panic!("無法連線 daemon：{e}")
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }

    async fn stop(&mut self, c: &Client) {
        let _ = c
            .request_timeout(
                ClientMsg::Shutdown { kill_agents: true },
                Duration::from_secs(5),
            )
            .await;
        if let Some(mut d) = self.daemon.take() {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                if d.try_wait().unwrap().is_some() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let _ = d.kill();
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
        }
    }
}

fn create(agent: &str, repo: &Path, task: &str) -> ClientMsg {
    ClientMsg::CreateSession(CreateSession {
        agent: agent.into(),
        repo: repo.to_string_lossy().into_owned(),
        task: task.into(),
        base_branch: None,
        share: None,
        trust_setup: false,
        cols: Some(80),
        rows: Some(24),
    })
}

/// 等待事件直到條件成立；同時維護 attach 中的畫面。
async fn wait_until(
    ev: &mut tokio::sync::mpsc::UnboundedReceiver<Incoming>,
    screen: &mut Option<Screen>,
    timeout: Duration,
    mut cond: impl FnMut(&Incoming, &Option<Screen>) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(Some(inc)) = tokio::time::timeout(left, ev.recv()).await else {
            return false;
        };
        if let Incoming::Screen(u) = &inc {
            match u {
                ScreenUpdate::Snapshot { screen: s, .. } => *screen = Some(s.clone()),
                ScreenUpdate::Diff { diff, .. } => {
                    if let Some(s) = screen.as_mut() {
                        s.apply(diff);
                    }
                }
            }
        }
        if cond(&inc, screen) {
            return true;
        }
    }
}

fn screen_has(s: &Option<Screen>, text: &str) -> bool {
    s.as_ref()
        .is_some_and(|s| (0..s.lines.len()).any(|y| s.row_text(y).contains(text)))
}

/// 假 Agent：把收到的參數記到 launches.txt；`new <id>` 時建立對話檔，
/// `resume <id>` 時對話檔不存在就以 1 結束（模擬 Claude 找不到對話）。
const CONV_AGENT: &str = r#"
echo "$1 $2" >> launches.txt
if [ "$1" = resume ] && [ ! -f "conv-$2" ]; then exit 1; fi
touch "conv-$2"
"#;

fn launches(dir: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(dir.join("launches.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

async fn wait_launches(dir: &Path, n: usize) -> Vec<(String, String)> {
    let start = Instant::now();
    loop {
        let l = launches(dir);
        if l.len() >= n || start.elapsed() > Duration::from_secs(10) {
            return l;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 等 Agent 結束（restart 只能重新啟動已結束的 Agent）。
async fn wait_exited(c: &Client, id: &str) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        let list = expect_sessions(c.request(ClientMsg::ListSessions).await.unwrap()).unwrap();
        if list
            .iter()
            .any(|s| s.id == id && matches!(s.status, AgentStatus::Exited | AgentStatus::Error))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("Agent 沒有結束");
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_resumes_conversation() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;

    let s = expect_session(c.request(create("conv", &env.repo, "conv")).await.unwrap()).unwrap();
    let dir = PathBuf::from(&s.cwd);
    let restart = |fresh| ClientMsg::Restart {
        id: s.id.clone(),
        fresh,
    };

    // 新分頁 → new；R → 接續同一個對話；N → 新的對話
    let l = wait_launches(&dir, 1).await;
    assert_eq!(l[0].0, "new");
    let c1 = l[0].1.clone();
    wait_exited(&c, &s.id).await;
    c.request(restart(false)).await.unwrap();
    assert_eq!(
        wait_launches(&dir, 2).await[1],
        ("resume".into(), c1.clone())
    );
    wait_exited(&c, &s.id).await;
    c.request(restart(true)).await.unwrap();
    let l = wait_launches(&dir, 3).await;
    assert_eq!(l[2].0, "new");
    let c2 = l[2].1.clone();
    assert_ne!(c1, c2);

    // 對話不見了：接續失敗 → 自動改開新對話
    wait_exited(&c, &s.id).await;
    std::fs::remove_file(dir.join(format!("conv-{c2}"))).unwrap();
    c.request(restart(false)).await.unwrap();
    let l = wait_launches(&dir, 5).await;
    assert!(
        l.len() >= 5,
        "{l:?}\n{}",
        std::fs::read_to_string(env.home.join("data/agtd.log")).unwrap_or_default()
    );
    assert_eq!(l[3], ("resume".into(), c2.clone()));
    assert_eq!(l[4].0, "new", "{l:?}");
    let c3 = l[4].1.clone();
    assert_ne!(c2, c3);

    // daemon 重啟後，R 仍接續最後一個對話
    wait_exited(&c, &s.id).await;
    env.stop(&c).await;
    env.start();
    let Connected { client: c, .. } = env.connect("cli").await;
    c.request(ClientMsg::Restart {
        id: s.id.clone(),
        fresh: false,
    })
    .await
    .unwrap();
    assert_eq!(wait_launches(&dir, 6).await[5], ("resume".into(), c3));
}

#[tokio::test(flavor = "multi_thread")]
async fn full_flow() {
    let mut env = Env::new();
    let Connected {
        client: c,
        events: mut ev,
        hello,
    } = env.connect("tui").await;
    assert!(
        matches!(
            hello,
            ReplyBody::Hello {
                config_error: None,
                ..
            }
        ),
        "{hello:?}"
    );

    // ── 建立兩個分頁：各自的 worktree 與 branch ──
    let a = expect_session(
        c.request(create("fake", &env.repo, "Login page"))
            .await
            .unwrap(),
    )
    .unwrap();
    let b = expect_session(
        c.request(create("fake", &env.repo, "重構介面"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_ne!(a.worktree, b.worktree);
    let ba = a.branch.clone().unwrap();
    let bb = b.branch.clone().unwrap();
    assert!(ba.starts_with("agt/fake/login-page-"), "{ba}");
    assert_eq!(
        bb,
        format!("agt/fake/{}", b.short_id),
        "中文任務名稱省略 slug"
    );
    let wa = PathBuf::from(a.worktree.clone().unwrap());
    assert!(wa.join(".env").exists(), "copy_files 應複製 .env");

    // ── attach，等待 WaitingInput（規則比對 + 靜止 500 ms）──
    c.request(ClientMsg::Attach { id: a.id.clone() })
        .await
        .unwrap();
    let mut screen = None;
    let start = Instant::now();
    let ok = wait_until(&mut ev, &mut screen, Duration::from_secs(10), |inc, _| {
        matches!(inc, Incoming::Msg(m) if matches!(&**m, ServerMsg::StatusChanged { id, to: AgentStatus::WaitingInput, .. } if *id == a.id))
    })
    .await;
    if !ok {
        let info = c
            .request(ClientMsg::GetSession { id: a.id.clone() })
            .await
            .unwrap();
        let text: Vec<String> = screen
            .as_ref()
            .map(|s| (0..s.lines.len()).map(|y| s.row_text(y)).collect())
            .unwrap_or_default();
        panic!(
            "應在時限內進入 WaitingInput；{info:?}
畫面：{text:#?}"
        );
    }
    assert!(start.elapsed() < Duration::from_secs(5));
    let ok = wait_until(&mut ev, &mut screen, Duration::from_secs(3), |_, s| {
        screen_has(s, "中文輸出")
    })
    .await
        || screen_has(&screen, "中文輸出");
    assert!(ok, "畫面應顯示中文輸出");

    // ── 輸入 ──
    c.input(&a.id, b"y\r").unwrap();
    let ok = wait_until(&mut ev, &mut screen, Duration::from_secs(5), |_, s| {
        screen_has(s, "answer=y")
    })
    .await;
    assert!(ok, "輸入應送到 Agent");

    // ── 兩個 branch 互不影響 ──
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        let n: u32 = git(&env.repo, &["rev-list", "--count", &format!("main..{bb}")])
            .parse()
            .unwrap();
        if n == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        git(&env.repo, &["rev-list", "--count", &format!("main..{ba}")]),
        "1"
    );
    assert_eq!(
        git(&env.repo, &["rev-list", "--count", &format!("main..{bb}")]),
        "1"
    );

    // ── hooks 回報（agt notify 送出的訊息）──
    let n = Client::connect(&env.sock, "notify").await.unwrap();
    n.client
        .request(ClientMsg::Notify {
            session_id: a.id.clone(),
            status: AgentStatus::Done,
            message: None,
            tool: None,
        })
        .await
        .unwrap();
    let ok = wait_until(&mut ev, &mut screen, Duration::from_secs(3), |inc, _| {
        matches!(inc, Incoming::Msg(m) if matches!(**m, ServerMsg::StatusChanged { to: AgentStatus::Done, .. }))
    })
    .await;
    assert!(ok);

    // ── git 資訊推播 ──
    // 只有狀態改變時才推播，第一次的推播可能在前面等待其他事件時就被略過了；
    // 加一個未追蹤的檔案製造新的變化（dirty），一定會再推播一次（merge 不看未追蹤的檔案）。
    std::fs::write(wa.join("untracked.txt"), "x").unwrap();
    let ok = wait_until(&mut ev, &mut screen, Duration::from_secs(8), |inc, _| {
        matches!(inc, Incoming::Msg(m) if matches!(&**m, ServerMsg::WorktreeChanged { id, ahead: 1, dirty: true, .. } if *id == a.id))
    })
    .await;
    assert!(ok, "應推播 WorktreeChanged");

    // ── diff ──
    let ReplyBody::Text(d) = c
        .request(ClientMsg::Diff {
            id: a.short_id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(d.contains("work-"), "{d}");

    // ── merge 需要策略（設定為 ask）──
    let e = c
        .request(ClientMsg::Finish {
            id: a.id.clone(),
            action: FinishAction::Merge { strategy: None },
            force: false,
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(codes::NEED_STRATEGY));
    c.request(ClientMsg::Finish {
        id: a.id.clone(),
        action: FinishAction::Merge {
            strategy: Some(MergeStrategy::Squash),
        },
        force: false,
    })
    .await
    .unwrap();
    assert!(env.repo.join(format!("work-{}.txt", a.short_id)).exists());

    // ── discard 有未合併的 commit 時必須再確認 ──
    let e = c
        .request(ClientMsg::Finish {
            id: b.id.clone(),
            action: FinishAction::Discard,
            force: false,
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(codes::NEEDS_CONFIRM));
    c.request(ClientMsg::Finish {
        id: b.id.clone(),
        action: FinishAction::Discard,
        force: true,
    })
    .await
    .unwrap();
    assert!(!Path::new(b.worktree.as_ref().unwrap()).exists());
    assert!(git(&env.repo, &["branch", "--list", &bb]).is_empty());
    assert!(
        git(&env.repo, &["branch", "--list", "main"]).contains("main"),
        "絕不刪除非 agt/ 的 branch"
    );

    // ── 不是 git repo 的目錄 ──
    let plain = env.home.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let p = expect_session(c.request(create("shell", &plain, "")).await.unwrap()).unwrap();
    assert!(p.worktree.is_none());
    // 收尾失敗會寫進 agtd.log（錯誤視窗關掉後也查得到）
    c.request(ClientMsg::Finish {
        id: p.id.clone(),
        action: FinishAction::Pr,
        force: false,
    })
    .await
    .unwrap_err();
    let log = std::fs::read_to_string(env.home.join("data/agtd.log")).unwrap_or_default();
    assert!(log.contains(&format!("{}：收尾 pr 失敗", p.id)), "{log}");

    // ── setup 指令第一次執行前要確認 ──
    std::fs::write(env.repo.join(".agt.toml"), "setup = \"echo SETUP-RAN\"\n").unwrap();
    let e = c
        .request(create("fake", &env.repo, "setup"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(codes::SETUP_UNTRUSTED));
    assert!(
        git(&env.repo, &["branch", "--list", "agt/fake/setup-*"]).is_empty(),
        "被拒絕時不應留下 branch"
    );
    let mut req = match create("fake", &env.repo, "setup") {
        ClientMsg::CreateSession(r) => r,
        _ => unreachable!(),
    };
    req.trust_setup = true;
    let s = expect_session(c.request(ClientMsg::CreateSession(req)).await.unwrap()).unwrap();
    c.request(ClientMsg::Attach { id: s.id.clone() })
        .await
        .unwrap();
    let mut scr = None;
    let ok = wait_until(&mut ev, &mut scr, Duration::from_secs(5), |inc, sc| {
        matches!(inc, Incoming::Screen(u) if u.id() == s.id) && screen_has(sc, "SETUP-RAN")
    })
    .await;
    assert!(ok, "setup 的輸出應顯示在分頁中");
    // 已信任後不再詢問
    c.request(create("fake", &env.repo, "setup again"))
        .await
        .unwrap();

    // ── 未知的 Agent 與協定版本 ──
    assert!(c.request(create("nope", &env.repo, "")).await.is_err());

    // ── daemon 重啟後，工作階段保留為 Exited ──
    let list = expect_sessions(c.request(ClientMsg::ListSessions).await.unwrap()).unwrap();
    assert_eq!(list.len(), 4, "a、plain、setup、setup again");
    env.stop(&c).await;
    env.start();
    let Connected { client: c2, .. } = env.connect("cli").await;
    let list = expect_sessions(c2.request(ClientMsg::ListSessions).await.unwrap()).unwrap();
    assert_eq!(list.len(), 4);
    assert!(
        list.iter()
            .all(|s| s.status == AgentStatus::Exited || s.status == AgentStatus::Error)
    );
    // 在同一個 worktree 重新啟動。用 setup 分頁（s）而不是 a：下面要刪掉 a 的 worktree，
    // Windows 不能刪除正在執行的 Agent 所在的目錄。
    let r = expect_session(
        c2.request(ClientMsg::Restart {
            id: s.short_id.clone(),
            fresh: false,
        })
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(r.status, AgentStatus::Starting);

    // ── gc：一致時沒有項目；手動刪掉 worktree 後回報 ──
    let ReplyBody::Gc(items) = c2.request(ClientMsg::Gc { prune: false }).await.unwrap() else {
        panic!()
    };
    assert!(items.is_empty(), "{items:?}");
    git(
        &env.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agt/x/orphan",
            env.home.join("orphan").to_str().unwrap(),
        ],
    );
    let ReplyBody::Gc(items) = c2.request(ClientMsg::Gc { prune: false }).await.unwrap() else {
        panic!()
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "orphan_worktree");
    // prune 移除乾淨的孤兒 worktree（branch 保留）
    let ReplyBody::Gc(_) = c2.request(ClientMsg::Gc { prune: true }).await.unwrap() else {
        panic!()
    };
    assert!(!env.home.join("orphan").exists());
    assert!(!git(&env.repo, &["branch", "--list", "agt/x/orphan"]).is_empty());
    // 遺失的 worktree：資料庫有、磁碟上沒有
    // Windows：daemon 每 3 秒的背景 git 輪詢可能剛好正在這個資料夾裡執行，刪除會暫時失敗，重試即可
    let mut tries = 0;
    while let Err(e) = std::fs::remove_dir_all(&wa) {
        tries += 1;
        assert!(tries < 50, "無法刪除 worktree：{e}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let ReplyBody::Gc(items) = c2.request(ClientMsg::Gc { prune: true }).await.unwrap() else {
        panic!()
    };
    assert!(
        items.iter().any(|i| i.kind == "missing_worktree"),
        "{items:?}"
    );
    let ReplyBody::Gc(items) = c2.request(ClientMsg::Gc { prune: false }).await.unwrap() else {
        panic!()
    };
    assert!(items.is_empty(), "prune 後應一致：{items:?}");

    // ── 關閉分頁時保留副本：gc 回報 closed_worktree，prune 移除副本與已合併的 branch ──
    let k = expect_session(
        c2.request(create("fake", &env.repo, "keep copy"))
            .await
            .unwrap(),
    )
    .unwrap();
    let wk = PathBuf::from(k.worktree.clone().unwrap());
    let bk = k.branch.clone().unwrap();
    c2.request(ClientMsg::Kill {
        id: k.id.clone(),
        cleanup: false,
        force: false,
    })
    .await
    .unwrap();
    assert!(wk.exists(), "關閉時選擇保留，副本應還在");
    let ReplyBody::Gc(items) = c2.request(ClientMsg::Gc { prune: false }).await.unwrap() else {
        panic!()
    };
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, "closed_worktree");
    assert_eq!(items[0].session.as_deref(), Some(k.short_id.as_str()));
    // 等 Agent 結束，不再寫入副本
    tokio::time::sleep(Duration::from_millis(500)).await;
    // 有未 commit 的變更：prune 保留副本
    std::fs::write(wk.join("note.txt"), "keep me").unwrap();
    let ReplyBody::Gc(_) = c2.request(ClientMsg::Gc { prune: true }).await.unwrap() else {
        panic!()
    };
    assert!(wk.join("note.txt").exists(), "有變更的副本應保留");
    // 清乾淨、回到 base 後（ignored 的 .env 不算變更）prune 移除副本與 branch。
    // 假 Agent 有時會 commit，那種 branch 會被保留，所以先回到 base。
    git(
        &wk,
        &["reset", "-q", "--hard", k.base_branch.as_deref().unwrap()],
    );
    git(&wk, &["clean", "-q", "-fd"]);
    // Windows：剛結束的 Agent 可能還佔著資料夾，移除失敗時 prune 會保留，重試即可
    for _ in 0..50 {
        let ReplyBody::Gc(_) = c2.request(ClientMsg::Gc { prune: true }).await.unwrap() else {
            panic!()
        };
        if !wk.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!wk.exists(), "prune 應移除乾淨的副本");
    assert!(
        git(&env.repo, &["branch", "--list", &bk]).is_empty(),
        "沒有新 commit 的 branch 應一併刪除"
    );
    let ReplyBody::Gc(items) = c2.request(ClientMsg::Gc { prune: false }).await.unwrap() else {
        panic!()
    };
    assert!(items.is_empty(), "清理後應一致：{items:?}");

    // 同一條連線連續送出多個請求（DaemonStatus 曾因重複上鎖而死結）
    for _ in 0..3 {
        let ReplyBody::Daemon(info) = c2
            .request_timeout(ClientMsg::DaemonStatus, Duration::from_secs(3))
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(info.sessions, 4);
    }
    expect_sessions(
        c2.request_timeout(ClientMsg::ListSessions, Duration::from_secs(3))
            .await
            .unwrap(),
    )
    .unwrap();

    env.stop(&c2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn proto_mismatch_and_hello_required() {
    use agt_proto::codec::{read_frame, write_frame};
    let env = Env::new();
    let _ = env.connect("cli").await; // 等 daemon 起來

    let mut s = agt_proto::transport::connect(&env.sock).await.unwrap();
    let req = Request {
        req_id: 1,
        msg: ClientMsg::Hello {
            proto_version: PROTO_VERSION + 1,
            client_kind: "tui".into(),
        },
    };
    write_frame(&mut s, KIND_JSON, &serde_json::to_vec(&req).unwrap())
        .await
        .unwrap();
    let (_, payload) = read_frame(&mut s).await.unwrap().unwrap();
    let msg: ServerMsg = serde_json::from_slice(&payload).unwrap();
    assert!(
        matches!(msg, ServerMsg::Reply { result: ReplyBody::Error { ref code, .. }, .. } if code == codes::PROTO_MISMATCH),
        "{msg:?}"
    );

    let mut s = agt_proto::transport::connect(&env.sock).await.unwrap();
    let req = Request {
        req_id: 1,
        msg: ClientMsg::ListSessions,
    };
    write_frame(&mut s, KIND_JSON, &serde_json::to_vec(&req).unwrap())
        .await
        .unwrap();
    let (_, payload) = read_frame(&mut s).await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&payload).contains("Hello"));

    // 超過 16 MiB 的封包會被關閉
    let mut s = agt_proto::transport::connect(&env.sock).await.unwrap();
    use tokio::io::AsyncWriteExt;
    s.write_all(&(32u32 * 1024 * 1024).to_be_bytes())
        .await
        .unwrap();
    // Unix 是 EOF（Ok(None)），Windows 的 named pipe 是錯誤（pipe 已關閉）；兩者都表示連線被關閉
    let r = read_frame(&mut s).await;
    assert!(!matches!(r, Ok(Some(_))), "{r:?}");
}
