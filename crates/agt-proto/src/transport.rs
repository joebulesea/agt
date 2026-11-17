//! 傳輸層（規格 4.1）：Windows 用 Named Pipe，macOS / Linux 用 Unix Domain Socket，
//! 皆透過 `interprocess` 的 tokio 介面。client 端需要的平台差異也集中在這裡。

use interprocess::local_socket::tokio::{Listener, Stream, prelude::*};
use interprocess::local_socket::{GenericFilePath, ListenerOptions, Name, ToFsName};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use interprocess::local_socket::tokio::Stream as Conn;

/// 環境變數：daemon 的 socket / pipe 路徑（規格 6.4）。
pub const ENV_SOCKET: &str = "AGT_SOCKET";
/// 環境變數：工作階段 UUID（規格 6.4）。
pub const ENV_SESSION: &str = "AGT_SESSION_ID";

/// 預設位置。
pub fn default_endpoint() -> String {
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
        let user: String = user
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!(r"\\.\pipe\agt-{user}")
    }
    #[cfg(target_os = "macos")]
    {
        let uid = unsafe { libc::getuid() };
        let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(tmp)
            .join(format!("agt-{uid}"))
            .join("agt.sock")
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match std::env::var("XDG_RUNTIME_DIR") {
            Ok(d) if !d.is_empty() => PathBuf::from(d).join("agt").join("agt.sock"),
            _ => {
                let uid = unsafe { libc::getuid() };
                PathBuf::from(format!("/tmp/agt-{uid}")).join("agt.sock")
            }
        }
        .to_string_lossy()
        .into_owned()
    }
}

/// 目前要使用的 endpoint：`AGT_SOCKET` 優先。
pub fn endpoint() -> String {
    match std::env::var(ENV_SOCKET) {
        Ok(s) if !s.is_empty() => s,
        _ => default_endpoint(),
    }
}

fn name(ep: &str) -> io::Result<Name<'_>> {
    ep.to_fs_name::<GenericFilePath>()
}

/// 讓讀、寫兩個 task 共用同一條連線（`&Stream` 同時實作 AsyncRead 與 AsyncWrite）。
pub fn shared(s: Stream) -> std::sync::Arc<Stream> {
    std::sync::Arc::new(s)
}

pub use interprocess::local_socket::tokio::Listener as LocalListener;

pub async fn accept(l: &Listener) -> io::Result<Stream> {
    l.accept().await
}

pub async fn connect(ep: &str) -> io::Result<Stream> {
    Stream::connect(name(ep)?).await
}

/// 建立 listener。Unix 會先建立權限 0700 的目錄並移除殘留的 socket 檔；
/// 呼叫前必須先取得 daemon lock，確保不會刪掉別人正在用的 socket。
pub fn listen(ep: &str) -> io::Result<Listener> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = Path::new(ep);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let _ = std::fs::remove_file(p);
    }
    #[cfg(windows)]
    let _ = Path::new(ep);
    ListenerOptions::new().name(name(ep)?).create_tokio()
}

/// 在背景啟動 daemon（規格 4.5）：Windows 用 `DETACHED_PROCESS | CREATE_NO_WINDOW`，Unix 用 `setsid`。
pub fn spawn_detached(exe: &Path, args: &[&str]) -> io::Result<u32> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);

        // Rust 一律以 bInheritHandles=TRUE 建立子程序，父程序可繼承的 std handle
        // （例如 `publish.sh | tail` 的管線）會漏給 daemon，讓管線一直不關閉、外層卡住。
        // 先關掉這三個 handle 的可繼承旗標；之後用 Stdio::inherit() 啟動的子程序不受影響
        // （Rust 會另外複製一份可繼承的 handle 給子程序）。
        unsafe extern "system" {
            fn GetStdHandle(n_std_handle: u32) -> *mut core::ffi::c_void;
            fn SetHandleInformation(h: *mut core::ffi::c_void, mask: u32, flags: u32) -> i32;
        }
        const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
        const STD_INPUT_HANDLE: u32 = -10i32 as u32;
        const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
        const STD_ERROR_HANDLE: u32 = -12i32 as u32;
        for n in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: 只修改本程序自己的 std handle 旗標；沒有 handle（NULL）或
            // INVALID_HANDLE_VALUE 時略過，呼叫失敗也只是維持原狀。
            unsafe {
                let h = GetStdHandle(n);
                if !h.is_null() && h as isize != -1 {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid 是 async-signal-safe，在 fork 後、exec 前呼叫是安全的。
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = cmd.spawn()?;
    Ok(child.id())
}

/// 找到與目前執行檔同目錄的另一個執行檔（例如從 `agt` 找 `agtd`）。
pub fn sibling_exe(name: &str) -> PathBuf {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    if let Ok(me) = std::env::current_exe()
        && let Some(dir) = me.parent()
    {
        let p = dir.join(&file);
        if p.exists() {
            return p;
        }
        // `cargo test` 的執行檔位在 target/<profile>/deps/
        if let Some(up) = dir.parent() {
            let p = up.join(&file);
            if p.exists() {
                return p;
            }
        }
    }
    PathBuf::from(file)
}

/// 等待行程 `pid` 結束，最多等 `timeout`；回傳行程是否已結束（找不到該行程也算已結束）。
///
/// 用來在重新啟動 daemon 前確認舊版 agtd 真的結束了：named pipe 關閉後舊程序還會收尾一段時間，
/// 這段期間它仍持有 lock 檔，Windows 上它的 exe（例如改名後的 `agtd.exe.old`）也還被鎖住。
/// 這是阻塞呼叫，在 async 環境中請用 `spawn_blocking`。
pub fn wait_pid_exit(pid: u32, timeout: std::time::Duration) -> bool {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut core::ffi::c_void;
            fn WaitForSingleObject(h: *mut core::ffi::c_void, ms: u32) -> u32;
            fn CloseHandle(h: *mut core::ffi::c_void) -> i32;
        }
        const SYNCHRONIZE: u32 = 0x0010_0000;
        const WAIT_OBJECT_0: u32 = 0;
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: 只開啟具 SYNCHRONIZE 權限的 handle 等待它被 signal，用完即關閉。
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if h.is_null() {
                // 行程已不存在（或無權限開啟；daemon 與 CLI 同一使用者，不會發生）
                return true;
            }
            let r = WaitForSingleObject(h, ms);
            CloseHandle(h);
            r == WAIT_OBJECT_0
        }
    }
    #[cfg(unix)]
    {
        let start = std::time::Instant::now();
        loop {
            // SAFETY: signal 0 只檢查行程是否存在，不會送出訊號。
            let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
                || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            if !alive {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod wait_pid_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn wait_pid_exit_returns_after_child_exits() {
        #[cfg(unix)]
        let mut child = Command::new("sh")
            .args(["-c", "sleep 0.3"])
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/c", "ping -n 2 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        let pid = child.id();
        // 還在執行：短逾時應回傳 false
        assert!(!wait_pid_exit(pid, Duration::from_millis(50)));
        // 另一個執行緒回收子行程（Unix 上未回收的 zombie 仍算存在）
        let reaper = std::thread::spawn(move || child.wait());
        assert!(wait_pid_exit(pid, Duration::from_secs(10)));
        reaper.join().unwrap().unwrap();
    }
}
