//! `agt update`（自我更新）與 `agt uninstall`。
//!
//! 不引入 HTTP / 雜湊相依套件：下載用系統的 `curl`（Windows 10 以後內建）、
//! 核對 SHA256 用 `sha256sum` / `shasum` / `certutil`、解壓用 `tar`（Windows 內建的 bsdtar 也能解 zip）。
//! 發布格式見 `.github/workflows/release.yml`：`agt-<版本>-<平台>.<zip|tar.gz>` 加上 `SHA256SUMS`。

use anyhow::{Context, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_REPO: &str = "OWNER/agt";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 發布用的 GitHub repo：環境變數 `AGT_REPO` > 建置時的 `AGT_REPO` > 預設值。
fn repo() -> String {
    std::env::var("AGT_REPO")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| option_env!("AGT_REPO").map(String::from))
        .unwrap_or_else(|| DEFAULT_REPO.into())
}

/// (平台名稱, 壓縮檔副檔名)；沒有預編譯版本的平台回傳 None。
fn platform() -> Option<(&'static str, &'static str)> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some(("windows-x64", "zip"))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("linux-x64", "tar.gz"))
    } else {
        None
    }
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// 把 `x.y.z` 轉成數字（不能解析的部分視為 0）。
fn parse_version(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split(['.', '-', '+'])
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

fn is_newer(candidate: &str, current: &str) -> bool {
    let (mut a, mut b) = (parse_version(candidate), parse_version(current));
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

/// 從 `SHA256SUMS`（`<hash>  <檔名>` 或 `<hash> *<檔名>`）找出某個檔案的雜湊。
fn find_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let (hash, file) = (it.next()?, it.next()?);
        (file.trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

fn run(cmd: &mut Command, what: &str) -> anyhow::Result<String> {
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("無法執行{what}（找不到指令？）"))?;
    if !out.status.success() {
        bail!(
            "{what}失敗：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 最新版本號（不含 v）。讀 `releases/latest` 轉址後的網址，不呼叫 API。
fn latest_version(repo: &str) -> anyhow::Result<String> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let url = run(
        Command::new("curl").args([
            "-fsSL",
            "-o",
            null,
            "-w",
            "%{url_effective}",
            &format!("https://github.com/{repo}/releases/latest"),
        ]),
        "查詢最新版本（curl）",
    )?;
    match url.trim().rsplit_once("/tag/v") {
        Some((_, v)) if !v.is_empty() => Ok(v.to_string()),
        _ => bail!("無法從 {} 判斷最新版本", url.trim()),
    }
}

fn sha256_of(path: &Path) -> anyhow::Result<String> {
    let out = if cfg!(windows) {
        let o = run(
            Command::new("certutil")
                .args(["-hashfile"])
                .arg(path)
                .arg("SHA256"),
            "計算 SHA256（certutil）",
        )?;
        // 第 1 行是說明，第 2 行是雜湊（可能有空白）
        o.lines().nth(1).unwrap_or("").replace(' ', "")
    } else {
        let o = run(Command::new("sha256sum").arg(path), "計算 SHA256").or_else(|_| {
            run(
                Command::new("shasum").args(["-a", "256"]).arg(path),
                "計算 SHA256（shasum）",
            )
        })?;
        o.split_whitespace().next().unwrap_or("").to_string()
    };
    if out.len() != 64 {
        bail!("無法取得 SHA256");
    }
    Ok(out.to_ascii_lowercase())
}

fn download(url: &str, dest: &Path) -> anyhow::Result<()> {
    run(
        Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(dest)
            .arg(url),
        &format!("下載 {url}"),
    )
    .map(|_| ())
}

/// 把新檔放進安裝目錄。執行中的檔案不能覆蓋（Windows）：先把舊檔改名為 `.old`，
/// 複製失敗時改回去；`.old` 在下次啟動時由 [`cleanup_old`] 清除。
fn swap_in(new_dir: &Path, install_dir: &Path) -> anyhow::Result<()> {
    let mut done: Vec<(PathBuf, PathBuf)> = vec![];
    for base in ["agt"] {
        let name = exe_name(base);
        let target = install_dir.join(&name);
        let old = install_dir.join(format!("{name}.old"));
        let _ = std::fs::remove_file(&old);
        let had_old = target.exists();
        let result = (|| -> anyhow::Result<()> {
            if had_old {
                std::fs::rename(&target, &old)
                    .with_context(|| format!("無法改名 {}", target.display()))?;
            }
            std::fs::copy(new_dir.join(&name), &target)
                .with_context(|| format!("無法寫入 {}", target.display()))?;
            Ok(())
        })();
        if let Err(e) = result {
            // 回復這一個與之前已換好的檔案
            if had_old {
                let _ = std::fs::remove_file(&target);
                let _ = std::fs::rename(&old, &target);
            }
            for (t, o) in done.iter().rev() {
                let _ = std::fs::remove_file(t);
                let _ = std::fs::rename(o, t);
            }
            return Err(e);
        }
        done.push((target, old));
    }
    for (_, old) in &done {
        let _ = std::fs::remove_file(old);
    }
    // 舊版有獨立的 agtd：已併入 agt（`agt daemon run`），刪掉不再使用的檔案（使用中就留到下次）
    remove_legacy_agtd(install_dir);
    Ok(())
}

/// 舊版的 `agtd` 執行檔與它的 `.old`。
fn remove_legacy_agtd(dir: &Path) {
    let name = exe_name("agtd");
    let _ = std::fs::remove_file(dir.join(&name));
    let _ = std::fs::remove_file(dir.join(format!("{name}.old")));
}

/// 清除上次更新留下、已不再使用的 `.old`（失敗就留到下次）。
pub fn cleanup_old() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    for base in ["agt", "agtd"] {
        let _ = std::fs::remove_file(dir.join(format!("{}.old", exe_name(base))));
    }
}

pub async fn update(check: bool, version: Option<String>, yes: bool) -> anyhow::Result<()> {
    let repo = repo();
    if repo.starts_with("OWNER/") {
        bail!("尚未設定發布位置：請設定環境變數 AGT_REPO=<owner>/<repo>");
    }
    let Some((plat, ext)) = platform() else {
        bail!("這個平台沒有預編譯版本，請自行用 cargo build --release 編譯");
    };
    let target = match version {
        Some(v) => v.trim_start_matches('v').to_string(),
        None => latest_version(&repo)?,
    };
    println!("目前版本 {VERSION}，最新版本 {target}");
    if !is_newer(&target, VERSION) && !yes {
        println!("已經是最新版本");
        return Ok(());
    }
    if check {
        if is_newer(&target, VERSION) {
            println!("有新版本，執行 `agt update` 更新");
        }
        return Ok(());
    }

    let tmp = tempdir()?;
    let result = update_in(&tmp, &repo, plat, ext, &target, yes).await;
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

async fn update_in(
    tmp: &Path,
    repo: &str,
    plat: &str,
    ext: &str,
    target: &str,
    yes: bool,
) -> anyhow::Result<()> {
    let name = format!("agt-{target}-{plat}.{ext}");
    let base = format!("https://github.com/{repo}/releases/download/v{target}");
    println!("下載 {name}…");
    download(&format!("{base}/{name}"), &tmp.join(&name))?;
    download(&format!("{base}/SHA256SUMS"), &tmp.join("SHA256SUMS"))?;
    let sums = std::fs::read_to_string(tmp.join("SHA256SUMS")).context("讀取 SHA256SUMS")?;
    let expected = find_sum(&sums, &name).with_context(|| format!("SHA256SUMS 裡找不到 {name}"))?;
    let actual = sha256_of(&tmp.join(&name))?;
    if expected != actual {
        bail!("SHA256 不符（預期 {expected}，實際 {actual}），已中止，沒有更動任何檔案");
    }
    println!("SHA256 核對通過");

    let x = tmp.join("x");
    std::fs::create_dir_all(&x)?;
    run(
        Command::new("tar")
            .arg("-xf")
            .arg(tmp.join(&name))
            .arg("-C")
            .arg(&x),
        "解壓縮（tar）",
    )?;
    if !x.join(exe_name("agt")).is_file() {
        bail!("壓縮檔內容不符預期（缺少 {}）", exe_name("agt"));
    }

    let exe = std::env::current_exe().context("找不到目前的執行檔位置")?;
    let dir = exe.parent().context("找不到安裝目錄")?.to_path_buf();
    // 有 Agent 在執行時會先詢問（結束 daemon 會一併結束它們）
    crate::stop_daemon(yes).await?;
    swap_in(&x, &dir)?;
    println!("已更新到 {target}（{}）", dir.display());
    println!("daemon 已結束，下次執行 agt 會自動啟動新版；分頁都還在，可用 Ctrl+A → R 接續對話。");
    Ok(())
}

fn tempdir() -> anyhow::Result<PathBuf> {
    let p = std::env::temp_dir().join(format!("agt-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).context("無法建立暫存目錄")?;
    Ok(p)
}

/// 移除 agt：停止 daemon、刪除沙箱容器、刪除執行檔（Windows 並移除 PATH 設定），
/// 設定與資料、worktree 另外詢問（`--purge` / `--purge-worktrees` 可略過詢問）。
pub async fn uninstall(yes: bool, purge: bool, purge_worktrees: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("找不到目前的執行檔位置")?;
    let dir = exe.parent().context("找不到安裝目錄")?.to_path_buf();
    if !yes
        && !crate::confirm(&format!(
            "要移除 agt 嗎？會結束 daemon 與所有 Agent，並刪除 {}",
            dir.display()
        ))
    {
        bail!("已取消");
    }

    // 先記下資料目錄（daemon 結束後就問不到了）
    let mut data_dirs: Vec<PathBuf> = vec![];
    if let Ok(conn) = crate::connect_kind("cli").await
        && let Ok(agt_proto::ReplyBody::Daemon(i)) = conn
            .client
            .request(agt_proto::ClientMsg::DaemonStatus)
            .await
    {
        data_dirs.push(PathBuf::from(i.data_dir));
    }
    crate::stop_daemon(true).await?;

    // 沙箱容器（標記 agt=1）
    if let Ok(ids) = run(
        Command::new("docker").args(["ps", "-aq", "--filter", "label=agt=1"]),
        "列出 agt 的容器",
    ) {
        let ids: Vec<&str> = ids.split_whitespace().collect();
        if !ids.is_empty() {
            let _ = run(
                Command::new("docker").args(["rm", "-f"]).args(&ids),
                "刪除容器",
            );
            println!("已刪除 {} 個 Docker 容器", ids.len());
        }
    }
    println!(
        "Docker Sandboxes 的沙箱請自行確認：`sbx ls`，再用 `sbx rm <名稱>` 刪除名稱以 agt- 開頭的項目。"
    );

    let cfg_dir = crate::setup::config_path().parent().map(Path::to_path_buf);
    if let Some(d) = cfg_dir.clone() {
        data_dirs.push(d);
    }
    if let Some(b) = directories::BaseDirs::new() {
        data_dirs.push(b.data_local_dir().join("agt"));
    }
    data_dirs.sort();
    data_dirs.dedup();

    remove_exes(&dir);
    #[cfg(windows)]
    remove_from_user_path(&dir);

    if purge
        || (!yes
            && crate::confirm(&format!(
                "也刪除設定與資料（資料庫、終端機紀錄）嗎？\n  {}",
                data_dirs
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            )))
    {
        for d in &data_dirs {
            if d.exists() {
                match std::fs::remove_dir_all(d) {
                    Ok(()) => println!("已刪除 {}", d.display()),
                    Err(e) => println!("無法完全刪除 {}：{e}（可能還有檔案被使用中）", d.display()),
                }
            }
        }
    }
    let wt = directories::UserDirs::new().map(|u| u.home_dir().join(".agt").join("worktrees"));
    if let Some(wt) = wt.filter(|p| p.exists()) {
        if purge_worktrees
            || (!yes
                && crate::confirm(&format!(
                    "也刪除 worktree 副本嗎？未合併的變更會一併消失。\n  {}",
                    wt.display()
                )))
        {
            match std::fs::remove_dir_all(&wt) {
                Ok(()) => println!("已刪除 {}", wt.display()),
                Err(e) => println!("無法刪除 {}：{e}", wt.display()),
            }
        } else {
            println!("保留 worktree：{}", wt.display());
        }
    }
    println!("agt 已移除。");
    Ok(())
}

/// 刪除執行檔。Windows 不能刪除執行中的檔案，改名為 `.old` 後嘗試刪除，刪不掉的留著。
fn remove_exes(dir: &Path) {
    for base in ["agt", "agtd"] {
        let p = dir.join(exe_name(base));
        if !p.exists() {
            continue;
        }
        if std::fs::remove_file(&p).is_ok() {
            continue;
        }
        let old = dir.join(format!("{}.old", exe_name(base)));
        let _ = std::fs::remove_file(&old);
        if std::fs::rename(&p, &old).is_ok() {
            let _ = std::fs::remove_file(&old);
            if old.exists() {
                println!("{} 使用中，結束後請手動刪除", old.display());
            }
        } else {
            println!("無法刪除 {}，請手動刪除", p.display());
        }
    }
}

#[cfg(windows)]
fn remove_from_user_path(dir: &Path) {
    // 目錄用環境變數傳遞，不放進指令字串
    let script = "$d=$env:AGT_DIR; $p=@([Environment]::GetEnvironmentVariable('Path','User') -split ';' | Where-Object { $_ -and $_ -ne $d }); [Environment]::SetEnvironmentVariable('Path',($p -join ';'),'User')";
    let ok = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .env("AGT_DIR", dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!("已從使用者 PATH 移除 {}", dir.display());
    } else {
        println!("無法修改 PATH，請自行移除 {}", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.2", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
    }

    #[test]
    fn sums_lookup() {
        let s = "AABB  agt-1.0.0-linux-x64.tar.gz\nccdd *agt-1.0.0-windows-x64.zip\n";
        assert_eq!(
            find_sum(s, "agt-1.0.0-linux-x64.tar.gz").as_deref(),
            Some("aabb")
        );
        assert_eq!(
            find_sum(s, "agt-1.0.0-windows-x64.zip").as_deref(),
            Some("ccdd")
        );
        assert!(find_sum(s, "other").is_none());
    }

    #[test]
    fn swap_replaces_and_removes_legacy_agtd() {
        let new = tempfile::tempdir().unwrap();
        let inst = tempfile::tempdir().unwrap();
        std::fs::write(new.path().join(exe_name("agt")), "new").unwrap();
        std::fs::write(inst.path().join(exe_name("agt")), "old").unwrap();
        // 舊版的獨立 daemon 與它的 .old
        std::fs::write(inst.path().join(exe_name("agtd")), "legacy").unwrap();
        std::fs::write(inst.path().join(format!("{}.old", exe_name("agtd"))), "x").unwrap();
        swap_in(new.path(), inst.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(inst.path().join(exe_name("agt"))).unwrap(),
            "new"
        );
        assert!(
            !inst
                .path()
                .join(format!("{}.old", exe_name("agt")))
                .exists()
        );
        assert!(!inst.path().join(exe_name("agtd")).exists());
        assert!(
            !inst
                .path()
                .join(format!("{}.old", exe_name("agtd")))
                .exists()
        );
    }

    #[test]
    fn swap_failure_restores_old_file() {
        let new = tempfile::tempdir().unwrap();
        let inst = tempfile::tempdir().unwrap();
        // 新檔缺少 → 複製失敗，舊檔要改回原名
        std::fs::write(inst.path().join(exe_name("agt")), "old").unwrap();
        std::fs::write(inst.path().join(exe_name("agtd")), "legacy").unwrap();
        assert!(swap_in(new.path(), inst.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(inst.path().join(exe_name("agt"))).unwrap(),
            "old"
        );
        // 更新失敗時不能動到舊的 agtd
        assert!(inst.path().join(exe_name("agtd")).exists());
    }
}
