//! 平台差異（規格 11.1）。其他模組不寫 `#[cfg(windows)]`。

use crate::adapter::CommandSpec;
use std::path::{Path, PathBuf};

/// setup 失敗時包裝指令使用的 exit code，Session 看到它就進入 Error 狀態。
pub const SETUP_FAILED_EXIT: u32 = 97;

/// 在 PATH 中尋找執行檔（Windows 會依 PATHEXT 補副檔名）。
pub fn which(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.exists().then(|| p.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .map(|s| s.to_ascii_lowercase())
            .chain(std::iter::once(String::new()))
            .collect()
    } else {
        vec![String::new()]
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 把要交給 PTY 啟動的程式解析成 (執行檔, 前置參數)。
///
/// Windows 的 CreateProcessW 不會套用 PATHEXT，而 npm 等安裝方式會在同一目錄放一個
/// 沒有副檔名的 shell 腳本（例如 `npm\claude`），直接啟動會得到 os error 193。
/// 這裡先用 [`which`] 找到真正的檔案；`.cmd` / `.bat` 再交給 `cmd.exe /d /c` 執行。
pub fn resolve_program(program: &str) -> (String, Vec<String>) {
    if !cfg!(windows) {
        return (program.into(), Vec::new());
    }
    let Some(path) = which(program) else {
        return (program.into(), Vec::new());
    };
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let full = path.to_string_lossy().into_owned();
    match ext.as_deref() {
        Some("cmd") | Some("bat") => {
            let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
            (comspec, vec!["/d".into(), "/c".into(), full])
        }
        _ => (full, Vec::new()),
    }
}

/// 使用者預設 shell：Windows 為 `pwsh`，沒有則 `powershell`；Unix 為 `$SHELL`。
pub fn default_shell() -> String {
    if cfg!(windows) {
        if which("pwsh").is_some() {
            "pwsh".into()
        } else {
            "powershell".into()
        }
    } else {
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into())
    }
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 把 setup 指令包在 Agent 前面執行，輸出會顯示在同一個分頁中（規格 7.1 第 5 步）。
/// setup 失敗時以 [`SETUP_FAILED_EXIT`] 結束，不啟動 Agent。
pub fn wrap_with_setup(setup: &str, spec: CommandSpec) -> CommandSpec {
    if cfg!(windows) {
        let shell = if which("pwsh").is_some() {
            "pwsh"
        } else {
            "powershell"
        };
        let mut agent = format!("& {}", ps_quote(&spec.program));
        for a in &spec.args {
            agent.push(' ');
            agent.push_str(&ps_quote(a));
        }
        let script = format!(
            "{setup}\nif (-not $?) {{ Write-Host ''; Write-Host '[agt] setup 失敗'; exit {SETUP_FAILED_EXIT} }}\n{agent}\nexit $LASTEXITCODE"
        );
        CommandSpec {
            program: shell.into(),
            args: vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-Command".into(),
                script,
            ],
            env: spec.env,
            cwd: spec.cwd,
            cleanup: spec.cleanup,
        }
    } else {
        let script = format!(
            "{setup}\nrc=$?\nif [ $rc -ne 0 ]; then echo; echo \"[agt] setup 失敗（exit $rc）\"; exit {SETUP_FAILED_EXIT}; fi\nexec \"$0\" \"$@\""
        );
        let mut args = vec!["-c".into(), script, spec.program];
        args.extend(spec.args);
        CommandSpec {
            program: "/bin/sh".into(),
            args,
            env: spec.env,
            cwd: spec.cwd,
            cleanup: spec.cleanup,
        }
    }
}

/// 執行收尾指令（見 [`CommandSpec::cleanup`]），等它結束；失敗只記錄、不回報。
pub fn run_cleanup(cmd: &[String]) {
    // 啟動前執行時要清的東西通常不存在，失敗是正常的，不記成警告
    run_quiet(cmd, "收尾指令", false);
}

/// 執行 Agent 的更新指令（設定 `update`），等它結束；失敗只記錄、不回報。
pub fn run_update(cmd: &[String]) {
    run_quiet(cmd, "更新指令", true);
}

/// 不開視窗、不接輸入輸出地執行指令並等它結束。`what` 只用於日誌。
fn run_quiet(cmd: &[String], what: &str, warn_on_fail: bool) {
    let Some((first, rest)) = cmd.split_first() else {
        return;
    };
    let (program, pre_args) = resolve_program(first);
    let mut c = std::process::Command::new(program);
    c.args(pre_args)
        .args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    match c.status() {
        Ok(s) if s.success() || !warn_on_fail => tracing::debug!(?cmd, ?s, "{what}已執行"),
        Ok(s) => tracing::warn!(?cmd, ?s, "{what}執行失敗"),
        Err(e) => tracing::warn!(?cmd, "無法執行{what}：{e}"),
    }
}

/// 結束整個行程樹。Windows 用 `taskkill /T`（等同 Job Object 終止行程樹）；
/// Unix 對行程群組送 SIGHUP，5 秒後仍存活則送 SIGKILL。
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(unix)]
    {
        let pgid = pid as libc::pid_t;
        // SAFETY: 單純的系統呼叫；portable-pty 以 setsid 啟動子行程，所以 pid 即群組 id。
        unsafe {
            libc::kill(-pgid, libc::SIGHUP);
        }
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            // SAFETY: 同上；群組不存在時 kill 只會回傳錯誤。
            unsafe {
                if libc::kill(-pgid, 0) == 0 {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
        });
    }
}

/// 送出系統通知（Windows Toast、macOS 通知中心、freedesktop）。失敗時只記錄。
pub fn notify(title: &str, body: &str) {
    let title = title.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        let r = notify_rust::Notification::new()
            .appname("agt")
            .summary(&title)
            .body(&body)
            .timeout(notify_rust::Timeout::Milliseconds(6000))
            .show();
        if let Err(e) = r {
            tracing::debug!("系統通知失敗：{e}");
        }
    });
}

/// 把檔案權限設為只有使用者可讀寫（Unix）。Windows 的使用者目錄預設已限制存取。
pub fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(windows)]
    let _ = path;
}

/// 判斷行程是否仍存在。
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 只檢查行程是否存在。
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

/// Windows 路徑長度上限檢查（規格 7.2）。
pub fn path_too_long(path: &Path) -> bool {
    cfg!(windows) && path.as_os_str().len() > 240
}
