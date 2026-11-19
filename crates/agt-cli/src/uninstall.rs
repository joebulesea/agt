//! `agt uninstall`：停止 daemon、移除沙箱 / 容器、刪除執行檔與 PATH 設定；
//! 設定與資料、worktree 只在使用者明確同意（`--purge` / `--worktrees` 或互動詢問）時刪除。

use crate::setup::{capture, on_path};
use std::path::{Path, PathBuf};

/// 解析 `sbx ls` 的輸出，取出 agt 建立的沙箱名稱（`agt-` 開頭的第一欄；略過標題列）。
fn parse_sandbox_names(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|n| n.starts_with("agt-"))
        .map(str::to_string)
        .collect()
}

/// 從 `;` 分隔的 PATH 字串移除指定資料夾（Windows 不分大小寫、忽略結尾的 `\`）。回傳 None 表示沒有這一項。
fn remove_from_path(path: &str, dir: &str) -> Option<String> {
    let norm = |s: &str| s.trim_end_matches(['\\', '/']).to_ascii_lowercase();
    let want = norm(dir);
    let parts: Vec<&str> = path.split(';').filter(|p| !p.is_empty()).collect();
    let kept: Vec<&str> = parts.iter().copied().filter(|p| norm(p) != want).collect();
    (kept.len() != parts.len()).then(|| kept.join(";"))
}

pub(crate) struct Dirs {
    pub config: PathBuf,
    pub data: PathBuf,
    #[allow(dead_code)]
    pub logs: Option<PathBuf>,
    pub worktrees: PathBuf,
}

/// 與 daemon 的 `Paths::detect` 相同的規則（agt-cli 不依賴 agt-daemon）。
pub(crate) fn dirs() -> Dirs {
    let base = directories::BaseDirs::new();
    let home = base
        .as_ref()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let (config, data, logs, home) = match std::env::var("AGT_HOME").ok().filter(|h| !h.is_empty())
    {
        Some(h) => {
            let h = PathBuf::from(h);
            (h.clone(), h.join("data"), None, h)
        }
        None => {
            let config = base
                .as_ref()
                .map(|b| b.config_dir().join("agt"))
                .unwrap_or_else(|| home.join(".config/agt"));
            let data = if cfg!(windows) {
                config.join("data")
            } else {
                base.as_ref()
                    .map(|b| b.data_local_dir().join("agt"))
                    .unwrap_or_else(|| home.join(".agt/data"))
            };
            let logs = cfg!(windows)
                .then(|| base.as_ref().map(|b| b.data_local_dir().join("agt")))
                .flatten();
            (config, data, logs, home)
        }
    };
    let root = std::fs::read_to_string(config.join("config.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("general")?
                .get("worktree_root")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| "~/.agt/worktrees".to_string());
    let worktrees = match root.strip_prefix("~/").or_else(|| root.strip_prefix("~\\")) {
        Some(rest) => home.join(rest),
        None if root == "~" => home,
        None => PathBuf::from(root),
    };
    Dirs {
        config,
        data,
        logs,
        worktrees,
    }
}

fn exists(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

fn remove_dir(p: &Path) {
    match std::fs::remove_dir_all(p) {
        Ok(()) => println!("  已刪除 {}", p.display()),
        Err(e) if !exists(p) => {
            let _ = e;
        }
        // Windows：執行中的 agt.exe.old 等檔案刪不掉，登出或重新開機後可自行刪除
        Err(e) => eprintln!("  無法完全刪除 {}：{e}（剩下的請手動刪除）", p.display()),
    }
}

/// 直接讀寫登錄檔的使用者 PATH：保留 `REG_EXPAND_SZ` 與 `%VAR%`（`SetEnvironmentVariable` 會把它們展開並改成 `REG_SZ`）。
const PS_GET_PATH: &str = "$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment'); \
     [Console]::Out.Write($k.GetValue('Path','','DoNotExpandEnvironmentNames'))";
/// 寫回後用設定再刪除一個暫時變數來廣播 WM_SETTINGCHANGE，新開的終端機才會讀到新 PATH。
const PS_SET_PATH: &str = "$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$true); \
     $k.SetValue('Path',$env:AGT_NEW_PATH,'ExpandString'); \
     [Environment]::SetEnvironmentVariable('AGT_PATH_REFRESH','1','User'); \
     [Environment]::SetEnvironmentVariable('AGT_PATH_REFRESH',$null,'User')";

/// 移除 Windows 使用者 PATH 中的 `dir`。
fn remove_user_path(dir: &Path) {
    if !cfg!(windows) {
        return;
    }
    let Some((true, cur)) = capture("powershell", &["-NoProfile", "-Command", PS_GET_PATH]) else {
        eprintln!("  無法讀取使用者 PATH，請手動移除 {}", dir.display());
        return;
    };
    let Some(new) = remove_from_path(&cur, &dir.to_string_lossy()) else {
        return;
    };
    // 新值透過環境變數傳入，避免引號跳脫問題
    let ok = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", PS_SET_PATH])
        .env("AGT_NEW_PATH", &new)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!(
            "  已從使用者 PATH 移除 {}（已開啟的終端機需要重新開啟）",
            dir.display()
        );
    } else {
        eprintln!("  更新使用者 PATH 失敗，請手動移除 {}", dir.display());
    }
}

pub async fn run(yes: bool, purge: bool, worktrees: bool, dry_run: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let d = dirs();
    let suffix = std::env::consts::EXE_SUFFIX;
    let bins: Vec<PathBuf> = ["agt", "agtd"]
        .iter()
        .map(|n| exe_dir.join(format!("{n}{suffix}")))
        .filter(|p| exists(p))
        .collect();

    // 找出 agt 建立的容器與沙箱
    let containers: Vec<String> = capture("docker", &["ps", "-aq", "--filter", "label=agt=1"])
        .filter(|(ok, _)| *ok)
        .map(|(_, o)| o.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let sandboxes: Vec<String> = if on_path("sbx") {
        capture("sbx", &["ls"])
            .filter(|(ok, _)| *ok)
            .map(|(_, o)| parse_sandbox_names(&o))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    println!("將會處理：");
    println!("  - 停止 daemon（會結束所有 Agent）");
    if !containers.is_empty() {
        println!("  - 移除 {} 個 agt 的 Docker 容器", containers.len());
    }
    if !sandboxes.is_empty() {
        println!(
            "  - 移除 {} 個 agt 的 Docker 沙箱（{}）",
            sandboxes.len(),
            sandboxes.join(", ")
        );
    }
    for b in &bins {
        println!("  - 刪除 {}", b.display());
    }
    if cfg!(windows) {
        println!("  - 從使用者 PATH 移除 {}", exe_dir.display());
    }
    println!(
        "不會處理（除非另外同意）：設定與資料 {}、worktree {}",
        d.config.display(),
        d.worktrees.display()
    );
    println!("不會刪除：Docker 映像檔與 volume（登入資料）、各 repo 裡的 agt/* branch");
    if dry_run {
        println!("\n（--dry-run：沒有改動任何東西）");
        return Ok(());
    }
    if !(yes || crate::confirm("\n確定要解除安裝 agt 嗎？")) {
        anyhow::bail!("已取消");
    }

    println!("==> 停止 daemon");
    crate::stop_daemon(yes).await?;

    if !containers.is_empty() {
        println!("==> 移除容器");
        let mut args = vec!["rm", "-f"];
        args.extend(containers.iter().map(String::as_str));
        let ok = capture("docker", &args).is_some_and(|(ok, _)| ok);
        println!(
            "  {}",
            if ok {
                "完成"
            } else {
                "失敗，請手動執行 docker rm -f"
            }
        );
    }
    for s in &sandboxes {
        println!("==> 移除沙箱 {s}");
        if !capture("sbx", &["rm", "-f", s]).is_some_and(|(ok, _)| ok) {
            eprintln!("  失敗，請手動執行 sbx rm -f {s}");
        }
    }

    // 設定與資料、worktree：旗標或互動式同意
    let interactive = !yes;
    let purge = purge
        || (interactive
            && exists(&d.config)
            && crate::confirm(&format!(
                "也要刪除設定與資料（{}，含分頁資料庫與終端機紀錄）嗎？",
                d.config.display()
            )));
    let worktrees = worktrees
        || (interactive
            && exists(&d.worktrees)
            && crate::confirm(&format!(
                "也要刪除所有 worktree（{}）嗎？沒有 commit / merge 的修改會永久遺失",
                d.worktrees.display()
            )));
    if worktrees {
        println!("==> 刪除 worktree");
        remove_dir(&d.worktrees);
        println!("  各 repo 裡留下的 worktree 登記可用 `git worktree prune` 清除");
    }
    if purge {
        println!("==> 刪除設定與資料");
        remove_dir(&d.data);
        if let Some(l) = &d.logs {
            remove_dir(l);
        }
        // 執行檔在設定資料夾裡（Windows）時，下面刪除執行檔後再整個刪掉
    }

    println!("==> 刪除執行檔");
    for b in &bins {
        if std::fs::remove_file(b).is_ok() {
            println!("  已刪除 {}", b.display());
        } else {
            // Windows：執行中的 exe 不能刪，但可以改名；稍後手動刪除 .old
            let mut old = b.as_os_str().to_os_string();
            old.push(".old");
            match std::fs::rename(b, &old) {
                Ok(()) => println!(
                    "  {} 執行中，已改名為 .old（結束後可手動刪除）",
                    b.display()
                ),
                Err(e) => eprintln!("  無法刪除 {}：{e}", b.display()),
            }
        }
    }
    remove_user_path(&exe_dir);
    if purge {
        if std::env::var("AGT_HOME").is_ok_and(|h| !h.is_empty()) {
            // 可攜模式：AGT_HOME 也是 worktree 的預設位置，只刪 agt 自己的檔案，不整個刪掉
            for name in ["config.toml", "setup-skip.txt", "docker", "data"] {
                let p = d.config.join(name);
                if p.is_dir() {
                    remove_dir(&p);
                } else if std::fs::remove_file(&p).is_ok() {
                    println!("  已刪除 {}", p.display());
                }
            }
            let _ = std::fs::remove_dir(&d.config); // 空了才會成功
        } else {
            remove_dir(&d.config);
        }
    }
    println!("agt 已解除安裝");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_names_skip_header_and_others() {
        let out = "NAME        AGENT   STATUS\nagt-ab12cd  claude  running\nmine        claude  stopped\nagt-ef34    codex   stopped\n";
        assert_eq!(parse_sandbox_names(out), ["agt-ab12cd", "agt-ef34"]);
        assert!(parse_sandbox_names("").is_empty());
    }

    #[test]
    fn path_removal() {
        let p = r"C:\a;C:\Users\x\AppData\Roaming\agt;C:\b";
        assert_eq!(
            remove_from_path(p, r"c:\users\x\appdata\roaming\agt\").as_deref(),
            Some(r"C:\a;C:\b")
        );
        assert_eq!(remove_from_path(p, r"C:\none"), None);
        assert_eq!(remove_from_path(r"C:\agt", r"C:\agt").as_deref(), Some(""));
    }
}
