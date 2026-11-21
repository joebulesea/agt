//! 端對端測試：啟動真正的 daemon（`agt daemon run`），以假 Agent 腳本 + 暫存 git repo 走完整流程。
//! （規格 12 的 tests/ 目錄；放在 agt-cli 底下才能取得 agt 執行檔路徑。）
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

/// 測試用的 agt。交叉編譯後在別的機器上執行時，編譯時記下的路徑不存在，
/// 改找與測試執行檔相鄰的 agt。
fn agt_exe() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_agt"));
    if built.is_file() {
        built
    } else {
        agt_proto::transport::sibling_exe("agt")
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

/// 假 Agent：做出三個互不相關的變更，並用 .agt/groups/ 分成 one、two 兩組（其餘自成 rest）。
/// two 的 FILES 最後寫，測試以它出現當作「Agent 做完了」。
const GROUP_AGENT: &str = r#"
mkdir -p one .agt/groups/one .agt/groups/two
echo 1 > one/a.txt
echo 2 > two.txt
echo r > rest.txt
echo one > .agt/groups/one/FILES
printf '加入 one\n\n內文\n' > .agt/groups/one/COMMIT_MSG
echo two.txt > .agt/groups/two/FILES
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

[agents.grp]
command = '{sh}'
args = ["-c", '''{GROUP_AGENT}''']
status_source = "none"
auto_commit = true

[templates.fix]
description = "修 bug"
agent = "fake"
task = "修正"
tags = ["bug"]

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
        let child = Command::new(agt_exe())
            .args(["daemon", "run"])
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
        template: None,
        tags: vec![],
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

/// 以這個測試環境執行 `agt <args>`，回傳 (exit code, stdout 解析成的 JSON)。
fn agt_json(env: &Env, args: &[&str]) -> (i32, serde_json::Value) {
    agt_json_with(env, args, &[])
}

/// 同 `agt_json`，另外設定環境變數。
fn agt_json_with(env: &Env, args: &[&str], vars: &[(&str, &str)]) -> (i32, serde_json::Value) {
    let o = Command::new(agt_exe())
        .args(args)
        .env("AGT_HOME", &env.home)
        .env("AGT_SOCKET", &env.sock)
        .envs(vars.iter().copied())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let v = serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "agt {args:?} 的 stdout 不是 JSON：{e}\nstdout：{}\nstderr：{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    });
    (o.status.code().unwrap_or(-1), v)
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_json() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    let daemon_pid = env.daemon.as_ref().unwrap().id();

    // 執行中：daemon_running 是布林，running 仍是「執行中的分頁數」
    let (code, v) = agt_json(&env, &["daemon", "status", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["daemon_running"], true, "{v}");
    assert_eq!(v["socket"], env.sock.as_str());
    assert_eq!(v["pid"], daemon_pid);
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["sessions"], 0);
    assert_eq!(v["running"], 0, "running 是分頁數，不是布林：{v}");
    assert!(v["data_dir"].is_string(), "{v}");
    assert!(
        v["config_path"].as_str().unwrap().ends_with("config.toml"),
        "{v}"
    );

    // 有分頁之後，數字跟著變
    expect_session(
        c.request(create("fake", &env.repo, "status json"))
            .await
            .unwrap(),
    )
    .unwrap();
    let (_, v) = agt_json(&env, &["daemon", "status", "--json"]);
    assert_eq!(v["daemon_running"], true);
    assert_eq!(v["sessions"], 1, "{v}");
    assert!(v["running"].is_u64(), "{v}");

    // 結束後：daemon_running = false，仍有 socket，exit code 0
    env.stop(&c).await;
    let (code, v) = agt_json(&env, &["daemon", "status", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["daemon_running"], false, "{v}");
    assert_eq!(v["socket"], env.sock.as_str());
    assert!(v.get("pid").is_none(), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn gc_json() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    // 和 full_flow 一樣先有一個分頁，gc 才會掃描這個 repo
    expect_session(
        c.request(create("fake", &env.repo, "gc json"))
            .await
            .unwrap(),
    )
    .unwrap();

    // 一致：items 為空，pruned = false
    let (code, v) = agt_json(&env, &["gc", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["pruned"], false);
    assert_eq!(v["items"].as_array().unwrap().len(), 0, "{v}");

    // 孤兒 worktree（git 有、資料庫沒有）
    let orphan = env.home.join("orphan");
    git(
        &env.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agt/x/orphan",
            orphan.to_str().unwrap(),
        ],
    );
    let (code, v) = agt_json(&env, &["gc", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["pruned"], false);
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{v}");
    assert_eq!(items[0]["kind"], "orphan_worktree");
    assert!(items[0]["path"].as_str().unwrap().contains("orphan"), "{v}");
    assert!(items[0]["repo"].is_string(), "{v}");
    assert!(orphan.exists(), "沒有 --prune 不應刪除");

    // --prune：pruned = true，列出處理的項目，副本被移除
    let (code, v) = agt_json(&env, &["gc", "--prune", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["pruned"], true);
    assert_eq!(v["items"][0]["kind"], "orphan_worktree", "{v}");
    assert!(!orphan.exists(), "--prune 應移除乾淨的孤兒 worktree");

    // 之後恢復一致
    let (_, v) = agt_json(&env, &["gc", "--json"]);
    assert_eq!(v["items"].as_array().unwrap().len(), 0, "{v}");

    env.stop(&c).await;
}

/// `agt config check --json`：不需要 daemon，只看輸出格式與 exit code。
#[test]
fn config_check_json() {
    let env = Env::new();
    let dir = env.home.join("cc");
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, text: &str| {
        let f = dir.join(name);
        std::fs::write(&f, text).unwrap();
        f.to_string_lossy().into_owned()
    };

    // 預設的設定檔（測試環境寫的 config.toml）
    let (code, v) = agt_json(&env, &["config", "check", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["config_exists"], true);
    assert!(
        v["config_file"].as_str().unwrap().ends_with("config.toml"),
        "{v}"
    );
    assert!(v["repo"].is_null());
    assert!(v["worktree_root"].is_string());
    let agents: Vec<&str> = v["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    for a in ["fake", "conv", "grp"] {
        assert!(agents.contains(&a), "{agents:?}");
    }
    assert_eq!(v["auto_commit"]["grp"], true);
    assert_eq!(v["auto_commit"]["fake"], false);
    assert!(v["warnings"].is_array());

    // --repo：回報 repo 路徑
    let repo = env.repo.to_string_lossy().into_owned();
    let (code, v) = agt_json(&env, &["config", "check", "--json", "--repo", &repo]);
    assert_eq!(code, 0, "{v}");
    assert!(v["repo"].as_str().unwrap().ends_with("proj"), "{v}");

    // 檔案不存在：使用內建預設值
    let missing = dir.join("nope.toml").to_string_lossy().into_owned();
    let (code, v) = agt_json(&env, &["config", "check", "--json", "--file", &missing]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["config_exists"], false);

    // 警告：指令找不到。exit 0，列在 warnings
    let ghost = write(
        "ghost.toml",
        "[agents.ghost]\ncommand = \"definitely-not-installed-agt-test\"\n",
    );
    let (code, v) = agt_json(&env, &["config", "check", "--json", "--file", &ghost]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("agents.ghost.command")),
        "{v}"
    );

    // 錯誤：欄位值不合法 → exit 1，stdout 是 {ok: false, error}
    let bad = write("bad.toml", "[general]\nsidebar_width = 3\n");
    let (code, v) = agt_json(&env, &["config", "check", "--json", "--file", &bad]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["ok"], false);
    assert!(
        v["error"].as_str().unwrap().contains("sidebar_width"),
        "{v}"
    );

    // 錯誤：TOML 語法錯誤
    let broken = write("broken.toml", "[general\n");
    let (code, v) = agt_json(&env, &["config", "check", "--json", "--file", &broken]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["ok"], false);
    assert!(v["error"].is_string());
}

/// `agt update --check --json`：用 `--version` 指定版本時不會連網，所以可以在離線環境測試。
#[test]
fn update_check_json() {
    let env = Env::new();
    let current = env!("CARGO_PKG_VERSION");
    let vars = [("AGT_REPO", "example/agt-test")];
    let check = |version: &str| {
        agt_json_with(
            &env,
            &["update", "--check", "--json", "--version", version],
            &vars,
        )
    };

    // 相同版本
    let (code, v) = check(current);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["status"], "up_to_date", "{v}");
    assert_eq!(v["update_available"], false);
    assert_eq!(v["current"], current);
    assert_eq!(v["latest"], current);
    assert_eq!(v["tag"], format!("v{current}"));

    // 較新的版本（接受有沒有 v 前綴）
    for ver in ["999.0.0", "v999.0.0"] {
        let (code, v) = check(ver);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["status"], "update_available", "{v}");
        assert_eq!(v["update_available"], true);
        assert_eq!(v["latest"], "999.0.0");
        assert_eq!(v["tag"], "v999.0.0");
    }

    // 指定較舊的版本：downgrade，不算有新版
    let (code, v) = check("0.0.1");
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["status"], "downgrade", "{v}");
    assert_eq!(v["update_available"], false);

    // 版本格式錯誤：exit 1，stdout 是 {error}
    let (code, v) = check("not-a-version");
    assert_eq!(code, 1, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("版本格式不正確"),
        "{v}"
    );
    assert!(v.get("status").is_none());

    // AGT_REPO 格式不對：exit 1，錯誤訊息提到 AGT_REPO
    let (code, v) = agt_json_with(
        &env,
        &["update", "--check", "--json", "--version", "1.0.0"],
        &[("AGT_REPO", "no-slash")],
    );
    assert_eq!(code, 1, "{v}");
    assert!(v["error"].as_str().unwrap().contains("AGT_REPO"), "{v}");

    // --json 必須搭配 --check：clap 直接拒絕（exit 2），不會執行更新
    let o = Command::new(agt_exe())
        .args(["update", "--json", "--version", current])
        .env("AGT_HOME", &env.home)
        .env("AGT_SOCKET", &env.sock)
        .envs(vars)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--check"));
}

/// 刪除資料夾；Windows 上 daemon 的背景 git 輪詢可能暫時佔著，失敗時重試。
async fn remove_dir_retry(p: &Path) {
    let mut tries = 0;
    while let Err(e) = std::fs::remove_dir_all(p) {
        tries += 1;
        assert!(tries < 50, "無法刪除 {}：{e}", p.display());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn gc_items(v: &serde_json::Value) -> &Vec<serde_json::Value> {
    v["items"].as_array().unwrap_or_else(|| panic!("{v}"))
}

/// 已關閉分頁留下的副本：欄位內容、有變更時 prune 保留、清乾淨後 prune 移除副本與 branch。
#[tokio::test(flavor = "multi_thread")]
async fn gc_json_closed_worktree() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    let k = expect_session(
        c.request(create("fake", &env.repo, "closed"))
            .await
            .unwrap(),
    )
    .unwrap();
    let wk = PathBuf::from(k.worktree.clone().unwrap());
    let bk = k.branch.clone().unwrap();
    c.request(ClientMsg::Kill {
        id: k.id.clone(),
        cleanup: false,
        force: false,
    })
    .await
    .unwrap();
    assert!(wk.exists(), "關閉時選擇保留，副本應還在");
    // 等 Agent 結束，不再寫入副本
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (code, v) = agt_json(&env, &["gc", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["pruned"], false);
    let items = gc_items(&v);
    assert_eq!(items.len(), 1, "{v}");
    let it = &items[0];
    assert_eq!(it["kind"], "closed_worktree");
    assert_eq!(it["session"], k.short_id.as_str());
    assert_eq!(it["branch"], bk.as_str());
    assert_eq!(
        Path::new(it["path"].as_str().unwrap()).file_name(),
        wk.file_name()
    );
    assert!(it["repo"].is_string());
    assert!(wk.exists(), "不加 --prune 不會動副本");

    // 有未 commit 的變更：--prune 仍列出該項目，但副本與變更保留
    std::fs::write(wk.join("note.txt"), "keep me").unwrap();
    let (_, v) = agt_json(&env, &["gc", "--prune", "--json"]);
    assert_eq!(v["pruned"], true);
    assert_eq!(gc_items(&v).len(), 1, "{v}");
    assert!(wk.join("note.txt").exists(), "有變更的副本應保留");
    let (_, v) = agt_json(&env, &["gc", "--json"]);
    assert_eq!(gc_items(&v).len(), 1, "保留的副本下次仍會列出：{v}");

    // 回到 base 並清乾淨：--prune 移除副本與沒有新 commit 的 branch
    git(
        &wk,
        &["reset", "-q", "--hard", k.base_branch.as_deref().unwrap()],
    );
    git(&wk, &["clean", "-q", "-fd"]);
    for _ in 0..50 {
        let (_, v) = agt_json(&env, &["gc", "--prune", "--json"]);
        assert_eq!(v["pruned"], true);
        if !wk.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!wk.exists(), "prune 應移除乾淨的副本");
    assert!(git(&env.repo, &["branch", "--list", &bk]).is_empty());
    let (_, v) = agt_json(&env, &["gc", "--json"]);
    assert!(gc_items(&v).is_empty(), "清理後應一致：{v}");

    env.stop(&c).await;
}

/// 資料庫有、磁碟上沒有：missing_worktree，prune 修正資料庫後恢復一致。
#[tokio::test(flavor = "multi_thread")]
async fn gc_json_missing_worktree() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    let m = expect_session(
        c.request(create("fake", &env.repo, "missing"))
            .await
            .unwrap(),
    )
    .unwrap();
    let wm = PathBuf::from(m.worktree.clone().unwrap());
    // 結束 daemon 與 Agent 後再刪 worktree（Windows 不能刪除執行中 Agent 所在的目錄）
    env.stop(&c).await;
    env.start();
    let Connected { client: c, .. } = env.connect("cli").await;
    remove_dir_retry(&wm).await;

    let (code, v) = agt_json(&env, &["gc", "--json"]);
    assert_eq!(code, 0, "{v}");
    let items = gc_items(&v);
    assert_eq!(items.len(), 1, "{v}");
    assert_eq!(items[0]["kind"], "missing_worktree");
    assert_eq!(items[0]["session"], m.short_id.as_str());
    assert_eq!(items[0]["branch"], m.branch.as_deref().unwrap());

    let (_, v) = agt_json(&env, &["gc", "--prune", "--json"]);
    assert_eq!(v["pruned"], true);
    assert_eq!(gc_items(&v).len(), 1, "{v}");
    let (_, v) = agt_json(&env, &["gc", "--json"]);
    assert!(gc_items(&v).is_empty(), "prune 後應一致：{v}");

    env.stop(&c).await;
}

/// 文字模式（沒有 --json）：一致時的訊息、孤兒的說明與 prune 提示。
#[tokio::test(flavor = "multi_thread")]
async fn gc_text_output() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    expect_session(c.request(create("fake", &env.repo, "text")).await.unwrap()).unwrap();
    let run = |args: &[&str]| {
        let o = Command::new(agt_exe())
            .args(args)
            .env("AGT_HOME", &env.home)
            .env("AGT_SOCKET", &env.sock)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        )
    };

    let (code, out) = run(&["gc"]);
    assert_eq!(code, Some(0));
    assert!(out.contains("worktree 與資料庫一致"), "{out}");
    assert!(!out.contains("--prune"), "{out}");

    let orphan = env.home.join("orphan-text");
    git(
        &env.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agt/x/orphan-text",
            orphan.to_str().unwrap(),
        ],
    );
    let (code, out) = run(&["gc"]);
    assert_eq!(code, Some(0));
    assert!(out.contains("孤兒 worktree"), "{out}");
    assert!(out.contains("orphan-text"), "{out}");
    assert!(out.contains("agt gc --prune"), "{out}");
    assert!(orphan.exists());

    // 文字模式不會輸出 JSON
    assert!(serde_json::from_str::<serde_json::Value>(&out).is_err());

    env.stop(&c).await;
}

/// `agt config check` 的 exit code：用法錯誤是 2（clap 在載入設定之前就拒絕，stdout 為空），
/// 設定檔的錯誤是 1。
#[test]
fn config_check_usage_errors_exit_2() {
    let env = Env::new();
    let run = |args: &[&str]| {
        let o = Command::new(agt_exe())
            .args(args)
            .env("AGT_HOME", &env.home)
            .env("AGT_SOCKET", &env.sock)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };
    for args in [
        &["config", "check", "--file"][..],
        &["config", "check", "--repo"],
        &["config", "check", "--json", "--file"],
        &["config", "check", "--json", "--repo"],
        &["config", "check", "--bogus"],
        &["config", "check", "--json", "--bogus"],
    ] {
        let (code, out, err) = run(args);
        assert_eq!(code, Some(2), "{args:?}\nstdout：{out}\nstderr：{err}");
        assert!(out.is_empty(), "{args:?} 不應有 stdout：{out}");
        assert!(!err.is_empty(), "{args:?} 應說明用法");
    }

    // 對照：設定檔本身的錯誤是 exit 1（stdout 才有內容）
    let bad = env.home.join("bad-usage.toml");
    std::fs::write(&bad, "[general]\nsidebar_width = 3\n").unwrap();
    let (code, out, _) = run(&["config", "check", "--file", bad.to_str().unwrap()]);
    assert_eq!(code, Some(1), "{out}");
}

/// 在暫存資料夾放一個假的 `curl`（shell 腳本），回傳該資料夾；把它設成 PATH 就能不連網地
/// 模擬 GitHub 的各種回應。
#[cfg(unix)]
fn fake_curl(env: &Env, name: &str, script: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let dir = env.home.join(format!("fake-curl-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("curl");
    std::fs::write(&f, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir.to_string_lossy().into_owned()
}

/// `agt update --check --json` 查詢最新版本（不加 `--version`）時的網路失敗與成功路徑。
/// 用假的 curl 取代真正的網路，所以結果固定；Windows 的 curl.exe 不能這樣替換，只在 Unix 執行。
#[cfg(unix)]
#[test]
fn update_check_json_network_failures() {
    let env = Env::new();
    let args = ["update", "--check", "--json"];
    let run = |path: &str| {
        agt_json_with(
            &env,
            &args,
            &[("AGT_REPO", "example/agt-test"), ("PATH", path)],
        )
    };

    // 連線失敗（curl -f 遇到 404 / 連不上都是 exit 22 之類的非 0）
    let dir = fake_curl(
        &env,
        "fail",
        "echo 'curl: (22) The requested URL returned error: 404' >&2\nexit 22",
    );
    let (code, v) = run(&dir);
    assert_eq!(code, 1, "{v}");
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("查詢最新版本失敗"), "{e}");
    assert!(e.contains("404"), "應帶出 curl 的錯誤訊息：{e}");
    assert!(v.get("status").is_none(), "{v}");

    // 系統找不到 curl
    let empty = env.home.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let (code, v) = run(empty.to_str().unwrap());
    assert_eq!(code, 1, "{v}");
    assert!(v["error"].as_str().unwrap().contains("curl"), "{v}");

    // 連得上，但還沒有任何 Release（redirect 回 /releases，最後一段不是版本）
    let dir = fake_curl(
        &env,
        "norelease",
        "printf '%s' 'https://github.com/example/agt-test/releases'",
    );
    let (code, v) = run(&dir);
    assert_eq!(code, 1, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("無法判斷最新版本"),
        "{v}"
    );

    // 對照：成功時由 redirect 後的網址取出版本
    let dir = fake_curl(
        &env,
        "ok",
        "printf '%s' 'https://github.com/example/agt-test/releases/tag/v999.0.0'",
    );
    let (code, v) = run(&dir);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["status"], "update_available", "{v}");
    assert_eq!(v["latest"], "999.0.0");
    assert_eq!(v["tag"], "v999.0.0");
}

/// 網路連不上時的 `agt update --check --json`，在所有平台（含 Windows）執行。
///
/// Windows 的 curl.exe 在系統資料夾，不能用 PATH 換成假的，所以改用 curl 讀取的 proxy 環境變數，
/// 把連線導向一個沒有人在聽的本機 port：不需要真的離線，也不會連到 GitHub，curl 立刻失敗。
#[test]
fn update_check_json_unreachable_network() {
    let env = Env::new();
    let dead = "http://127.0.0.1:1";
    let (code, v) = agt_json_with(
        &env,
        &["update", "--check", "--json"],
        &[
            ("AGT_REPO", "example/agt-test"),
            ("HTTPS_PROXY", dead),
            ("https_proxy", dead),
            ("ALL_PROXY", dead),
            ("all_proxy", dead),
            ("NO_PROXY", ""),
            ("no_proxy", ""),
        ],
    );
    assert_eq!(code, 1, "{v}");
    let e = v["error"].as_str().unwrap_or_else(|| panic!("{v}"));
    assert!(e.contains("查詢最新版本失敗"), "{e}");
    assert!(v.get("status").is_none() && v.get("update_available").is_none());
}

/// 執行 `agt <args>`，回傳 (exit code, stdout, stderr) 原文。
fn agt_raw(env: &Env, args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(agt_exe())
        .args(args)
        .env("AGT_HOME", &env.home)
        .env("AGT_SOCKET", &env.sock)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// 同時有三種不一致項目：一次列出、每筆欄位完整、輸出只有 JSON；`--prune` 後恢復一致且可重複執行。
#[tokio::test(flavor = "multi_thread")]
async fn gc_json_mixed_items_and_output_hygiene() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    // 要保留副本的分頁、之後會遺失 worktree 的分頁
    let k = expect_session(c.request(create("fake", &env.repo, "kept")).await.unwrap()).unwrap();
    let m = expect_session(c.request(create("fake", &env.repo, "lost")).await.unwrap()).unwrap();
    let wk = PathBuf::from(k.worktree.clone().unwrap());
    let wm = PathBuf::from(m.worktree.clone().unwrap());
    c.request(ClientMsg::Kill {
        id: k.id.clone(),
        cleanup: false,
        force: false,
    })
    .await
    .unwrap();
    env.stop(&c).await;
    env.start();
    let Connected { client: c, .. } = env.connect("cli").await;
    remove_dir_retry(&wm).await;
    // 孤兒（agt 的 branch）；另一個不是 agt 建立的 worktree 必須被忽略
    let orphan = env.home.join("orphan-mixed");
    git(
        &env.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agt/x/orphan-mixed",
            orphan.to_str().unwrap(),
        ],
    );
    let foreign = env.home.join("foreign");
    git(
        &env.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/not-agt",
            foreign.to_str().unwrap(),
        ],
    );

    // stdout 只有一份 JSON，stderr 沒有東西，exit 0（即使有項目）
    let (code, out, err) = agt_raw(&env, &["gc", "--json"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(err.is_empty(), "stderr 應為空：{err}");
    assert!(
        out.trim_start().starts_with('{') && out.trim_end().ends_with('}'),
        "{out}"
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["pruned"], false);
    let items = gc_items(&v);
    assert_eq!(items.len(), 3, "{v}");
    let mut kinds: Vec<&str> = items.iter().map(|i| i["kind"].as_str().unwrap()).collect();
    kinds.sort();
    assert_eq!(
        kinds,
        ["closed_worktree", "missing_worktree", "orphan_worktree"]
    );
    for it in items {
        // 五個欄位一個不缺（沒有值是 null）
        assert_eq!(it.as_object().unwrap().len(), 5, "{it}");
        assert_eq!(
            Path::new(it["repo"].as_str().unwrap()).file_name(),
            env.repo.file_name()
        );
    }
    let by = |kind: &str| items.iter().find(|i| i["kind"] == kind).unwrap();
    assert_eq!(by("orphan_worktree")["branch"], "agt/x/orphan-mixed");
    assert!(by("orphan_worktree")["session"].is_null());
    assert_eq!(by("closed_worktree")["session"], k.short_id.as_str());
    assert_eq!(by("missing_worktree")["session"], m.short_id.as_str());
    assert!(
        !items
            .iter()
            .any(|i| i["path"].as_str().unwrap().contains("foreign"))
    );

    // 假 Agent 可能剛好在被結束前寫了檔案：先回到 base 並清乾淨，prune 才會移除副本
    git(
        &wk,
        &["reset", "-q", "--hard", k.base_branch.as_deref().unwrap()],
    );
    git(&wk, &["clean", "-q", "-fd"]);

    // 選項順序不影響：`--json --prune` 與 `--prune --json` 相同
    let (code, out, err) = agt_raw(&env, &["gc", "--json", "--prune"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(err.is_empty(), "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["pruned"], true);
    assert_eq!(gc_items(&v).len(), 3, "{v}");
    assert!(!orphan.exists(), "乾淨的孤兒應被移除");
    assert!(foreign.exists(), "不是 agt 建立的 worktree 不應被動到");

    // 恢復一致後再 prune：不報錯、items 為空（可重複執行）
    for _ in 0..50 {
        if !wk.exists() {
            break;
        }
        let _ = agt_json(&env, &["gc", "--prune", "--json"]);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (code, v) = agt_json(&env, &["gc", "--prune", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["pruned"], true);
    assert!(gc_items(&v).is_empty(), "{v}");

    env.stop(&c).await;
}

/// 有未 commit 變更的孤兒 worktree：`--prune` 不會刪掉它，branch 也保留，下次仍會列出。
#[tokio::test(flavor = "multi_thread")]
async fn gc_json_dirty_orphan_is_kept() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    expect_session(c.request(create("fake", &env.repo, "base")).await.unwrap()).unwrap();
    let orphan = env.home.join("orphan-dirty");
    git(
        &env.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agt/x/dirty",
            orphan.to_str().unwrap(),
        ],
    );
    std::fs::write(orphan.join("wip.txt"), "unsaved").unwrap();

    for _ in 0..2 {
        let (code, v) = agt_json(&env, &["gc", "--prune", "--json"]);
        assert_eq!(code, 0, "{v}");
        let items = gc_items(&v);
        assert_eq!(items.len(), 1, "保留的項目每次都會列出：{v}");
        assert_eq!(items[0]["kind"], "orphan_worktree");
        assert_eq!(items[0]["branch"], "agt/x/dirty");
        assert!(
            orphan.join("wip.txt").exists(),
            "有變更的 worktree 不能被刪"
        );
    }
    assert!(!git(&env.repo, &["branch", "--list", "agt/x/dirty"]).is_empty());

    env.stop(&c).await;
}

/// daemon 沒有在執行時，`agt gc --json` 會自動啟動它，輸出仍是乾淨的 JSON。
#[tokio::test(flavor = "multi_thread")]
async fn gc_json_starts_daemon_when_needed() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    env.stop(&c).await;

    let (code, out, err) = agt_raw(&env, &["gc", "--json"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(v["pruned"], false);
    assert!(gc_items(&v).is_empty(), "{v}");

    // 自動啟動的 daemon 不是這個測試啟動的：用協定要求它結束
    let Connected { client: c, .. } = env.connect("cli").await;
    env.stop(&c).await;
}

/// 走協定（TUI 用的同一個請求）取得 daemon 資訊。
async fn daemon_info(c: &Client) -> DaemonInfo {
    let ReplyBody::Daemon(i) = c.request(ClientMsg::DaemonStatus).await.unwrap() else {
        panic!()
    };
    i
}

/// 與協定回報的資訊一致，且 clients 會隨連線數增減；關閉分頁後數字跟著變。
#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_matches_protocol_and_tracks_changes() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    let s1 = expect_session(c.request(create("fake", &env.repo, "one")).await.unwrap()).unwrap();
    let s2 = expect_session(c.request(create("fake", &env.repo, "two")).await.unwrap()).unwrap();
    let info = daemon_info(&c).await;
    let (code, v) = agt_json(&env, &["daemon", "status", "--json"]);
    assert_eq!(code, 0, "{v}");
    // 與走協定（TUI 用的同一個請求）取得的內容一致
    assert_eq!(v["pid"], info.pid);
    assert_eq!(v["version"], info.version.as_str());
    assert_eq!(v["sessions"], info.sessions);
    assert_eq!(v["running"], info.running);
    assert_eq!(v["data_dir"], info.data_dir.as_str());
    assert_eq!(v["config_path"], info.config_path.as_str());
    assert_eq!(info.sessions, 2);
    assert_eq!(v["daemon_running"], true);
    // `agt daemon status` 自己也是一條連線
    assert_eq!(v["clients"], info.clients + 1, "{v}");

    // 再開一條 TUI 連線：clients 多一條；放掉之後會回來
    let t = env.connect("tui").await;
    let (_, v) = agt_json(&env, &["daemon", "status", "--json"]);
    assert_eq!(v["clients"], info.clients + 2, "{v}");
    drop(t);
    let mut back = false;
    for _ in 0..30 {
        let (_, v) = agt_json(&env, &["daemon", "status", "--json"]);
        if v["clients"] == info.clients + 1 {
            back = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(back, "TUI 連線關閉後 clients 應該減少");

    // 關閉一個分頁：sessions 變少，running 不會增加，且仍與協定一致
    let before = daemon_info(&c).await;
    c.request(ClientMsg::Kill {
        id: s2.id.clone(),
        cleanup: true,
        force: true,
    })
    .await
    .unwrap();
    let after = daemon_info(&c).await;
    assert_eq!(after.sessions, before.sessions - 1);
    assert!(after.running <= before.running);
    let (_, v) = agt_json(&env, &["daemon", "status", "--json"]);
    assert_eq!(v["sessions"], after.sessions);
    assert_eq!(v["running"], after.running);
    let _ = s1;

    env.stop(&c).await;
}

/// 文字模式（沒有 --json）：執行中、沒在執行、以及不是 JSON。
#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_text_output() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    let pid = env.daemon.as_ref().unwrap().id();

    let (code, out, err) = agt_raw(&env, &["daemon", "status"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(
        out.contains(&format!(
            "agt daemon {}（pid {pid}）",
            env!("CARGO_PKG_VERSION")
        )),
        "{out}"
    );
    assert!(out.contains(&env.sock), "{out}");
    for label in ["socket", "工作階段", "UI 連線", "資料目錄", "設定檔"] {
        assert!(out.contains(label), "缺少「{label}」：{out}");
    }
    assert!(serde_json::from_str::<serde_json::Value>(&out).is_err());

    env.stop(&c).await;
    let (code, out, _) = agt_raw(&env, &["daemon", "status"]);
    assert_eq!(code, Some(0), "沒在執行也是 exit 0：{out}");
    assert!(out.contains("daemon 沒有在執行"), "{out}");
    assert!(out.contains(&env.sock), "{out}");
}

/// `daemon status` 只查詢：daemon 沒在執行時不會把它啟動，JSON 輸出也很乾淨。
#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_json_is_clean_and_does_not_start_daemon() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;

    // 執行中：stdout 只有 JSON、stderr 為空
    let (code, out, err) = agt_raw(&env, &["daemon", "status", "--json"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(err.is_empty(), "stderr 應為空：{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(v["daemon_running"], true);

    env.stop(&c).await;
    for _ in 0..3 {
        let (code, out, err) = agt_raw(&env, &["daemon", "status", "--json"]);
        assert_eq!(code, Some(0), "{out}\n{err}");
        assert!(err.is_empty(), "stderr 應為空：{err}");
        let v: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        assert_eq!(v["daemon_running"], false, "{v}");
    }
    // 查詢過後仍然連不上：status 沒有偷偷啟動 daemon
    assert!(Client::connect(&env.sock, "cli").await.is_err());
}

/// 對方有回應但拒絕連線（例如協定版本不同的舊 daemon）：JSON 與文字都回報錯誤，exit 0。
/// 用一個假的 server 回覆 proto_mismatch。
#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_reports_refused_connection() {
    use agt_proto::codec::{read_frame, write_frame};
    let env = Env::new();
    let fake_ep = if cfg!(windows) {
        format!("{}-fake", env.sock)
    } else {
        Path::new(&env.sock)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("fake-run")
            .join("agt.sock")
            .to_string_lossy()
            .into_owned()
    };
    let listener = agt_proto::transport::listen(&fake_ep).unwrap();
    tokio::spawn(async move {
        while let Ok(mut s) = agt_proto::transport::accept(&listener).await {
            if let Ok(Some((_, payload))) = read_frame(&mut s).await
                && let Ok(req) = serde_json::from_slice::<Request>(&payload)
            {
                let reply = ServerMsg::Reply {
                    req_id: req.req_id,
                    result: ReplyBody::Error {
                        code: codes::PROTO_MISMATCH.into(),
                        message: "協定版本不同（測試）".into(),
                    },
                };
                let _ = write_frame(&mut s, KIND_JSON, &serde_json::to_vec(&reply).unwrap()).await;
            }
        }
    });
    let run = |args: &[&str]| {
        let o = Command::new(agt_exe())
            .args(args)
            .env("AGT_HOME", &env.home)
            .env("AGT_SOCKET", &fake_ep)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };

    let (code, out, err) = run(&["daemon", "status", "--json"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(v["daemon_running"], false, "{v}");
    assert_eq!(v["socket"], fake_ep.as_str());
    assert!(v["error"].as_str().unwrap().contains("協定版本不同"), "{v}");
    assert!(v.get("pid").is_none());

    let (code, out, _) = run(&["daemon", "status"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains("daemon 回應錯誤") && out.contains("協定版本不同"),
        "{out}"
    );
}

/// `agt update --check` 的文字輸出（用 `--version` 指定，不連網）。
/// 沒有預先建置版本的平台會先因為平台檢查而失敗，所以只在支援的平台執行。
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64")
))]
#[test]
fn update_check_text_output() {
    let env = Env::new();
    let cur = env!("CARGO_PKG_VERSION");
    let run = |args: &[&str]| {
        let o = Command::new(agt_exe())
            .args(args)
            .env("AGT_HOME", &env.home)
            .env("AGT_SOCKET", &env.sock)
            .env("AGT_REPO", "example/agt-test")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };

    let (code, out, err) = run(&["update", "--check", "--version", cur]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(
        out.contains(&format!("目前版本：v{cur}，可用版本：v{cur}")),
        "{out}"
    );
    assert!(out.contains("已經是這個版本"), "{out}");
    assert!(!out.contains("執行 `agt update`"), "{out}");

    let (code, out, _) = run(&["update", "--check", "--version", "999.0.0"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("可用版本：v999.0.0"), "{out}");
    assert!(out.contains("執行 `agt update` 更新"), "{out}");
    assert!(!out.contains("下載"), "--check 不應下載：{out}");

    // 指定較舊的版本也可以（降版）：不會說「不需要更新」
    let (code, out, _) = run(&["update", "--check", "--version", "0.0.1"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(!out.contains("不需要更新"), "{out}");
    assert!(out.contains("執行 `agt update` 更新"), "{out}");

    // 版本格式錯誤：exit 1，錯誤在 stderr，stdout 沒有東西
    let (code, out, err) = run(&["update", "--check", "--version", "nope"]);
    assert_eq!(code, Some(1), "{out}\n{err}");
    assert!(err.contains("版本格式不正確"), "{err}");
    assert!(out.is_empty(), "{out}");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod update_flow {
    //! 完整的 `agt update`：用假的 curl 提供 Release，真的 tar 解壓縮，換掉一份「複製出來的 agt」
    //!（不會動到建置出來的執行檔）。
    use super::*;

    const TARGET: &str = "x86_64-unknown-linux-gnu";

    /// 建立一個假的 Release：壓縮檔、SHA256SUMS、`latest` 查詢結果，以及放在 PATH 最前面的假 curl。
    /// `sums`：`None` = 正確的雜湊；`Some(text)` = 直接用這段當 SHA256SUMS。
    fn make_release(
        env: &Env,
        name: &str,
        tag: &str,
        latest: &str,
        payload: &[u8],
        sums: Option<&str>,
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = env.home.join(format!("release-{name}"));
        let pkg = format!("agt-{tag}-{TARGET}");
        let stage = dir.join("stage").join(&pkg);
        std::fs::create_dir_all(&stage).unwrap();
        let bin = stage.join("agt");
        std::fs::write(&bin, payload).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let archive = dir.join("archive.tar.gz");
        let st = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.join("stage"))
            .arg(&pkg)
            .status()
            .unwrap();
        assert!(st.success());
        let text = match sums {
            Some(t) => t.to_string(),
            None => {
                let o = Command::new("sha256sum").arg(&archive).output().unwrap();
                let out = String::from_utf8_lossy(&o.stdout).into_owned();
                format!("{}  {pkg}.tar.gz\n", out.split_whitespace().next().unwrap())
            }
        };
        std::fs::write(dir.join("SHA256SUMS"), text).unwrap();
        std::fs::write(
            dir.join("latest"),
            format!("https://github.com/example/agt-test/releases/tag/{latest}"),
        )
        .unwrap();
        let d = dir.display();
        let script = format!(
            r#"#!/bin/sh
out=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -w) shift 2 ;;
    -*|=https) shift ;;
    *) url="$1"; shift ;;
  esac
done
case "$url" in
  */SHA256SUMS) cat "{d}/SHA256SUMS" ;;
  *.tar.gz) cp "{d}/archive.tar.gz" "$out" ;;
  */releases/latest) printf '%s' "$(cat "{d}/latest")" ;;
  *) echo "unexpected url: $url" >&2; exit 22 ;;
esac
"#
        );
        let curl_dir = dir.join("bin");
        std::fs::create_dir_all(&curl_dir).unwrap();
        let curl = curl_dir.join("curl");
        std::fs::write(&curl, script).unwrap();
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
        curl_dir
    }

    /// 複製一份 agt 當作「已安裝的版本」。
    fn install_copy(env: &Env, name: &str) -> PathBuf {
        let dir = env.home.join(format!("installed-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("agt");
        std::fs::copy(agt_exe(), &bin).unwrap();
        bin
    }

    fn run_update(
        env: &Env,
        bin: &Path,
        fake_bin: &Path,
        args: &[&str],
    ) -> (Option<i32>, String, String) {
        let path = format!(
            "{}:{}",
            fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let o = Command::new(bin)
            .args(args)
            .env("AGT_HOME", &env.home)
            .env("AGT_SOCKET", &env.sock)
            .env("AGT_REPO", "example/agt-test")
            .env("PATH", path)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    const NEW_AGT: &[u8] = b"#!/bin/sh\necho NEW-AGT\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn update_rejects_bad_release_and_keeps_installed_file() {
        let mut env = Env::new();
        let Connected { client: c, .. } = env.connect("cli").await;
        let bin = install_copy(&env, "reject");
        let original = std::fs::read(&bin).unwrap();

        // --check：只查詢，不下載、不動檔案
        let rel = make_release(&env, "ok", "v999.0.0", "v999.0.0", NEW_AGT, None);
        let (code, out, err) = run_update(&env, &bin, &rel, &["update", "--check"]);
        assert_eq!(code, Some(0), "{out}\n{err}");
        assert!(out.contains("可用版本：v999.0.0"), "{out}");
        assert!(out.contains("執行 `agt update` 更新"), "{out}");
        assert!(!out.contains("下載"), "{out}");
        assert_eq!(std::fs::read(&bin).unwrap(), original);

        // 最新版比目前版本舊：不更新
        let rel = make_release(&env, "ahead", "v0.0.1", "v0.0.1", NEW_AGT, None);
        let (code, out, err) = run_update(&env, &bin, &rel, &["update", "-y"]);
        assert_eq!(code, Some(0), "{out}\n{err}");
        assert!(out.contains("目前版本比最新發布版新，不需要更新"), "{out}");
        assert!(!out.contains("下載"), "{out}");
        assert_eq!(std::fs::read(&bin).unwrap(), original);

        // SHA256 不符：中止，檔案不變，daemon 仍在執行（核對發生在停 daemon 之前）
        let bad = format!("{}  agt-v999.0.0-{TARGET}.tar.gz\n", "0".repeat(64));
        let rel = make_release(&env, "badsum", "v999.0.0", "v999.0.0", NEW_AGT, Some(&bad));
        let (code, out, err) = run_update(&env, &bin, &rel, &["update", "-y"]);
        assert_eq!(code, Some(1), "{out}\n{err}");
        assert!(err.contains("SHA256 不符"), "{err}");
        assert!(out.contains("==> 下載"), "{out}");
        assert!(!out.contains("停止 daemon"), "{out}");
        assert_eq!(std::fs::read(&bin).unwrap(), original);
        assert!(
            Client::connect(&env.sock, "cli").await.is_ok(),
            "daemon 應仍在執行"
        );

        // SHA256SUMS 裡沒有這個壓縮檔
        let other = format!("{}  something-else.tar.gz\n", "1".repeat(64));
        let rel = make_release(&env, "nosum", "v999.0.0", "v999.0.0", NEW_AGT, Some(&other));
        let (code, _, err) = run_update(&env, &bin, &rel, &["update", "-y"]);
        assert_eq!(code, Some(1), "{err}");
        assert!(err.contains("SHA256SUMS 裡找不到"), "{err}");
        assert_eq!(std::fs::read(&bin).unwrap(), original);

        env.stop(&c).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_installs_new_version() {
        let mut env = Env::new();
        let Connected { client: c, .. } = env.connect("cli").await;
        env.stop(&c).await; // daemon 沒在執行：不需要重新啟動
        let bin = install_copy(&env, "install");
        let rel = make_release(&env, "ok", "v999.0.0", "v999.0.0", NEW_AGT, None);

        let (code, out, err) = run_update(&env, &bin, &rel, &["update", "-y"]);
        assert_eq!(code, Some(0), "{out}\n{err}");
        for want in [
            "==> 下載",
            "==> 核對 SHA256",
            "==> 安裝到",
            "已更新到 v999.0.0",
        ] {
            assert!(out.contains(want), "缺少「{want}」：{out}");
        }
        assert_eq!(std::fs::read(&bin).unwrap(), NEW_AGT, "agt 應被換成新版");
        assert!(!out.contains("daemon 已用新版重新啟動"), "{out}");
    }

    /// daemon 原本在執行：更新時先停止，換好檔案後自動重新啟動（新的行程）。
    #[tokio::test(flavor = "multi_thread")]
    async fn update_restarts_running_daemon() {
        let mut env = Env::new();
        let Connected { client: c, .. } = env.connect("cli").await;
        let old_pid = env.daemon.as_ref().unwrap().id();
        let bin = install_copy(&env, "restart");
        // 新版用真的 agt（才能當 daemon 啟動）
        let payload = std::fs::read(agt_exe()).unwrap();
        let rel = make_release(&env, "real", "v999.0.0", "v999.0.0", &payload, None);

        let (code, out, err) = run_update(&env, &bin, &rel, &["update", "-y"]);
        assert_eq!(code, Some(0), "{out}\n{err}");
        assert!(out.contains("==> 停止 daemon"), "{out}");
        assert!(out.contains("daemon 已用新版重新啟動"), "{out}\n{err}");
        drop(c);

        let Connected { client: c2, .. } = env.connect("cli").await;
        let info = daemon_info(&c2).await;
        assert_ne!(info.pid, old_pid, "應該是新啟動的 daemon");
        // 這個 daemon 不是測試啟動的：用協定要求它結束
        env.daemon = None;
        env.stop(&c2).await;
    }
}

/// `agt log`：匯出終端機紀錄（去掉控制序列、tail、Markdown、寫檔與覆寫保護、--raw 限制）。
#[tokio::test(flavor = "multi_thread")]
async fn log_export() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    let s = expect_session(
        c.request(create("fake", &env.repo, "log task"))
            .await
            .unwrap(),
    )
    .unwrap();
    let id = s.short_id.clone();

    // 等 Agent 印出第一行（紀錄檔由 daemon 寫入）
    let mut out = String::new();
    for _ in 0..50 {
        let (code, o, _) = agt_raw(&env, &["log", &id]);
        if code == Some(0) && o.contains("中文輸出") {
            out = o;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        out.contains("agent in") && out.contains("中文輸出"),
        "{out:?}"
    );
    assert!(!out.contains('\x1b'), "控制序列應被去掉：{out:?}");

    // --tail
    let (code, o, _) = agt_raw(&env, &["log", &id, "--tail", "1"]);
    assert_eq!(code, Some(0));
    assert_eq!(o.lines().count(), 1, "{o:?}");

    // Markdown
    let (code, o, err) = agt_raw(&env, &["log", &id, "--format", "md"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(o.starts_with("# agt 分頁紀錄：log task\n"), "{o}");
    assert!(
        o.contains(&format!("- 分頁：{id}")) && o.contains("```text"),
        "{o}"
    );

    // 寫到檔案：已存在時拒絕，--force 才覆寫
    let file = env.home.join("exported.txt");
    let f = file.to_str().unwrap();
    let (code, _, err) = agt_raw(&env, &["log", &id, "-o", f]);
    assert_eq!(code, Some(0), "{err}");
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(written.contains("中文輸出"));
    let (code, _, err) = agt_raw(&env, &["log", &id, "-o", f]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("--force"), "{err}");
    let (code, _, err) = agt_raw(&env, &["log", &id, "-o", f, "--force"]);
    assert_eq!(code, Some(0), "{err}");

    // --raw 不能直接輸出到終端機；搭配 -o 時是原始位元組
    let (code, o, err) = agt_raw(&env, &["log", &id, "--raw"]);
    assert_eq!(code, Some(1));
    assert!(o.is_empty() && err.contains("-o"), "{o:?} {err}");
    let raw = env.home.join("raw.log");
    let (code, _, err) = agt_raw(&env, &["log", &id, "--raw", "-o", raw.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{err}");
    assert!(std::fs::read(&raw).unwrap().len() >= written.len());

    // 不存在的分頁
    let (code, _, _) = agt_raw(&env, &["log", "zzzzzzzz"]);
    assert_eq!(code, Some(1));

    env.stop(&c).await;
}

/// `agt stats`：從資料庫的狀態事件統計時間。
#[tokio::test(flavor = "multi_thread")]
async fn stats_json_reports_sessions() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    let s = expect_session(
        c.request(create("fake", &env.repo, "stats task"))
            .await
            .unwrap(),
    )
    .unwrap();
    // 等狀態事件寫進資料庫（假 Agent 印出 (y/n) 後進入等待輸入）
    let mut v = serde_json::Value::Null;
    for _ in 0..50 {
        let (code, o, _) = agt_raw(&env, &["stats", "--json"]);
        assert_eq!(code, Some(0));
        v = serde_json::from_str(&o).unwrap();
        if v["sessions"][0]["by_status"]["waiting"].is_number() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(v["days"], 7);
    let list = v["sessions"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{v}");
    assert_eq!(list[0]["short_id"], s.short_id.as_str());
    assert_eq!(list[0]["agent"], "fake");
    assert!(list[0]["total_secs"].as_i64().unwrap() >= 0);
    assert_eq!(v["by_agent"]["fake"]["sessions"], 1);

    // 文字模式
    let (code, out, _) = agt_raw(&env, &["stats"]);
    assert_eq!(code, Some(0));
    assert!(
        out.contains(&s.short_id) && out.contains("依 Agent 合計"),
        "{out}"
    );

    env.stop(&c).await;
}

fn ls_json(env: &Env) -> Vec<serde_json::Value> {
    let (code, v) = agt_json(env, &["ls", "--json"]);
    assert_eq!(code, 0, "{v}");
    v.as_array().unwrap().clone()
}

fn tags_of(v: &serde_json::Value) -> Vec<&str> {
    v["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect()
}

/// 範本：`agt new --template`，沒填的 Agent、任務、標籤用範本的值，填了的優先；錯誤訊息。
#[tokio::test(flavor = "multi_thread")]
async fn new_with_template() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    let repo = env.repo.to_string_lossy().into_owned();

    // 省略 Agent：沿用範本的 Agent、任務與標籤
    let (code, out, err) = agt_raw(&env, &["new", "--template", "fix", "--repo", &repo, "-y"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let list = ls_json(&env);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["agent"], "fake");
    assert_eq!(list[0]["task"], "修正");
    assert_eq!(tags_of(&list[0]), ["bug"]);

    // 自己填的任務優先；--tag 與範本的標籤合併（逗號與重複都可以）
    let (code, out, err) = agt_raw(
        &env,
        &[
            "new",
            "fake",
            "--template",
            "fix",
            "--task",
            "mine",
            "--tag",
            "urgent,ui",
            "--tag",
            "BUG",
            "--repo",
            &repo,
            "-y",
        ],
    );
    assert_eq!(code, Some(0), "{out}\n{err}");
    let list = ls_json(&env);
    let mine = list.iter().find(|s| s["task"] == "mine").expect("mine");
    assert_eq!(mine["agent"], "fake");
    assert_eq!(
        tags_of(mine),
        ["bug", "urgent", "ui"],
        "重複的 BUG 不分大小寫去掉"
    );

    // 不存在的範本：列出可用的
    let (code, _, err) = agt_raw(
        &env,
        &["new", "fake", "--template", "nope", "--repo", &repo, "-y"],
    );
    assert_eq!(code, Some(1));
    assert!(err.contains("找不到範本") && err.contains("fix"), "{err}");

    // 沒有 Agent 也沒有範本
    let (code, _, err) = agt_raw(&env, &["new", "--repo", &repo, "-y"]);
    assert_eq!(code, Some(1));
    assert!(err.contains("--template"), "{err}");

    // 標籤不合法：太長
    let long = "x".repeat(25);
    let (code, _, err) = agt_raw(
        &env,
        &["new", "fake", "--tag", &long, "--repo", &repo, "-y"],
    );
    assert_eq!(code, Some(1));
    assert!(err.contains("太長"), "{err}");
    assert_eq!(ls_json(&env).len(), 2, "失敗的建立不應留下分頁");

    // `config check` 列出範本
    let (code, v) = agt_json(&env, &["config", "check", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["templates"], serde_json::json!(["fix"]));
    let (_, out, _) = agt_raw(&env, &["config", "check"]);
    assert!(out.contains("範本：fix"), "{out}");

    env.stop(&c).await;
}

/// 標籤：`agt tag` 的各種修改、`ls --tag` 篩選、文字輸出，重啟 daemon 後仍在。
#[tokio::test(flavor = "multi_thread")]
async fn session_tags() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("cli").await;
    let a = expect_session(c.request(create("fake", &env.repo, "alpha")).await.unwrap()).unwrap();
    let b = expect_session(c.request(create("fake", &env.repo, "beta")).await.unwrap()).unwrap();
    let (ia, ib) = (a.short_id.clone(), b.short_id.clone());

    // 沒有標籤時
    let (code, out, _) = agt_raw(&env, &["tag", &ia]);
    assert_eq!(code, Some(0));
    assert!(out.contains("沒有標籤"), "{out}");

    // 加入（逗號、#、重複與大小寫）
    let (code, out, err) = agt_raw(&env, &["tag", &ia, "--add", "bug,#UI", "--add", "ui"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("#bug #UI"), "{out}");
    // 移除不分大小寫
    let (_, out, _) = agt_raw(&env, &["tag", &ia, "--remove", "ui"]);
    assert!(out.contains("#bug") && !out.contains("UI"), "{out}");
    // 取代與清除
    let (_, out, _) = agt_raw(&env, &["tag", &ib, "--set", "x,y"]);
    assert!(out.contains("#x #y"), "{out}");
    let (_, out, _) = agt_raw(&env, &["tag", &ib, "--add", "bug"]);
    assert!(out.contains("#x #y #bug"), "{out}");

    // 篩選與文字輸出
    let ids = |env: &Env, tag: &str| -> Vec<String> {
        let (code, v) = agt_json(env, &["ls", "--json", "--tag", tag]);
        assert_eq!(code, 0, "{v}");
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s["short_id"].as_str().unwrap().to_string())
            .collect()
    };
    let mut both = ids(&env, "BUG");
    both.sort();
    let mut want = vec![ia.clone(), ib.clone()];
    want.sort();
    assert_eq!(both, want);
    assert_eq!(ids(&env, "#x"), [ib.clone()]);
    assert!(ids(&env, "none").is_empty());
    let (_, out, _) = agt_raw(&env, &["ls"]);
    assert!(out.contains("#bug"), "{out}");

    // 不合法：太長、太多、與 --clear 互斥
    let (code, _, err) = agt_raw(&env, &["tag", &ia, "--add", &"x".repeat(25)]);
    assert_eq!(code, Some(1));
    assert!(err.contains("太長"), "{err}");
    let nine = (0..9)
        .map(|i| format!("t{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let (code, _, err) = agt_raw(&env, &["tag", &ia, "--set", &nine]);
    assert_eq!(code, Some(1));
    assert!(err.contains("最多"), "{err}");
    let (code, _, _) = agt_raw(&env, &["tag", &ia, "--add", "x", "--clear"]);
    assert_eq!(code, Some(2), "clap 應拒絕互斥的選項");
    let (code, _, _) = agt_raw(&env, &["tag", "zzzzzzzz", "--add", "x"]);
    assert_eq!(code, Some(1));
    // 失敗的修改不會改動既有標籤
    let (_, out, _) = agt_raw(&env, &["tag", &ia]);
    assert!(out.contains("#bug") && !out.contains("t0"), "{out}");

    // daemon 重啟後標籤還在
    env.stop(&c).await;
    env.start();
    let Connected { client: c2, .. } = env.connect("cli").await;
    let list = ls_json(&env);
    let a_after = list.iter().find(|s| s["short_id"] == ia.as_str()).unwrap();
    assert_eq!(tags_of(a_after), ["bug"]);

    // --clear
    let (_, out, _) = agt_raw(&env, &["tag", &ia, "--clear"]);
    assert!(out.contains("沒有標籤"), "{out}");
    assert!(ids(&env, "bug") == [ib.clone()]);

    env.stop(&c2).await;
}

/// 建立 grp 分頁並等 Agent 寫好分組檔，回傳 (分頁, worktree)。
async fn grouped_session(env: &Env, c: &Client, task: &str) -> (SessionInfo, PathBuf) {
    let s = expect_session(c.request(create("grp", &env.repo, task)).await.unwrap()).unwrap();
    let wt = PathBuf::from(s.worktree.clone().unwrap());
    let start = Instant::now();
    while !wt.join(".agt/groups/two/FILES").exists() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Agent 沒有寫出分組檔"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (s, wt)
}

fn finish(id: &str, action: FinishAction) -> ClientMsg {
    ClientMsg::Finish {
        id: id.into(),
        action,
        force: false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn grouped_merge_makes_one_commit_per_group() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("tui").await;
    let (s, wt) = grouped_session(&env, &c, "分組").await;
    let branch = s.branch.clone().unwrap();

    let reply = c
        .request(finish(
            &s.id,
            FinishAction::Merge {
                strategy: Some(MergeStrategy::Merge),
            },
        ))
        .await
        .unwrap();
    let ReplyBody::Text(out) = reply else {
        panic!("{reply:?}")
    };
    // 結果訊息要讓 UI 看得出分成幾個 commit
    assert!(out.contains("分成 3 組"), "{out}");
    assert!(out.contains("[one] 加入 one"), "{out}");
    assert!(out.contains("[two]") && out.contains("[rest]"), "{out}");

    // base 上：每組一個 merge commit，三組的檔案都在，.agt 不在
    assert_eq!(
        git(&env.repo, &["rev-list", "--count", "--merges", "main"]),
        "3"
    );
    for f in ["one/a.txt", "two.txt", "rest.txt"] {
        assert!(env.repo.join(f).exists(), "{f}");
    }
    assert!(!env.repo.join(".agt").exists());
    let tree = git(&env.repo, &["ls-tree", "-r", "--name-only", "main"]);
    assert!(!tree.contains(".agt"), "{tree}");
    // 暫時的分組 branch 已刪除，任務 branch 還在
    assert!(git(&env.repo, &["branch", "--list", &format!("{branch}-*")]).is_empty());
    assert!(!git(&env.repo, &["branch", "--list", &branch]).is_empty());
    // 已處理的組說明被清掉
    assert!(!wt.join(".agt/groups/one").exists());
    assert!(!wt.join(".agt/groups/two").exists());

    env.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn grouped_merge_squash_is_linear() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("tui").await;
    let (s, _wt) = grouped_session(&env, &c, "分組 squash").await;
    c.request(finish(
        &s.id,
        FinishAction::Merge {
            strategy: Some(MergeStrategy::Squash),
        },
    ))
    .await
    .unwrap();
    assert_eq!(
        git(&env.repo, &["rev-list", "--count", "--merges", "main"]),
        "0"
    );
    assert_eq!(git(&env.repo, &["rev-list", "--count", "main"]), "4"); // init + 3 組
    assert_eq!(
        git(&env.repo, &["log", "--format=%s", "-3", "main"])
            .lines()
            .filter(|l| *l == "加入 one" || l.contains("（two）") || l.contains("（rest）"))
            .count(),
        3
    );
    env.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn grouped_merge_conflict_stops_and_keeps_finished_groups() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("tui").await;
    let (s, _wt) = grouped_session(&env, &c, "分組衝突").await;
    // base 先有了不同內容的 two.txt：組 two 合併時衝突
    std::fs::write(env.repo.join("two.txt"), "base 版本\n").unwrap();
    git(&env.repo, &["add", "."]);
    git(&env.repo, &["commit", "-qm", "base two"]);

    let e = c
        .request(finish(
            &s.id,
            FinishAction::Merge {
                strategy: Some(MergeStrategy::Merge),
            },
        ))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("two"), "{e}");
    // 組 one 已合進去；衝突停在主工作目錄
    assert!(env.repo.join("one/a.txt").exists());
    assert!(!git(&env.repo, &["diff", "--name-only", "--diff-filter=U"]).is_empty());
    // 收尾失敗要記錄在 agtd.log
    let log = std::fs::read_to_string(env.home.join("data/agtd.log")).unwrap_or_default();
    assert!(log.contains(&format!("{}：收尾 merge 失敗", s.id)), "{log}");
    env.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn grouped_pr_without_remote_fails_before_creating_branches() {
    let mut env = Env::new();
    let Connected { client: c, .. } = env.connect("tui").await;
    let (s, _wt) = grouped_session(&env, &c, "分組 pr").await;
    let branch = s.branch.clone().unwrap();
    let e = c
        .request(finish(&s.id, FinishAction::Pr))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("remote"), "{e}");
    // 沒有 remote：不能留下半成品的分組 branch
    assert!(git(&env.repo, &["branch", "--list", &format!("{branch}-*")]).is_empty());
    env.stop(&c).await;
}
