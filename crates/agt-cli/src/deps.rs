//! 環境檢查與安裝：git、Docker Desktop、Docker Sandboxes（sbx）、主機上的 Agent。
//!
//! 偵測結果（[`crate::setup::Env`]）→ [`plan`] 產生要處理的項目與可用的安裝方式（純函式，可測試）
//! → [`run_menu`] 讓使用者勾選、選擇安裝方式並執行。
//!
//! 安全原則：每一項都先顯示完整指令再由使用者選擇；不在背景安裝、不加 `--silent`；
//! 指令一律用 `Command::new().args()` 組成，不經過 shell；只開啟寫死的官方網址；
//! agt 本身不要求系統管理員權限，需要時由 winget 或安裝程式跳出 UAC。

use crate::prompt::{self, Item};
use crate::setup::{Docker, Env, Sbx, capture, detect};
use crossterm::style::Stylize;
use std::process::Command;
use std::time::{Duration, Instant};

/// 使用者選擇先結束（例如安裝程式要求重新開機）。訊息已經顯示過。
#[derive(Debug)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("已結束；完成後重新執行 agt 會從這裡繼續")
    }
}
impl std::error::Error for Stopped {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// 缺少時不能開啟 agt。
    Required,
    Recommended,
    Optional,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Required => "必要",
            Level::Recommended => "建議",
            Level::Optional => "選用",
        }
    }
}

/// 檢查項目，宣告順序就是處理順序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dep {
    Git,
    DockerDesktop,
    Sbx,
    Claude,
    Copilot,
    Codex,
    Agy,
}

/// 項目的固定資料。
pub struct Requirement {
    pub dep: Dep,
    pub name: &'static str,
    pub level: Level,
    /// winget 套件 ID（只在 Windows 使用）。
    pub winget: Option<&'static str>,
    /// 官方下載頁 / 說明文件。
    pub url: &'static str,
}

pub const REQUIREMENTS: &[Requirement] = &[
    Requirement {
        dep: Dep::Git,
        name: "Git",
        level: Level::Required,
        winget: Some("Git.Git"),
        url: "https://git-scm.com/downloads",
    },
    Requirement {
        dep: Dep::DockerDesktop,
        name: "Docker Desktop",
        level: Level::Optional,
        winget: Some("Docker.DockerDesktop"),
        url: "https://www.docker.com/products/docker-desktop/",
    },
    Requirement {
        dep: Dep::Sbx,
        name: "Docker Sandboxes（sbx）",
        level: Level::Recommended,
        winget: Some("Docker.sbx"),
        url: "https://docs.docker.com/ai/sandboxes/",
    },
    Requirement {
        dep: Dep::Claude,
        name: "Claude Code（主機）",
        level: Level::Optional,
        winget: Some("Anthropic.ClaudeCode"),
        url: "https://code.claude.com/docs/en/setup",
    },
    Requirement {
        dep: Dep::Copilot,
        name: "GitHub Copilot CLI",
        level: Level::Optional,
        // 需要 npm，只顯示指令
        winget: None,
        url: "https://github.com/github/copilot-cli",
    },
    Requirement {
        dep: Dep::Codex,
        name: "OpenAI Codex CLI",
        level: Level::Optional,
        // 需要 npm，只顯示指令
        winget: None,
        url: "https://github.com/openai/codex",
    },
    Requirement {
        dep: Dep::Agy,
        name: "Antigravity CLI（agy）",
        level: Level::Optional,
        winget: None,
        url: "https://antigravity.google/",
    },
];

pub fn requirement(dep: Dep) -> &'static Requirement {
    REQUIREMENTS
        .iter()
        .find(|r| r.dep == dep)
        .expect("REQUIREMENTS 涵蓋所有 Dep")
}

/// 自行安裝時顯示的指令或說明。
pub fn manual(dep: Dep, os: Os) -> &'static str {
    match (dep, os) {
        (Dep::Git, Os::Windows) => "winget install -e --id Git.Git --source winget",
        (Dep::Git, Os::Mac) => "xcode-select --install（或 brew install git）",
        (Dep::Git, Os::Linux) => "sudo apt install git（Fedora：sudo dnf install git）",
        (Dep::DockerDesktop, Os::Windows) => {
            "winget install -e --id Docker.DockerDesktop --source winget"
        }
        (Dep::DockerDesktop, Os::Mac) => "brew install --cask docker",
        (Dep::DockerDesktop, Os::Linux) => {
            "依照 https://docs.docker.com/desktop/setup/install/linux/ 安裝 Docker Desktop 或 Docker Engine"
        }
        (Dep::Sbx, Os::Windows) => "winget install -e --id Docker.sbx --source winget",
        (Dep::Sbx, _) => "依照 https://docs.docker.com/ai/sandboxes/ 安裝",
        (Dep::Claude, Os::Windows) => "irm https://claude.ai/install.ps1 | iex（PowerShell）",
        (Dep::Claude, _) => "curl -fsSL https://claude.ai/install.sh | bash",
        (Dep::Copilot, _) => "npm install -g @github/copilot",
        (Dep::Codex, _) => "npm install -g @openai/codex",
        (Dep::Agy, _) => "依照 https://antigravity.google/ 安裝",
    }
}

/// 項目目前的問題。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    Missing,
    /// 版本太舊（目前的版本）。
    Outdated(String),
    /// Docker Desktop 已安裝但沒有在執行。
    NotRunning,
    /// sbx 已安裝但沒有登入。
    NotSignedIn,
}

/// 處理方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// `winget install -e --id <ID> --source winget`
    Winget(&'static str),
    /// 開啟官方網址。
    Open(&'static str),
    /// 顯示指令，自行安裝。
    Manual(&'static str),
    /// 啟動 Docker Desktop 並等待 engine。
    StartDocker,
    /// 在同一個視窗執行 `sbx login`。
    SbxLogin,
    Skip,
}

impl Method {
    pub fn label(&self) -> &'static str {
        match self {
            Method::Winget(_) => "用 winget 安裝",
            Method::Open(_) => "開啟下載頁",
            Method::Manual(_) => "顯示指令自行安裝",
            Method::StartDocker => "啟動 Docker Desktop",
            Method::SbxLogin => "執行 sbx login",
            Method::Skip => "略過",
        }
    }

    pub fn desc(&self) -> String {
        match self {
            Method::Winget(id) => winget_args(id).join(" "),
            Method::Open(url) => url.to_string(),
            Method::Manual(cmd) => cmd.to_string(),
            Method::StartDocker => "啟動後等待 Docker engine 就緒（最多約 90 秒）".into(),
            Method::SbxLogin => "用瀏覽器登入 Docker 帳號".into(),
            Method::Skip => String::new(),
        }
    }
}

/// 一個要處理的項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub dep: Dep,
    pub problem: Problem,
    /// 預設勾選（只有必要項目）。
    pub default_on: bool,
    /// 第一個是建議的方式；最後一個一定是略過。
    pub methods: Vec<Method>,
}

impl Task {
    pub fn title(&self) -> String {
        let r = requirement(self.dep);
        let what = match &self.problem {
            Problem::Missing => "未安裝".to_string(),
            Problem::Outdated(v) => format!("版本 {v} 太舊，需要 2.20 以上"),
            Problem::NotRunning => "沒有在執行".into(),
            Problem::NotSignedIn => "尚未登入".into(),
        };
        format!("{}：{what}", r.name)
    }
}

pub fn winget_args(id: &str) -> Vec<&str> {
    vec!["winget", "install", "-e", "--id", id, "--source", "winget"]
}

/// git 版本是否至少 2.20（建立 worktree 需要）。無法解析時當作可用。
pub fn git_ok(version: &str) -> bool {
    let mut it = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u32>().unwrap_or(0));
    match (it.next(), it.next()) {
        (Some(major), Some(minor)) => (major, minor) >= (2, 20),
        _ => true,
    }
}

/// 安裝類的處理方式：Windows 有 winget 時優先用 winget，其次開啟下載頁、自行安裝。
fn install_methods(r: &Requirement, os: Os, winget: bool) -> Vec<Method> {
    let mut m = vec![];
    if os == Os::Windows
        && winget
        && let Some(id) = r.winget
    {
        m.push(Method::Winget(id));
    }
    m.push(Method::Open(r.url));
    m.push(Method::Manual(manual(r.dep, os)));
    m.push(Method::Skip);
    m
}

/// 依偵測結果列出要處理的項目（依處理順序）。
pub fn plan(env: &Env, os: Os) -> Vec<Task> {
    let mut tasks = vec![];
    let mut add = |dep: Dep, problem: Problem| {
        let r = requirement(dep);
        let methods = match problem {
            Problem::Missing | Problem::Outdated(_) => install_methods(r, os, env.winget),
            Problem::NotRunning => {
                let mut m = vec![];
                if os != Os::Linux {
                    m.push(Method::StartDocker);
                }
                m.push(Method::Manual("啟動 Docker Desktop（或 Docker Engine）"));
                m.push(Method::Skip);
                m
            }
            Problem::NotSignedIn => vec![Method::SbxLogin, Method::Skip],
        };
        tasks.push(Task {
            dep,
            problem,
            default_on: r.level == Level::Required,
            methods,
        });
    };
    match &env.git {
        None => add(Dep::Git, Problem::Missing),
        Some(v) if !git_ok(v) => add(Dep::Git, Problem::Outdated(v.clone())),
        Some(_) => {}
    }
    match env.docker {
        Docker::Missing => add(Dep::DockerDesktop, Problem::Missing),
        Docker::NotRunning => add(Dep::DockerDesktop, Problem::NotRunning),
        Docker::Running(_) => {}
    }
    match env.sbx {
        Sbx::Missing => add(Dep::Sbx, Problem::Missing),
        Sbx::NotSignedIn(_) => add(Dep::Sbx, Problem::NotSignedIn),
        Sbx::Ready(..) => {}
    }
    if !env.claude {
        add(Dep::Claude, Problem::Missing);
    }
    if !env.copilot {
        add(Dep::Copilot, Problem::Missing);
    }
    if !env.codex {
        add(Dep::Codex, Problem::Missing);
    }
    if !env.agy {
        add(Dep::Agy, Problem::Missing);
    }
    tasks
}

/// 不詢問時（`agt setup --yes`）要自動執行的處理：沒有 `--install` 時一律不安裝；
/// 有 `--install` 時只處理預設勾選的項目，而且只用 winget（不會開瀏覽器）。
pub fn auto_methods(tasks: &[Task], install: bool) -> Vec<(Dep, Method)> {
    if !install {
        return vec![];
    }
    tasks
        .iter()
        .filter(|t| t.default_on)
        .filter_map(|t| match t.methods.first() {
            Some(m @ Method::Winget(_)) => Some((t.dep, m.clone())),
            _ => None,
        })
        .collect()
}

/// 互動式安裝選單。回傳處理後重新偵測的環境。
pub fn run_menu(env: Env) -> anyhow::Result<Env> {
    let os = Os::current();
    let tasks = plan(&env, os);
    if tasks.is_empty() {
        return Ok(env);
    }
    println!();
    let labels: Vec<String> = tasks
        .iter()
        .map(|t| format!("[{}] {}", requirement(t.dep).level.label(), t.title()))
        .collect();
    let items: Vec<Item> = labels
        .iter()
        .map(|l| Item {
            label: l.as_str(),
            desc: "",
        })
        .collect();
    let defaults: Vec<bool> = tasks.iter().map(|t| t.default_on).collect();
    let on = prompt::multi_select("要處理哪些項目？", &items, &defaults)?;
    let mut env = env;
    for (t, _) in tasks.iter().zip(on).filter(|(_, o)| *o) {
        // 前面的步驟可能已經處理掉這一項（例如安裝 Docker Desktop 後一併啟動），
        // 或狀態已經改變（例如 sbx 安裝後變成尚未登入），所以用重新偵測的結果
        let Some(t) = plan(&env, os).into_iter().find(|p| p.dep == t.dep) else {
            continue;
        };
        let t = &t;
        println!("\n{}", t.title().bold());
        if os == Os::Windows && t.dep == Dep::DockerDesktop && env.wsl == Some(false) {
            println!(
                "  {}",
                "這台電腦還沒有 WSL2；Docker Desktop 安裝程式會一併啟用（可能需要重新開機）。"
                    .dark_grey()
            );
        }
        let method = choose(t)?;
        env = apply(t.dep, &method, env)?;
    }
    Ok(env)
}

/// 選擇處理方式。只有一種方式（加上略過）時也顯示選單，讓使用者確認。
fn choose(t: &Task) -> anyhow::Result<Method> {
    // 完整指令先印出來，選單中的說明可能因為視窗太窄被截斷
    for m in &t.methods {
        if let Method::Winget(_) | Method::Manual(_) = m {
            println!("  {}：{}", m.label(), m.desc().cyan());
        }
    }
    let descs: Vec<String> = t.methods.iter().map(Method::desc).collect();
    let items: Vec<Item> = t
        .methods
        .iter()
        .zip(&descs)
        .map(|(m, d)| Item {
            label: m.label(),
            desc: d.as_str(),
        })
        .collect();
    let i = prompt::select("要怎麼處理？", &items, 0)?;
    Ok(t.methods[i].clone())
}

/// 執行一種處理方式，回傳重新偵測的環境。
pub fn apply(dep: Dep, method: &Method, env: Env) -> anyhow::Result<Env> {
    let r = requirement(dep);
    match method {
        Method::Skip => return Ok(env),
        Method::Manual(cmd) => {
            prompt::status('!', &format!("請自行執行：{cmd}"));
            prompt::status('!', &format!("說明：{}", r.url));
            return Ok(env);
        }
        Method::Open(url) => {
            match open_url(url) {
                Ok(()) => prompt::status('✓', &format!("已開啟 {url}")),
                Err(e) => prompt::status('✗', &format!("{e}；請自行開啟 {url}")),
            }
            prompt::status('!', "安裝完成後重新執行 agt setup（或重新開啟 agt）");
            return Ok(env);
        }
        Method::Winget(id) => {
            let args = winget_args(id);
            println!("  {} {}", "執行：".dark_grey(), args.join(" "));
            let ok = Command::new(args[0])
                .args(&args[1..])
                .status()
                .is_ok_and(|s| s.success());
            refresh_path();
            if ok {
                prompt::status('✓', &format!("{} 安裝完成", r.name));
            } else {
                prompt::status(
                    '✗',
                    &format!(
                        "winget 沒有成功完成，請檢查上面的訊息；也可以從 {} 下載",
                        r.url
                    ),
                );
            }
        }
        Method::StartDocker => {
            start_docker();
        }
        Method::SbxLogin => {
            let ok = Command::new("sbx")
                .arg("login")
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                prompt::status('✗', "sbx login 沒有完成，之後可以自行執行 sbx login");
            }
        }
    }
    let env = detect();
    // 安裝後的後續步驟
    match (dep, method) {
        (Dep::DockerDesktop, Method::Winget(_)) => match env.docker {
            Docker::Running(_) => {}
            Docker::NotRunning | Docker::Missing => return after_docker_install(env),
        },
        (Dep::Sbx, Method::Winget(_)) if matches!(env.sbx, Sbx::NotSignedIn(_)) => {
            let i = prompt::select(
                "Docker Sandboxes 需要登入 Docker 帳號，要現在執行 sbx login 嗎？",
                &[
                    Item {
                        label: Method::SbxLogin.label(),
                        desc: "",
                    },
                    Item {
                        label: "稍後再登入",
                        desc: "之後自行執行 sbx login",
                    },
                ],
                0,
            )?;
            if i == 0 {
                return apply(Dep::Sbx, &Method::SbxLogin, env);
            }
        }
        _ => {}
    }
    Ok(env)
}

/// Docker Desktop 安裝後還無法使用：可能需要重新開機（WSL2）或還沒啟動。
fn after_docker_install(env: Env) -> anyhow::Result<Env> {
    let i = prompt::select(
        "Docker Desktop 還無法使用。如果安裝程式要求重新開機，請先重新開機。",
        &[
            Item {
                label: "啟動 Docker Desktop 並等待",
                desc: "不需要重新開機時",
            },
            Item {
                label: "先結束，重新開機後再開啟 agt",
                desc: "重新開機後再執行 agt 會從這裡繼續",
            },
            Item {
                label: "略過",
                desc: "繼續設定其他項目",
            },
        ],
        0,
    )?;
    match i {
        0 => apply(Dep::DockerDesktop, &Method::StartDocker, env),
        1 => {
            println!(
                "\n{}",
                "重新開機後再執行 agt（或雙擊 agt.exe），會重新檢查並從這裡繼續。".bold()
            );
            Err(Stopped.into())
        }
        _ => Ok(env),
    }
}

/// 啟動 Docker Desktop，等待 engine 就緒。
fn start_docker() {
    let started = match Os::current() {
        Os::Windows => {
            let exe = std::env::var_os("ProgramFiles")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| "C:\\Program Files".into())
                .join("Docker\\Docker\\Docker Desktop.exe");
            agt_proto::transport::spawn_detached(&exe, &[]).is_ok()
        }
        Os::Mac => Command::new("open")
            .args(["-a", "Docker"])
            .status()
            .is_ok_and(|s| s.success()),
        Os::Linux => false,
    };
    if !started {
        prompt::status('✗', "無法啟動 Docker Desktop，請自行啟動");
        return;
    }
    print!("  等待 Docker engine 就緒");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    if wait_docker(Duration::from_secs(90)) {
        println!();
        prompt::status('✓', "Docker 已在執行");
    } else {
        println!();
        prompt::status(
            '!',
            "Docker 還沒有就緒（第一次啟動可能要接受授權條款），就緒後重新執行 agt setup",
        );
    }
}

/// 輪詢 `docker version`，engine 就緒時回傳 true。
pub fn wait_docker(timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if capture("docker", &["version", "--format", "{{.Server.Version}}"])
            .is_some_and(|(ok, v)| ok && !v.is_empty())
        {
            return true;
        }
        print!(".");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::thread::sleep(Duration::from_secs(3));
    }
    false
}

/// 開啟寫死的官方網址（不接受外部輸入）。
fn open_url(url: &'static str) -> Result<(), String> {
    debug_assert!(url.starts_with("https://"));
    let mut cmd = match Os::current() {
        // explorer.exe 會用預設瀏覽器開啟網址；不經過 cmd /c start
        Os::Windows => Command::new("explorer.exe"),
        Os::Mac => Command::new("open"),
        Os::Linux => Command::new("xdg-open"),
    };
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("無法開啟瀏覽器：{e}"))
}

/// 安裝程式只會修改登錄檔中的 PATH，目前的行程看不到。
/// 從登錄檔讀出系統與使用者的 PATH，把沒有的目錄加到目前行程的 PATH 後面。
pub fn refresh_path() {
    #[cfg(windows)]
    {
        let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        for value in [win::system_path(), win::user_path()].into_iter().flatten() {
            for d in std::env::split_paths(&value) {
                if !d.as_os_str().is_empty()
                    && !dirs
                        .iter()
                        .any(|x| x.as_os_str().eq_ignore_ascii_case(d.as_os_str()))
                {
                    dirs.push(d);
                }
            }
        }
        if let Ok(p) = std::env::join_paths(dirs) {
            // SAFETY: 安裝選單在單一執行緒中執行，這時沒有其他執行緒讀寫環境變數
            unsafe { std::env::set_var("PATH", p) };
        }
    }
}

/// 是否從檔案總管雙擊開啟（console 裡只有 agt 一個行程）。
/// 這時程式結束視窗就會關閉，出錯或離開前要先等使用者按鍵。
pub fn launched_by_double_click() -> bool {
    #[cfg(windows)]
    {
        win::console_process_count() == 1
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    type Hkey = *mut c_void;
    const HKEY_CURRENT_USER: Hkey = 0x8000_0001u32 as i32 as isize as Hkey;
    const HKEY_LOCAL_MACHINE: Hkey = 0x8000_0002u32 as i32 as isize as Hkey;
    /// REG_SZ；REG_EXPAND_SZ 也會被接受並展開 %VAR%（沒有 RRF_NOEXPAND）。
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            hkey: Hkey,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            len: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn reg_string(root: Hkey, sub_key: &str, value: &str) -> Option<OsString> {
        let (sub, val) = (wide(sub_key), wide(value));
        let mut len: u32 = 0;
        // SAFETY: 第一次只查詢大小（data 為 NULL）；第二次的緩衝區大小就是查到的 bytes 數，
        // 值在兩次之間變大時 RegGetValueW 會回傳 ERROR_MORE_DATA 而不是寫出界，這時再試一次。
        unsafe {
            for _ in 0..3 {
                let rc = RegGetValueW(
                    root,
                    sub.as_ptr(),
                    val.as_ptr(),
                    RRF_RT_REG_SZ,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut len,
                );
                if rc != 0 || len == 0 {
                    return None;
                }
                let mut buf = vec![0u16; (len as usize).div_ceil(2)];
                let mut got = (buf.len() * 2) as u32;
                let rc = RegGetValueW(
                    root,
                    sub.as_ptr(),
                    val.as_ptr(),
                    RRF_RT_REG_SZ,
                    std::ptr::null_mut(),
                    buf.as_mut_ptr().cast(),
                    &mut got,
                );
                if rc == 0 {
                    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                    return Some(OsString::from_wide(&buf[..n]));
                }
                const ERROR_MORE_DATA: i32 = 234;
                if rc != ERROR_MORE_DATA {
                    return None;
                }
                len = got;
            }
        }
        None
    }

    pub fn system_path() -> Option<OsString> {
        reg_string(
            HKEY_LOCAL_MACHINE,
            "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment",
            "Path",
        )
    }

    pub fn user_path() -> Option<OsString> {
        reg_string(HKEY_CURRENT_USER, "Environment", "Path")
    }

    pub fn console_process_count() -> u32 {
        let mut list = [0u32; 4];
        // SAFETY: 緩衝區長度與傳入的 count 一致；行程數超過時只回傳數量、不寫出界
        unsafe { GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env {
            git: Some("2.44.0.windows.1".into()),
            claude: true,
            agy: true,
            copilot: true,
            codex: true,
            docker: Docker::Running("29".into()),
            sbx: Sbx::Ready("v0.53.0".into(), true),
            wsl: Some(true),
            winget: true,
        }
    }

    #[test]
    fn nothing_to_do_when_all_present() {
        assert!(plan(&env(), Os::Windows).is_empty());
    }

    #[test]
    fn order_and_defaults() {
        let e = Env {
            git: None,
            claude: false,
            agy: false,
            copilot: false,
            codex: false,
            docker: Docker::Missing,
            sbx: Sbx::Missing,
            ..env()
        };
        let t = plan(&e, Os::Windows);
        let deps: Vec<Dep> = t.iter().map(|t| t.dep).collect();
        assert_eq!(
            deps,
            [
                Dep::Git,
                Dep::DockerDesktop,
                Dep::Sbx,
                Dep::Claude,
                Dep::Copilot,
                Dep::Codex,
                Dep::Agy
            ]
        );
        // 只有必要項目預設勾選
        let on: Vec<Dep> = t.iter().filter(|t| t.default_on).map(|t| t.dep).collect();
        assert_eq!(on, [Dep::Git]);
        // 每一項最後都可以略過
        assert!(t.iter().all(|t| t.methods.last() == Some(&Method::Skip)));
    }

    #[test]
    fn winget_only_on_windows_with_winget() {
        let e = Env { git: None, ..env() };
        let first = |os, winget| {
            plan(
                &Env {
                    winget,
                    ..e.clone()
                },
                os,
            )[0]
            .methods
            .first()
            .cloned()
            .unwrap()
        };
        assert_eq!(first(Os::Windows, true), Method::Winget("Git.Git"));
        assert_eq!(
            first(Os::Windows, false),
            Method::Open("https://git-scm.com/downloads")
        );
        assert!(matches!(first(Os::Linux, true), Method::Open(_)));
        assert!(matches!(first(Os::Mac, false), Method::Open(_)));
        // Copilot 沒有 winget ID：只有開啟網頁與顯示指令
        let c = plan(
            &Env {
                copilot: false,
                ..env()
            },
            Os::Windows,
        );
        assert!(!c[0].methods.iter().any(|m| matches!(m, Method::Winget(_))));
        assert!(
            c[0].methods
                .contains(&Method::Manual("npm install -g @github/copilot"))
        );
    }

    #[test]
    fn not_running_and_not_signed_in() {
        let e = Env {
            docker: Docker::NotRunning,
            sbx: Sbx::NotSignedIn("v0.53.0".into()),
            ..env()
        };
        let t = plan(&e, Os::Windows);
        assert_eq!(t[0].problem, Problem::NotRunning);
        assert_eq!(t[0].methods[0], Method::StartDocker);
        assert_eq!(t[1].methods, [Method::SbxLogin, Method::Skip]);
        // Linux 上不自動啟動 Docker
        let l = plan(&e, Os::Linux);
        assert!(!l[0].methods.contains(&Method::StartDocker));
    }

    #[test]
    fn old_git_is_outdated() {
        assert!(git_ok("2.20.0") && git_ok("2.44.0.windows.1") && git_ok("3.0"));
        assert!(!git_ok("2.19.1") && !git_ok("1.9"));
        assert!(git_ok("?"));
        let t = plan(
            &Env {
                git: Some("2.17.1".into()),
                ..env()
            },
            Os::Windows,
        );
        assert_eq!(t[0].problem, Problem::Outdated("2.17.1".into()));
        assert!(t[0].default_on);
    }

    #[test]
    fn yes_never_installs_without_install_flag() {
        let e = Env {
            git: None,
            sbx: Sbx::Missing,
            ..env()
        };
        let t = plan(&e, Os::Windows);
        assert!(auto_methods(&t, false).is_empty());
        // --install：只處理預設勾選（必要）的項目，只用 winget
        assert_eq!(
            auto_methods(&t, true),
            [(Dep::Git, Method::Winget("Git.Git"))]
        );
        // 沒有 winget 時什麼都不做
        let t = plan(&Env { winget: false, ..e }, Os::Windows);
        assert!(auto_methods(&t, true).is_empty());
    }

    #[test]
    fn every_dep_has_requirement_and_manual() {
        for r in REQUIREMENTS {
            assert!(r.url.starts_with("https://"));
            for os in [Os::Windows, Os::Mac, Os::Linux] {
                assert!(!manual(r.dep, os).is_empty());
            }
        }
    }
}
