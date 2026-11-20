//! `agt update`：從 GitHub Release 下載新版、核對 SHA256、停止 daemon、換掉執行檔、重新啟動 daemon。
//!
//! 不引入 HTTP / 壓縮相依套件：下載用系統的 `curl`（Windows 10+ 內建 curl.exe），
//! 解壓用 `tar`（Linux 的 GNU tar、Windows 內建的 bsdtar 都能解 .tar.gz 與 .zip）。

use anyhow::{Context, bail};
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 發布用的 repo（`<owner>/<repo>`）：環境變數 `AGT_REPO` 優先，其次是建置時的 `AGT_REPO`
/// （release workflow 會設成 github.repository）。
fn repo() -> anyhow::Result<String> {
    let r = std::env::var("AGT_REPO")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| option_env!("AGT_REPO").map(str::to_string))
        .unwrap_or_default();
    if r.is_empty() || r.matches('/').count() != 1 {
        bail!("不知道要從哪個 GitHub repo 更新：請設定環境變數 AGT_REPO=<owner>/<repo>");
    }
    Ok(r)
}

/// 目前平台在 Release 中的 target 名稱與壓縮檔副檔名。
fn platform() -> anyhow::Result<(&'static str, &'static str)> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok(("x86_64-pc-windows-msvc", "zip"))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(("x86_64-unknown-linux-gnu", "tar.gz"))
    } else {
        bail!("這個平台沒有預先建置的版本，請自行編譯")
    }
}

/// 正規化成 `vX.Y.Z`。
fn normalize(v: &str) -> String {
    format!("v{}", v.trim().trim_start_matches('v'))
}

/// 解析 `X.Y.Z[-pre]`；多餘或非數字的部分視為無效。
fn parse_version(v: &str) -> Option<((u64, u64, u64), bool)> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((c, _)) => (c, true),
        None => (v, false),
    };
    let mut it = core.split('.');
    let n = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    if it.next().is_some() {
        return None;
    }
    Some((n, pre))
}

/// 比較版本：數字相同時，預發行版（`-rc1`）小於正式版。
fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (na, pa) = parse_version(a)?;
    let (nb, pb) = parse_version(b)?;
    Some(na.cmp(&nb).then(pb.cmp(&pa)))
}

fn curl_ok(args: &[&str]) -> anyhow::Result<std::process::Output> {
    let out = Command::new("curl")
        .args(["-fsSL", "--proto", "=https", "--tlsv1.2"])
        .args(args)
        .output()
        .context("無法執行 curl（需要系統安裝 curl）")?;
    if !out.status.success() {
        bail!("curl 失敗：{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out)
}

/// `/releases/latest` 會重新導向到 `/releases/tag/<版本>`，不使用有頻率限制的 API。
fn latest_version(repo: &str) -> anyhow::Result<String> {
    let sink = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let out = curl_ok(&[
        "-o",
        sink,
        "-w",
        "%{url_effective}",
        &format!("https://github.com/{repo}/releases/latest"),
    ])
    .context("查詢最新版本失敗")?;
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let tag = url.rsplit('/').next().unwrap_or_default().to_string();
    if parse_version(&tag).is_none() {
        bail!("無法判斷最新版本（{url}）；可能還沒有任何 Release");
    }
    Ok(tag)
}

// ---- SHA-256（FIPS 180-4）：避免為了一個雜湊多引入相依套件 ----

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, b) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

/// 從 `SHA256SUMS`（`<hash>  <檔名>` 或 `<hash> *<檔名>`）找出指定檔案的雜湊。
fn find_hash(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, name) = l.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file).then(|| hash.to_ascii_lowercase())
    })
}

/// 換掉單一檔案：先放成 `.new`，把舊檔改名為 `.old`（Windows 允許改名執行中的檔案），再把 `.new` 改成正式名稱。
/// 失敗時盡量把舊檔放回去。
fn replace_file(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let with = |ext: &str| {
        let mut s = dest.as_os_str().to_os_string();
        s.push(ext);
        PathBuf::from(s)
    };
    let (new, old) = (with(".new"), with(".old"));
    let _ = fs::remove_file(&new);
    fs::copy(src, &new).with_context(|| format!("無法寫入 {}", new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755))?;
    }
    let had_old = dest.exists();
    if had_old {
        let _ = fs::remove_file(&old); // 上次留下、已不再使用的
        if let Err(e) = fs::rename(dest, &old) {
            let _ = fs::remove_file(&new);
            return Err(e).with_context(|| format!("無法改名 {}", dest.display()));
        }
    }
    if let Err(e) = fs::rename(&new, dest) {
        if had_old {
            let _ = fs::rename(&old, dest);
        }
        let _ = fs::remove_file(&new);
        return Err(e).with_context(|| format!("無法寫入 {}", dest.display()));
    }
    // Unix 上舊檔已沒人使用可直接刪；Windows 上執行中的刪不掉，留到下次啟動清除
    let _ = fs::remove_file(&old);
    Ok(())
}

/// 舊版的 `agtd` 執行檔與它的 `.old`。
fn remove_legacy_agtd(dir: &Path) {
    let name = format!("agtd{}", std::env::consts::EXE_SUFFIX);
    let _ = fs::remove_file(dir.join(&name));
    let _ = fs::remove_file(dir.join(format!("{name}.old")));
}

/// 啟動時清除上次更新留下的 `.old` / `.new`（執行中的刪不掉就保留）。
pub fn cleanup_old() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    for name in ["agt", "agtd"] {
        let base = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        for ext in [".old", ".new"] {
            let _ = fs::remove_file(dir.join(format!("{base}{ext}")));
        }
    }
}

/// 解壓用的 tar。Windows 一律用系統內建的 bsdtar：從 Git Bash 執行時 PATH 上的 `tar` 是 GNU tar，
/// 不能解 .zip，也會把 `C:\...` 的 `C:` 當成遠端主機。
fn tar_program() -> PathBuf {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        let p = PathBuf::from(root).join("System32").join("tar.exe");
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from("tar")
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub async fn run(check: bool, version: Option<String>, yes: bool) -> anyhow::Result<()> {
    let repo = repo()?;
    let (target, ext) = platform()?;
    let current = env!("CARGO_PKG_VERSION");
    let explicit = version.is_some();
    let tag = match version {
        Some(v) => normalize(&v),
        None => latest_version(&repo)?,
    };
    if parse_version(&tag).is_none() {
        bail!("版本格式不正確：{tag}（需要 X.Y.Z）");
    }
    println!("目前版本：v{current}，可用版本：{tag}");
    match compare(current, &tag) {
        Some(Ordering::Equal) => {
            println!("已經是這個版本");
            return Ok(());
        }
        Some(Ordering::Greater) if !explicit => {
            println!("目前版本比最新發布版新，不需要更新（要降版請加 --version）");
            return Ok(());
        }
        _ => {}
    }
    if check {
        println!("執行 `agt update` 更新");
        return Ok(());
    }

    let exe = std::env::current_exe().context("無法取得 agt 的位置")?;
    let dir = exe
        .parent()
        .context("無法取得 agt 所在的資料夾")?
        .to_path_buf();

    let tmp = TempDir(std::env::temp_dir().join(format!("agt-update-{}", std::process::id())));
    let _ = fs::remove_dir_all(&tmp.0);
    fs::create_dir_all(&tmp.0)?;

    let name = format!("agt-{tag}-{target}");
    let archive = tmp.0.join(format!("{name}.{ext}"));
    let base = format!("https://github.com/{repo}/releases/download/{tag}");
    println!("==> 下載 {name}.{ext}");
    curl_ok(&[
        "-o",
        &archive.to_string_lossy(),
        &format!("{base}/{name}.{ext}"),
    ])
    .context("下載失敗")?;
    let sums = curl_ok(&[&format!("{base}/SHA256SUMS")]).context("下載 SHA256SUMS 失敗")?;
    let sums = String::from_utf8_lossy(&sums.stdout).into_owned();

    println!("==> 核對 SHA256");
    let want = find_hash(&sums, &format!("{name}.{ext}"))
        .with_context(|| format!("SHA256SUMS 裡找不到 {name}.{ext}"))?;
    let got = sha256(&fs::read(&archive)?);
    if want != got {
        bail!("SHA256 不符（預期 {want}，實際 {got}），已中止，沒有改動任何檔案");
    }

    let st = Command::new(tar_program())
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&tmp.0)
        .status()
        .context("無法執行 tar")?;
    if !st.success() {
        bail!("解壓縮失敗");
    }
    let suffix = std::env::consts::EXE_SUFFIX;
    let files: Vec<(PathBuf, PathBuf)> = ["agt"]
        .iter()
        .map(|n| {
            let f = format!("{n}{suffix}");
            (tmp.0.join(&name).join(&f), dir.join(&f))
        })
        .collect();
    if let Some((missing, _)) = files.iter().find(|(s, _)| !s.is_file()) {
        bail!("壓縮檔內容不完整：缺少 {}", missing.display());
    }

    // 先確認可以寫入安裝位置，再停 daemon，避免停了卻換不掉
    let probe = dir.join(".agt-update-probe");
    fs::write(&probe, b"").with_context(|| format!("沒有寫入 {} 的權限", dir.display()))?;
    let _ = fs::remove_file(&probe);

    println!("==> 停止 daemon");
    let was_running = crate::stop_daemon(yes).await?;

    println!("==> 安裝到 {}", dir.display());
    let mut done: Vec<&PathBuf> = Vec::new();
    for (src, dest) in &files {
        if let Err(e) = replace_file(src, dest) {
            eprintln!("更新失敗：{e:#}（已換掉的檔案：{}）", done.len());
            return Err(e);
        }
        done.push(dest);
    }

    if was_running {
        // 用新版的 agt 重新啟動（舊 daemon 已結束）
        match crate::ensure_daemon("cli").await {
            Ok(_) => println!("daemon 已用新版重新啟動（分頁保留，可用 Ctrl+A → R 接續 Agent）"),
            Err(e) => eprintln!("警告：daemon 沒有自動啟動：{e:#}（執行 agt 會自動啟動）"),
        }
    }
    // 舊版有獨立的 agtd：已併入 agt（`agt daemon run`），刪掉不再使用的檔案（使用中就留到下次）
    remove_legacy_agtd(&dir);
    println!("已更新到 {tag}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 跨區塊邊界（56 與 64 位元組附近）
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn version_compare() {
        use Ordering::*;
        assert_eq!(compare("0.2.0", "v0.3.0"), Some(Less));
        assert_eq!(compare("0.10.0", "0.9.9"), Some(Greater));
        assert_eq!(compare("1.0.0", "v1.0.0"), Some(Equal));
        assert_eq!(compare("1.0.0-rc1", "1.0.0"), Some(Less));
        assert_eq!(compare("1.0.0", "1.0.0-rc1"), Some(Greater));
        assert_eq!(compare("1.0", "1.0.0"), None);
        assert_eq!(compare("latest", "1.0.0"), None);
        assert_eq!(normalize("0.3.0"), "v0.3.0");
        assert_eq!(normalize("v0.3.0"), "v0.3.0");
    }

    #[test]
    fn finds_hash_in_sums() {
        let sums = "AAA111  agt-v1-x.zip\nbbb222 *agt-v1-y.tar.gz\n";
        assert_eq!(find_hash(sums, "agt-v1-x.zip").as_deref(), Some("aaa111"));
        assert_eq!(
            find_hash(sums, "agt-v1-y.tar.gz").as_deref(),
            Some("bbb222")
        );
        assert_eq!(find_hash(sums, "agt-v1-z.zip"), None);
    }

    #[test]
    fn replace_file_swaps_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let (src, dest) = (d.path().join("src"), d.path().join("agt"));
        fs::write(&src, b"new").unwrap();
        fs::write(&dest, b"old").unwrap();
        replace_file(&src, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert!(!d.path().join("agt.new").exists());
        // 目的檔不存在時也能放進去
        let dest2 = d.path().join("agtd");
        replace_file(&src, &dest2).unwrap();
        assert_eq!(fs::read(&dest2).unwrap(), b"new");
    }

    #[test]
    fn legacy_agtd_is_removed() {
        let d = tempfile::tempdir().unwrap();
        let name = format!("agtd{}", std::env::consts::EXE_SUFFIX);
        fs::write(d.path().join(&name), b"x").unwrap();
        fs::write(d.path().join(format!("{name}.old")), b"x").unwrap();
        fs::write(d.path().join("agt"), b"keep").unwrap();
        remove_legacy_agtd(d.path());
        assert!(!d.path().join(&name).exists());
        assert!(!d.path().join(format!("{name}.old")).exists());
        assert!(d.path().join("agt").exists());
    }
}
