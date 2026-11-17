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

fn denied(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg.into())
}

/// 接受連線。只接受同一位使用者的 client：Unix 檢查對方的 uid；
/// Windows 由 pipe 的 ACL 擋下其他使用者（見 [`listen`]）。
pub async fn accept(l: &Listener) -> io::Result<Stream> {
    let s = l.accept().await?;
    #[cfg(unix)]
    check_peer_uid(&s, "client")?;
    Ok(s)
}

/// 連線到 daemon，並確認對方真的是同一位使用者的 daemon：endpoint 的名稱可以預測，
/// 其他本機使用者可能搶先建立同名的 socket / pipe 來冒充，藉此收到按鍵與畫面內容。
pub async fn connect(ep: &str) -> io::Result<Stream> {
    let s = Stream::connect(name(ep)?).await?;
    #[cfg(unix)]
    check_peer_uid(&s, "daemon")?;
    #[cfg(windows)]
    winsec::check_pipe_owner(&s)?;
    Ok(s)
}

/// Unix：連線另一端的有效 uid 必須和自己相同。
#[cfg(unix)]
fn check_peer_uid(s: &Stream, who: &str) -> io::Result<()> {
    use interprocess::local_socket::traits::StreamCommon;
    let me = unsafe { libc::geteuid() };
    match s.peer_creds()?.euid() {
        Some(uid) if uid == me => Ok(()),
        Some(uid) => Err(denied(format!(
            "拒絕連線：{who} 的使用者（uid {uid}）不是目前的使用者（uid {me}）"
        ))),
        None => Err(denied(format!("拒絕連線：無法確認 {who} 的使用者"))),
    }
}

/// Unix：建立（或確認）只有自己能進入的 socket 目錄。
/// 目錄已存在時必須是自己的真實目錄（不是符號連結），否則可能是別人預先放好的。
#[cfg(unix)]
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(denied(format!(
            "socket 目錄 {} 不是一般目錄（可能是符號連結），拒絕使用",
            dir.display()
        )));
    }
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me {
        return Err(denied(format!(
            "socket 目錄 {} 的擁有者（uid {}）不是目前的使用者，拒絕使用",
            dir.display(),
            meta.uid()
        )));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// 建立 listener，只讓同一位使用者連線。
///
/// - Unix：socket 放在權限 0700、擁有者是自己的目錄裡，socket 本身 0600，並移除殘留的 socket 檔；
///   呼叫前必須先取得 daemon lock，確保不會刪掉別人正在用的 socket。
/// - Windows：pipe 的 ACL 只允許目前的使用者與 SYSTEM；同名的 pipe 已存在時建立會失敗。
pub fn listen(ep: &str) -> io::Result<Listener> {
    let opts = ListenerOptions::new().name(name(ep)?);
    #[cfg(unix)]
    let opts = {
        use interprocess::os::unix::local_socket::ListenerOptionsExt;
        let p = Path::new(ep);
        if let Some(dir) = p.parent() {
            ensure_private_dir(dir)?;
        }
        let _ = std::fs::remove_file(p);
        opts.mode(0o600)
    };
    #[cfg(windows)]
    let opts = {
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        opts.security_descriptor(winsec::owner_only_descriptor()?)
    };
    opts.create_tokio()
}

/// Windows 的存取控制：pipe 的 ACL 與擁有者檢查。
#[cfg(windows)]
mod winsec {
    use super::{Stream, denied};
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::io::{AsHandle, AsRawHandle};

    type Handle = *mut c_void;
    type Psid = *mut c_void;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn GetTokenInformation(
            token: Handle,
            class: u32,
            info: *mut c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
        fn ConvertSidToStringSidW(sid: Psid, out: *mut *mut u16) -> i32;
        fn EqualSid(a: Psid, b: Psid) -> i32;
        fn IsWellKnownSid(sid: Psid, kind: u32) -> i32;
        fn GetSecurityInfo(
            handle: Handle,
            object_type: u32,
            info: u32,
            owner: *mut Psid,
            group: *mut Psid,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn CloseHandle(h: Handle) -> i32;
        fn LocalFree(p: *mut c_void) -> *mut c_void;
    }

    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER: u32 = 1;
    const SE_KERNEL_OBJECT: u32 = 6;
    const OWNER_SECURITY_INFORMATION: u32 = 1;
    const WIN_LOCAL_SYSTEM_SID: u32 = 22;
    const WIN_BUILTIN_ADMINISTRATORS_SID: u32 = 26;

    /// 目前行程的使用者 SID（TOKEN_USER 結構連同 SID 本體，放在對齊的緩衝區裡）。
    /// TOKEN_USER 裡的指標指向同一塊緩衝區內的 SID，所以緩衝區必須在堆積上、位址固定，
    /// 不能跟著這個結構一起被搬動。
    struct UserSid(Box<[u64; 32]>);

    impl UserSid {
        fn current() -> io::Result<UserSid> {
            let mut buf = UserSid(Box::new([0; 32]));
            // SAFETY: token 用完即關閉；緩衝區大小（256 bytes）足以容納 TOKEN_USER 與最長的 SID，
            // 不夠時 GetTokenInformation 會失敗而不是寫出界。
            unsafe {
                let mut token: Handle = std::ptr::null_mut();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut len = 0u32;
                let ok = GetTokenInformation(
                    token,
                    TOKEN_USER,
                    buf.0.as_mut_ptr().cast(),
                    std::mem::size_of_val(&*buf.0) as u32,
                    &mut len,
                );
                let err = io::Error::last_os_error();
                CloseHandle(token);
                if ok == 0 {
                    return Err(err);
                }
            }
            Ok(buf)
        }

        /// TOKEN_USER 的第一個欄位就是指向 SID 的指標。
        fn psid(&self) -> Psid {
            // SAFETY: current() 成功後緩衝區開頭是有效的 TOKEN_USER。
            unsafe { *(self.0.as_ptr() as *const Psid) }
        }

        fn to_sddl(&self) -> io::Result<Vec<u16>> {
            // SAFETY: 成功時 out 指向以 NUL 結尾、由 LocalAlloc 配置的字串，複製後釋放。
            unsafe {
                let mut out: *mut u16 = std::ptr::null_mut();
                if ConvertSidToStringSidW(self.psid(), &mut out) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut n = 0;
                while *out.add(n) != 0 {
                    n += 1;
                }
                let s = std::slice::from_raw_parts(out, n).to_vec();
                LocalFree(out.cast());
                Ok(s)
            }
        }
    }

    /// 只允許目前的使用者與 SYSTEM 存取的安全描述元（`P` = 不繼承其他 ACE）。
    pub fn owner_only_descriptor() -> io::Result<SecurityDescriptor> {
        let sid = UserSid::current()?.to_sddl()?;
        let mut sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;".encode_utf16().collect();
        sddl.extend_from_slice(&sid);
        sddl.extend(")".encode_utf16());
        sddl.push(0);
        let sddl = widestring::U16CStr::from_slice(&sddl)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        SecurityDescriptor::deserialize(sddl)
    }

    /// client 端：pipe 的擁有者必須是目前的使用者。
    /// daemon 以系統管理員身分（提升權限）執行時，擁有者會是 Administrators 群組，也接受；SYSTEM 亦同。
    pub fn check_pipe_owner(s: &Stream) -> io::Result<()> {
        #[allow(unreachable_patterns)]
        let handle = match s {
            Stream::NamedPipe(p) => p.as_handle().as_raw_handle(),
            _ => return Ok(()),
        };
        let me = UserSid::current()?;
        // SAFETY: handle 在 s 存活期間有效；descriptor 由系統配置，用完以 LocalFree 釋放，
        // owner 指向 descriptor 內部，只在釋放前使用。
        unsafe {
            let mut owner: Psid = std::ptr::null_mut();
            let mut descriptor: *mut c_void = std::ptr::null_mut();
            let rc = GetSecurityInfo(
                handle.cast(),
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            );
            if rc != 0 {
                let e = io::Error::from_raw_os_error(rc as i32);
                return Err(io::Error::new(
                    e.kind(),
                    format!("無法確認 pipe 的擁有者：{e}"),
                ));
            }
            let ok = !owner.is_null()
                && (EqualSid(owner, me.psid()) != 0
                    || IsWellKnownSid(owner, WIN_BUILTIN_ADMINISTRATORS_SID) != 0
                    || IsWellKnownSid(owner, WIN_LOCAL_SYSTEM_SID) != 0);
            LocalFree(descriptor);
            if ok {
                Ok(())
            } else {
                Err(denied(
                    "拒絕連線：這個 pipe 不是目前的使用者建立的（可能有其他使用者冒充 agt daemon）",
                ))
            }
        }
    }
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
mod access_tests {
    use super::*;

    fn test_endpoint(dir: &Path) -> String {
        if cfg!(windows) {
            format!(r"\\.\pipe\agt-test-{}", std::process::id())
        } else {
            dir.join("s")
                .join("agt.sock")
                .to_string_lossy()
                .into_owned()
        }
    }

    /// 同一位使用者：兩端的身分檢查都通過，連線可用。
    #[tokio::test]
    async fn same_user_can_connect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let d = tempfile::tempdir().unwrap();
        let ep = test_endpoint(d.path());
        let l = listen(&ep).unwrap();
        let server = tokio::spawn(async move {
            let mut s = accept(&l).await.unwrap();
            let mut b = [0u8; 2];
            s.read_exact(&mut b).await.unwrap();
            b
        });
        let mut c = connect(&ep).await.unwrap();
        c.write_all(b"hi").await.unwrap();
        assert_eq!(&server.await.unwrap(), b"hi");
    }

    #[cfg(unix)]
    #[test]
    fn socket_dir_must_be_private_and_ours() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        // 既有的目錄權限太寬：收緊成 0700
        let dir = d.path().join("loose");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        ensure_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // 符號連結：拒絕（可能指向別人準備好的目錄）
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let e = ensure_private_dir(&link).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
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
