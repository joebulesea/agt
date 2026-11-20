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

/// PowerShell 的單引號字串。PowerShell 除了 `'` 之外也把 `‘ ’ ‚ ‛` 當成單引號，
/// 只跳脫 `'` 的話，參數裡的 `’` 會提早結束字串、讓後面的內容被當成指令執行。
fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            out.push(c); // 重複一次 = 字面上的引號
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// 經由 `cmd.exe /c` 執行（`.cmd` / `.bat`）時，參數裡這些字元會被 cmd 解讀：
/// `& | < >` 串接或導向指令、`%` 展開環境變數、`^` 跳脫、`"` 改變引號範圍。
/// 參數無法安全地跳脫，所以直接拒絕，請改用 `.exe` 或拿掉這些字元。
pub fn check_cmd_args(pre_args: &[String], args: &[String]) -> Result<(), String> {
    if pre_args.is_empty() {
        return Ok(()); // 不經過 cmd.exe
    }
    const UNSAFE: &[char] = &['&', '|', '<', '>', '%', '^', '"', '\n', '\r'];
    match args.iter().find(|a| a.contains(UNSAFE)) {
        Some(a) => Err(format!(
            "這個指令是 .cmd / .bat，要經由 cmd.exe 執行，但參數 {a:?} 含有 cmd 會解讀的字元（& | < > % ^ \" 或換行）。\
             請改用 .exe 版本的指令，或拿掉這些字元。"
        )),
        None => Ok(()),
    }
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

/// 把 Agent 的更新指令（設定 `update`）轉成可放在 [`wrap_with_setup`] 前面的腳本片段：
/// 在分頁中顯示進度，更新失敗只印出訊息、不會讓 setup 判定失敗。
pub fn update_script(cmd: &[String]) -> Option<String> {
    if cmd.is_empty() {
        return None;
    }
    Some(if cfg!(windows) {
        let line = cmd
            .iter()
            .map(|a| ps_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "Write-Host '[agt] 檢查 Agent 更新…'\n& {line}\nif (-not $?) {{ Write-Host '[agt] 更新指令失敗，使用現有版本' }}\n$null = 0"
        )
    } else {
        let line = cmd
            .iter()
            .map(|a| sh_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "echo '[agt] 檢查 Agent 更新…'\n{line} || echo \"[agt] 更新指令失敗（exit $?），使用現有版本\""
        )
    })
}

/// POSIX sh 的單引號字串（`'` 改寫成 `'\''`）。
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
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
    if let Err(e) = check_cmd_args(&pre_args, rest) {
        tracing::warn!(?cmd, "不執行{what}：{e}");
        return;
    }
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

/// Agent 的指令是 `docker` 時，確認 Docker engine 有在執行；沒有的話回傳要顯示給使用者的說明。
/// 其他指令一律回傳 `None`（不檢查）。逾時（engine 卡住）也視為沒有回應。
pub fn docker_unavailable(command: &str) -> Option<String> {
    let name = Path::new(command)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name != "docker" {
        return None;
    }
    let mut c = std::process::Command::new(command);
    c.args(["version", "--format", "{{.Server.Version}}"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(_) => {
            return Some(
                "找不到 docker 指令：尚未安裝 Docker。請先安裝 Docker Desktop（或執行 `agt setup`）。"
                    .into(),
            );
        }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return None,
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    Some(
        "Docker 沒有開啟：連不上 Docker engine。請先啟動 Docker Desktop，等它完全啟動（鯨魚圖示不再轉動）後再試一次。"
            .into(),
    )
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
    // 內容可能來自 Agent（hook 的訊息、分頁名稱）：去掉控制字元、限制長度
    let clip = |s: &str, max: usize| {
        let s = agt_proto::text::printable_line(s);
        match s.char_indices().nth(max) {
            Some((i, _)) => format!("{}…", &s[..i]),
            None => s,
        }
    };
    let title = clip(title, 100);
    let body = clip(body, 300);
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

/// Windows 路徑長度上限檢查（規格 7.2）。
pub fn path_too_long(path: &Path) -> bool {
    cfg!(windows) && path.as_os_str().len() > 240
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_script_quotes_args_and_never_fails() {
        assert!(update_script(&[]).is_none());
        let s = update_script(&["docker".into(), "a b".into(), "it's".into()]).unwrap();
        if cfg!(windows) {
            assert!(s.contains("& 'docker' 'a b' 'it''s'"), "{s}");
        } else {
            assert!(s.contains(r"'docker' 'a b' 'it'\''s' ||"), "{s}");
            // 更新失敗不能讓 setup 判定失敗
            let fail = update_script(&["false".into()]).unwrap();
            let st = std::process::Command::new("/bin/sh")
                .args(["-c", &fail])
                .output()
                .unwrap();
            assert!(st.status.success());
        }
    }

    #[test]
    fn ps_quote_escapes_all_single_quote_forms() {
        assert_eq!(ps_quote("a'b"), "'a''b'");
        // ’ 在 PowerShell 也是單引號：不跳脫的話 `’; calc; ’` 會跳出字串執行 calc
        assert_eq!(ps_quote("x’; calc; ’"), "'x’’; calc; ’’'");
        assert_eq!(ps_quote("‘‚‛"), "'‘‘‚‚‛‛'");
        assert_eq!(ps_quote("中文 ok"), "'中文 ok'");
    }

    #[test]
    fn cmd_args_with_metacharacters_are_refused() {
        let via_cmd = vec!["/d".to_string(), "/c".into(), "x.cmd".into()];
        let ok = vec!["--resume".to_string(), "中文提示，可以有空白".into()];
        assert!(check_cmd_args(&via_cmd, &ok).is_ok());
        for bad in ["a&calc", "x|y", "%PATH%", "a^b", "say \"hi\"", "a\nb"] {
            assert!(
                check_cmd_args(&via_cmd, &[bad.to_string()]).is_err(),
                "{bad}"
            );
        }
        // 不經過 cmd.exe（.exe 或 Unix）：不限制
        assert!(check_cmd_args(&[], &["a&b".to_string()]).is_ok());
    }

    #[test]
    fn docker_check_only_applies_to_docker() {
        assert!(docker_unavailable("claude").is_none());
        assert!(docker_unavailable("").is_none());
        assert!(docker_unavailable("sbx").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn docker_check_reports_when_engine_is_down() {
        // 用 symlink 指向系統的 true / false 當假的 docker（只看 exit code）。
        // 不要在測試裡寫入腳本再執行：其他測試執行緒同時 fork 時，子程序會暫時持有寫入中的 fd，
        // 執行剛寫好的檔案會偶發 ETXTBSY（Text file busy），被當成「找不到 docker」。
        fn sys(name: &str) -> std::path::PathBuf {
            ["/bin", "/usr/bin"]
                .iter()
                .map(|d| Path::new(d).join(name))
                .find(|p| p.is_file())
                .unwrap_or_else(|| panic!("找不到 {name}"))
        }
        let dir = tempfile::tempdir().unwrap();
        let down = dir.path().join("down/docker");
        let up = dir.path().join("up/docker");
        for (link, target) in [(&down, "false"), (&up, "true")] {
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(sys(target), link).unwrap();
        }
        let msg = docker_unavailable(down.to_str().unwrap()).expect("engine 掛掉時要有訊息");
        assert!(msg.contains("Docker 沒有開啟"), "{msg}");
        assert!(docker_unavailable(up.to_str().unwrap()).is_none());
        // 找不到指令
        let missing = dir.path().join("nodir/docker");
        assert!(
            docker_unavailable(missing.to_str().unwrap())
                .unwrap()
                .contains("找不到 docker")
        );
    }
}
