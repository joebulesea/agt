//! `agt setup`：檢查環境（git、Claude Code、Docker、Docker Sandboxes），缺少的項目可以選擇安裝
//! （見 [`crate::deps`]），再依偵測結果產生設定檔中的 Agent 區段，並把 Docker 映像檔需要的
//! Dockerfile 寫到設定目錄。
//!
//! 只有 `agt.exe`（與 `agtd.exe`）時也能完成設定：Agent 範例與 Dockerfile 都在編譯時內嵌。
//! 第一次執行 `agt`（還沒有設定檔）時會自動執行，介面類似 Claude Code 的初次設定。

use crate::deps;
use crate::prompt::{self, Item};
use anyhow::Context;
use crossterm::style::Stylize;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// 預設設定（Agent 範例的來源，見 [`example_block`]）。
const DEFAULT_TOML: &str = include_str!("../../../config/default.toml");

/// 內嵌的 Docker 映像檔內容：(子目錄, 檔名, 內容)。
const DOCKER_FILES: &[(&str, &str, &str)] = &[
    (
        "claude",
        "Dockerfile",
        include_str!("../../../docker/claude/Dockerfile"),
    ),
    (
        "claude",
        "entrypoint.sh",
        include_str!("../../../docker/claude/entrypoint.sh"),
    ),
    (
        "agy",
        "Dockerfile",
        include_str!("../../../docker/agy/Dockerfile"),
    ),
    (
        "agy",
        "entrypoint.sh",
        include_str!("../../../docker/agy/entrypoint.sh"),
    ),
    (
        "copilot",
        "Dockerfile",
        include_str!("../../../docker/copilot/Dockerfile"),
    ),
    (
        "copilot",
        "entrypoint.sh",
        include_str!("../../../docker/copilot/entrypoint.sh"),
    ),
    (
        "codex",
        "Dockerfile",
        include_str!("../../../docker/codex/Dockerfile"),
    ),
    (
        "codex",
        "entrypoint.sh",
        include_str!("../../../docker/codex/entrypoint.sh"),
    ),
];

pub struct Options {
    /// 不詢問，全部採用預設答案。
    pub yes: bool,
    /// 需要的 Docker 映像檔不存在時直接建置（搭配 `yes` 使用）。
    pub build: bool,
    /// 不詢問時也用 winget 安裝缺少的必要項目（搭配 `yes` 使用）。
    pub install: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Docker {
    Missing,
    /// 有 docker 指令，但 Docker Desktop / Engine 沒有在執行。
    NotRunning,
    Running(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Sbx {
    Missing,
    NotSignedIn(String),
    /// (版本, 是否已設定網路政策)
    Ready(String, bool),
}

#[derive(Debug, Clone)]
pub(crate) struct Env {
    pub git: Option<String>,
    pub claude: bool,
    pub agy: bool,
    pub copilot: bool,
    pub codex: bool,
    pub docker: Docker,
    pub sbx: Sbx,
    /// WSL2 是否可用（Docker Desktop 需要）；只在 Windows 偵測，其他平台為 None。
    pub wsl: Option<bool>,
    /// 是否有 winget（只在 Windows 偵測）。
    pub winget: bool,
}

/// 可以加入設定檔的 Agent。
struct Candidate {
    name: &'static str,
    desc: &'static str,
    /// 需要的 Docker 映像檔：(映像檔名稱, 內嵌檔案的子目錄)
    image: Option<(&'static str, &'static str)>,
    default_on: bool,
    /// 範例用 `extends` 沿用的 Agent（例如 advisor 版的 `claude-sbx`）。
    /// 它不在設定檔中時會一併加入，見 [`with_bases`]。
    extends: Option<&'static str>,
}

/// 設定檔位置；與 daemon 的規則相同（`AGT_HOME` 優先）。
pub fn config_path() -> PathBuf {
    if let Ok(h) = std::env::var("AGT_HOME")
        && !h.is_empty()
    {
        return PathBuf::from(h).join("config.toml");
    }
    directories::BaseDirs::new()
        .map(|b| b.config_dir().join("agt"))
        .unwrap_or_else(|| PathBuf::from(".agt"))
        .join("config.toml")
}

/// 第一次使用（還沒有設定檔、而且在互動式終端機中）時要先執行設定。
pub fn needs_first_run() -> bool {
    !config_path().exists() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// 執行指令並取得輸出（stdout + stderr）；指令不存在時回傳 None。
pub(crate) fn capture(program: &str, args: &[&str]) -> Option<(bool, String)> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some((out.status.success(), text.trim().to_string()))
}

/// 在 PATH 中找指令（含 Windows 的 .exe / .cmd / .ps1 等）。
pub(crate) fn on_path(name: &str) -> bool {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_ascii_lowercase())
            .chain([".ps1".to_string()])
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|dir| {
            exts.iter()
                .any(|e| dir.join(format!("{name}{e}")).is_file())
        })
    })
}

pub(crate) fn detect() -> Env {
    let git = capture("git", &["--version"])
        .filter(|(ok, _)| *ok)
        .map(|(_, v)| v.trim_start_matches("git version ").to_string());
    let docker = match capture("docker", &["version", "--format", "{{.Server.Version}}"]) {
        None => Docker::Missing,
        Some((true, v)) if !v.is_empty() => Docker::Running(v),
        Some(_) => Docker::NotRunning,
    };
    let sbx = match capture("sbx", &["version"]) {
        None => Sbx::Missing,
        Some((_, v)) => {
            let ver = v
                .split_whitespace()
                .find(|w| {
                    w.strip_prefix('v')
                        .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
                })
                .unwrap_or("?")
                .to_string();
            match capture("sbx", &["ls"]) {
                Some((_, out)) if out.contains("not signed in") => Sbx::NotSignedIn(ver),
                _ => {
                    let policy = capture("sbx", &["policy", "ls"]).is_some_and(|(ok, out)| {
                        ok && out.lines().filter(|l| !l.trim().is_empty()).count() > 1
                    });
                    Sbx::Ready(ver, policy)
                }
            }
        }
    };
    let windows = cfg!(windows);
    Env {
        git,
        claude: on_path("claude"),
        agy: on_path("agy"),
        copilot: on_path("copilot"),
        codex: on_path("codex"),
        docker,
        sbx,
        // `wsl --status` 的輸出是 UTF-16，只看結束代碼；沒有安裝時 wsl.exe 可能是只會提示安裝的版本
        wsl: windows.then(|| capture("wsl", &["--status"]).is_some_and(|(ok, _)| ok)),
        winget: windows && on_path("winget"),
    }
}

/// 從預設設定中取出某個 Agent 的範例區段（去掉行首的 `# `）。
/// 範例從 `# [agents.<name>]` 開始，到空行或單獨的 `#` 為止。
pub fn example_block(name: &str) -> Option<String> {
    let head = format!("# [agents.{name}]");
    let mut lines = DEFAULT_TOML.lines().skip_while(|l| l.trim() != head);
    let mut out = String::new();
    for l in lines.by_ref() {
        let Some(rest) = l.strip_prefix("# ") else {
            break;
        };
        out.push_str(rest);
        out.push('\n');
    }
    (!out.is_empty()).then_some(out)
}

/// 設定檔中是否已有這個 Agent。用 TOML 解析，才能認得 `[agents."x"]`、行尾註解等寫法；
/// 否則會再附加一個同名區段，重複的表格會讓整個設定檔無法載入。
/// 設定檔本身不合法時退回逐行比對（daemon 會另外回報錯誤）。
fn has_agent(config: &str, name: &str) -> bool {
    match toml::from_str::<toml::Table>(config) {
        Ok(t) => t
            .get("agents")
            .and_then(|a| a.as_table())
            .is_some_and(|a| a.contains_key(name)),
        Err(_) => config
            .lines()
            .any(|l| l.trim() == format!("[agents.{name}]")),
    }
}

/// 是否用互動式選單（`--yes` 或不是終端機時直接採用預設答案）。
fn interactive(opts: &Options) -> bool {
    !opts.yes && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// 把內嵌的 Docker 檔案寫到 `<設定目錄>/docker/<sub>/`，回傳該目錄。
fn write_docker_files(config_dir: &Path, sub: &str) -> anyhow::Result<PathBuf> {
    let dir = config_dir.join("docker").join(sub);
    std::fs::create_dir_all(&dir).with_context(|| format!("無法建立 {}", dir.display()))?;
    for (s, name, content) in DOCKER_FILES {
        if *s == sub {
            // entrypoint.sh 在 Linux 容器內執行，一律使用 LF 換行
            std::fs::write(dir.join(name), content.replace("\r\n", "\n"))
                .with_context(|| format!("無法寫入 {}", dir.join(name).display()))?;
        }
    }
    Ok(dir)
}

fn image_exists(image: &str) -> bool {
    capture("docker", &["image", "inspect", image]).is_some_and(|(ok, _)| ok)
}

fn section(title: &str) {
    println!("\n{}", title.bold());
}

/// 一個項目的狀態：(符號, 說明)。
fn dep_status(env: &Env, dep: deps::Dep) -> (char, String) {
    use deps::Dep;
    let missing = |extra: &str| ('✗', format!("未安裝{extra}"));
    match dep {
        Dep::Git => match &env.git {
            Some(v) if deps::git_ok(v) => ('✓', format!("git {v}")),
            Some(v) => (
                '✗',
                format!("git {v}：版本太舊，需要 2.20 以上（建立 worktree 用）"),
            ),
            None => (
                '✗',
                "找不到。agt 需要 git 2.20 以上（建立 worktree 用）".into(),
            ),
        },
        Dep::DockerDesktop => match &env.docker {
            Docker::Running(v) => ('✓', format!("Docker {v}（執行中）")),
            Docker::NotRunning => ('!', "已安裝但沒有在執行".into()),
            Docker::Missing => missing("（Docker 容器版 Agent 需要）"),
        },
        Dep::Sbx => match &env.sbx {
            Sbx::Ready(v, true) => ('✓', format!("sbx {v}，已登入")),
            Sbx::Ready(v, false) => ('!', format!("sbx {v}，已登入，尚未設定網路政策")),
            Sbx::NotSignedIn(v) => ('!', format!("sbx {v}，尚未登入")),
            Sbx::Missing => missing("（claude-sbx 需要，microVM 隔離最完整）"),
        },
        Dep::Claude if env.claude => ('✓', "claude".into()),
        Dep::Claude => missing("（內建的 claude Agent 需要；沙箱版不需要）"),
        Dep::Copilot if env.copilot => ('✓', "copilot".into()),
        Dep::Copilot => missing(""),
        Dep::Codex if env.codex => ('✓', "codex".into()),
        Dep::Codex => missing(""),
        Dep::Agy if env.agy => ('✓', "agy".into()),
        Dep::Agy => missing(""),
    }
}

fn print_env(env: &Env) {
    section("檢查環境");
    for r in deps::REQUIREMENTS {
        let (mark, text) = dep_status(env, r.dep);
        // 有問題的項目標示等級（必要 / 建議 / 選用）
        let level = if mark == '✓' {
            String::new()
        } else {
            format!("[{}] ", r.level.label())
        };
        prompt::status(mark, &format!("{level}{}：{text}", r.name));
        if r.dep == deps::Dep::DockerDesktop && env.wsl == Some(false) {
            println!(
                "      {}",
                "WSL2：未啟用（Docker Desktop 需要，安裝 Docker Desktop 時會一併啟用）".dark_grey()
            );
        }
    }
}

/// 列出缺少的項目與安裝方式（不詢問時使用）。
fn print_missing(env: &Env) {
    let tasks = deps::plan(env, deps::Os::current());
    if tasks.is_empty() {
        return;
    }
    section("缺少的項目");
    for t in &tasks {
        let how = t
            .methods
            .iter()
            .find(|m| !matches!(m, deps::Method::Skip | deps::Method::Open(_)))
            .map(deps::Method::desc)
            .unwrap_or_default();
        println!(
            "  • [{}] {}：{how}",
            deps::requirement(t.dep).level.label(),
            t.title()
        );
    }
    println!(
        "  {}",
        "執行 agt setup 選擇安裝方式（或 agt setup --yes --install 用 winget 安裝必要項目）"
            .dark_grey()
    );
}

/// 處理缺少的項目：互動時顯示安裝選單；不詢問時只在 `--install` 下用 winget 安裝必要項目。
/// 回傳處理後的環境。
fn handle_missing(opts: &Options, ui: bool, env: Env) -> anyhow::Result<Env> {
    if ui {
        return deps::run_menu(env);
    }
    let auto = deps::auto_methods(&deps::plan(&env, deps::Os::current()), opts.install);
    let mut env = env;
    for (dep, method) in &auto {
        env = deps::apply(*dep, method, env)?;
    }
    print_missing(&env);
    Ok(env)
}

/// 每次開啟 agt 時的快速檢查：只看 PATH（不執行程式）有沒有 git。
pub fn has_git_quick() -> bool {
    on_path("git")
}

/// 缺少必要項目時呼叫：完整檢查並顯示安裝選單，回傳 git 是否可用。
pub fn install_missing() -> anyhow::Result<bool> {
    prompt::banner("✻ agt 環境檢查", "缺少必要的項目，安裝後才能開啟 agt");
    let env = detect();
    print_env(&env);
    let env = deps::run_menu(env)?;
    Ok(env.git.as_deref().is_some_and(deps::git_ok))
}

fn candidates(env: &Env) -> Vec<Candidate> {
    let mut list = vec![];
    if env.sbx != Sbx::Missing {
        list.push(Candidate {
            name: "claude-sbx",
            desc: "Docker Sandboxes（microVM）中的 Claude Code，隔離最完整，建議",
            image: None,
            default_on: true,
            extends: None,
        });
    }
    if env.docker != Docker::Missing {
        list.push(Candidate {
            name: "claude-docker",
            desc: "Docker 容器中的 Claude Code",
            image: Some(("agt-claude", "claude")),
            default_on: env.sbx == Sbx::Missing,
            extends: None,
        });
        list.push(Candidate {
            name: "agy-docker",
            desc: "Docker 容器中的 Antigravity CLI",
            image: Some(("agt-agy", "agy")),
            default_on: false,
            extends: None,
        });
        list.push(Candidate {
            name: "copilot-docker",
            desc: "Docker 容器中的 GitHub Copilot CLI",
            image: Some(("agt-copilot", "copilot")),
            default_on: false,
            extends: None,
        });
        list.push(Candidate {
            name: "codex-docker",
            desc: "Docker 容器中的 OpenAI Codex CLI",
            image: Some(("agt-codex", "codex")),
            default_on: false,
            extends: None,
        });
    }
    // advisor 版：Sonnet 執行、Opus 擔任顧問。預設不勾選（需要支援 --advisor 的 Claude Code）
    if env.claude {
        list.push(Candidate {
            name: "claude-advisor",
            desc: "主機上的 Claude Code，Sonnet 執行、Opus 擔任顧問（--advisor）",
            image: None,
            default_on: false,
            extends: Some("claude"),
        });
    }
    if env.sbx != Sbx::Missing {
        list.push(Candidate {
            name: "claude-sbx-advisor",
            desc: "claude-sbx，Sonnet 執行、Opus 擔任顧問（--advisor）",
            image: None,
            default_on: false,
            extends: Some("claude-sbx"),
        });
    }
    if env.docker != Docker::Missing {
        list.push(Candidate {
            name: "claude-docker-advisor",
            desc: "claude-docker，Sonnet 執行、Opus 擔任顧問（--advisor，使用 agt-claude 映像檔）",
            image: None,
            default_on: false,
            extends: Some("claude-docker"),
        });
    }
    list
}

/// [`with_bases`] 並顯示一併加入了哪些 Agent。
fn add_bases<'a>(
    all: &'a [Candidate],
    chosen: Vec<&'a Candidate>,
    existing: &str,
) -> Vec<&'a Candidate> {
    let picked: Vec<&str> = chosen.iter().map(|c| c.name).collect();
    let out = with_bases(all, chosen, existing);
    for c in out.iter().filter(|c| !picked.contains(&c.name)) {
        prompt::status('✓', &format!("{}：advisor 版需要，一併加入", c.name));
    }
    out
}

/// 選到的 Agent 用 `extends` 沿用的 Agent 不在設定檔中、也沒有選到時，一併加入（放在它前面），
/// 否則 daemon 載入設定時會因為找不到被繼承的 Agent 而失敗。
/// 內建的 Agent（例如 `claude`）不在候選清單中，不需要加入。
fn with_bases<'a>(
    all: &'a [Candidate],
    chosen: Vec<&'a Candidate>,
    existing: &str,
) -> Vec<&'a Candidate> {
    let mut out: Vec<&'a Candidate> = vec![];
    for c in chosen {
        if let Some(base) = c.extends
            && !has_agent(existing, base)
            && !out.iter().any(|o| o.name == base)
            && let Some(b) = all.iter().find(|b| b.name == base)
        {
            out.push(b);
        }
        if !out.iter().any(|o| o.name == c.name) {
            out.push(c);
        }
    }
    out
}

/// 把選到的 Agent 範例附加到設定檔（沒有設定檔時新建）。
fn write_config(path: &Path, existing: &str, chosen: &[&Candidate]) -> anyhow::Result<()> {
    let config_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("無法建立 {}", config_dir.display()))?;
    let mut add = String::new();
    if existing.is_empty() {
        add.push_str(
            "# agt 使用者設定：只需要寫要覆蓋的部分。由 `agt setup` 產生，可以自由修改。\n\
             # 所有選項見 agt 原始碼的 config/default.toml；重新執行 `agt setup` 可以加入其他 Agent。\n",
        );
    }
    for c in chosen {
        let block = example_block(c.name)
            .with_context(|| format!("內嵌的預設設定中找不到 {} 的範例", c.name))?;
        add.push('\n');
        add.push_str(&format!("# {}\n", c.desc));
        if let Some((image, sub)) = c.image {
            let dir = config_dir.join("docker").join(sub);
            add.push_str(&format!(
                "# 映像檔：docker build -t {image} \"{}\"\n",
                dir.display()
            ));
        }
        add.push_str(&block);
    }
    if add.is_empty() {
        return Ok(());
    }
    // 附加寫入（不取代檔案），daemon 的設定監看能偵測到變更
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("無法寫入 {}", path.display()))?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        f.write_all(b"\n")?;
    }
    f.write_all(add.as_bytes())?;
    Ok(())
}

pub fn run(opts: Options) -> anyhow::Result<()> {
    let path = config_path();
    let config_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let ui = interactive(&opts);
    if !std::io::stdout().is_terminal() {
        crossterm::style::force_color_output(false);
    }
    prompt::banner(
        "✻ 歡迎使用 agt 初始設定",
        "檢查並安裝需要的工具，產生設定檔與 Dockerfile",
    );
    let env = detect();
    print_env(&env);
    let env = handle_missing(&opts, ui, env)?;

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    section("設定檔");
    println!(
        "  {}{}",
        path.display(),
        if path.exists() {
            "（已存在，只會在最後面加入新的區段）".dark_grey()
        } else {
            "（新建）".dark_grey()
        }
    );

    // 選擇要加入的 Agent
    let all = candidates(&env);
    let mut fresh: Vec<&Candidate> = vec![];
    for c in &all {
        if has_agent(&existing, c.name) {
            prompt::status('✓', &format!("{}：設定檔中已有", c.name));
        } else {
            fresh.push(c);
        }
    }
    let chosen: Vec<&Candidate> = if fresh.is_empty() {
        if all.is_empty() {
            println!(
                "  {}",
                "沒有偵測到 Docker 或 Docker Sandboxes，只使用內建的 claude / antigravity / copilot / codex / shell。"
                    .dark_grey()
            );
        }
        vec![]
    } else if ui {
        println!();
        let items: Vec<Item> = fresh
            .iter()
            .map(|c| Item {
                label: c.name,
                desc: c.desc,
            })
            .collect();
        let defaults: Vec<bool> = fresh.iter().map(|c| c.default_on).collect();
        let on = prompt::multi_select("要加入哪些 Agent？", &items, &defaults)?;
        fresh
            .into_iter()
            .zip(on)
            .filter(|(_, o)| *o)
            .map(|(c, _)| c)
            .collect()
    } else {
        fresh.into_iter().filter(|c| c.default_on).collect()
    };
    let chosen = add_bases(&all, chosen, &existing);
    write_config(&path, &existing, &chosen)?;
    if !chosen.is_empty() {
        let names: Vec<&str> = chosen.iter().map(|c| c.name).collect();
        prompt::status('✓', &format!("已寫入設定檔：{}", names.join("、")));
    }
    after_choose(&opts, ui, &env, &chosen, &config_dir)?;
    println!(
        "\n{} {}",
        "✓ 設定完成。".green().bold(),
        "新增分頁時（Ctrl+A → n）用 ←/→ 切換 Agent；隨時可以執行 agt setup 加入其他 Agent。"
            .dark_grey()
    );
    Ok(())
}

/// 加入 Agent 之後的處理：寫入 Dockerfile 並建置映像檔、設定沙箱網路政策、列出接下來要做的事。
fn after_choose(
    opts: &Options,
    ui: bool,
    env: &Env,
    chosen: &[&Candidate],
    config_dir: &Path,
) -> anyhow::Result<()> {
    // Docker 映像檔
    for c in chosen {
        let Some((image, sub)) = c.image else {
            continue;
        };
        let dir = write_docker_files(config_dir, sub)?;
        section(&format!("映像檔 {image}"));
        prompt::status('✓', &format!("Dockerfile 已寫入 {}", dir.display()));
        let cmd = format!("docker build -t {image} \"{}\"", dir.display());
        if !matches!(env.docker, Docker::Running(_)) {
            prompt::status('!', &format!("Docker 沒有在執行，啟動後執行：{cmd}"));
            continue;
        }
        if image_exists(image) {
            prompt::status('✓', &format!("{image} 已存在（要更新時重新執行 {cmd}）"));
            continue;
        }
        let build = if ui {
            prompt::select(
                &format!("要現在建置 {image} 嗎？"),
                &[
                    Item {
                        label: "現在建置",
                        desc: "需要幾分鐘，會顯示建置過程",
                    },
                    Item {
                        label: "稍後再建置",
                        desc: "之後自行執行 docker build",
                    },
                ],
                0,
            )? == 0
        } else {
            opts.build
        };
        if build {
            let ok = Command::new("docker")
                .args(["build", "-t", image])
                .arg(&dir)
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                prompt::status('✓', &format!("{image} 建置完成"));
            } else {
                prompt::status('✗', &format!("建置失敗，請檢查上面的訊息後重新執行：{cmd}"));
            }
        } else {
            prompt::status('!', &format!("之後再建置：{cmd}"));
        }
    }

    // Docker Sandboxes 的網路政策
    let mut notes: Vec<String> = vec![];
    if chosen.iter().any(|c| c.name == "claude-sbx") {
        match &env.sbx {
            Sbx::NotSignedIn(_) => {
                notes.push("執行 sbx login 登入 Docker（claude-sbx 需要）".into())
            }
            Sbx::Ready(_, false) => {
                section("沙箱網路政策");
                println!(
                    "  {}",
                    "沙箱的對外連線都經過主機上的 proxy；沒有設定政策時無法建立沙箱。".dark_grey()
                );
                let picked = if ui {
                    prompt::select(
                        "沙箱可以連到哪些網站？",
                        &[
                            Item {
                                label: "balanced",
                                desc: "允許常見開發服務（npm、PyPI、GitHub、Anthropic API 等），建議",
                            },
                            Item {
                                label: "allow-all",
                                desc: "不限制對外連線",
                            },
                            Item {
                                label: "deny-all",
                                desc: "全部擋掉，每個網域都要自己允許",
                            },
                            Item {
                                label: "稍後再設定",
                                desc: "之後執行 sbx policy init",
                            },
                        ],
                        0,
                    )?
                } else {
                    3
                };
                let policy = ["balanced", "allow-all", "deny-all"].get(picked);
                match policy.map(|p| capture("sbx", &["policy", "init", p])) {
                    Some(Some((true, _))) => {
                        prompt::status('✓', &format!("網路政策：{}", policy.unwrap()))
                    }
                    _ => notes.push(
                        "執行 sbx policy init <balanced|allow-all|deny-all> 設定沙箱網路政策"
                            .into(),
                    ),
                }
            }
            _ => {}
        }
        notes.push(
            "claude-sbx：新分頁的 Claude 還沒登入，在分頁中輸入 /login，用 Ctrl+A → u 複製登入網址"
                .into(),
        );
    }
    if chosen.iter().any(|c| c.name == "claude-docker") {
        notes.push(
            "claude-docker：容器內無法用瀏覽器登入。在主機執行 claude setup-token，\
             把 token 設成環境變數 CLAUDE_CODE_OAUTH_TOKEN（或設定 ANTHROPIC_API_KEY）"
                .into(),
        );
    }
    if chosen.iter().any(|c| c.name == "copilot-docker") {
        notes.push(
            "copilot-docker：新分頁中輸入 /login，用 Ctrl+A → u 複製網址並在主機瀏覽器輸入代碼登入；\
             或在主機設定環境變數 GH_TOKEN（具備 Copilot Requests 權限的 fine-grained PAT）"
                .into(),
        );
    }
    if chosen.iter().any(|c| c.name == "codex-docker") {
        notes.push(
            "codex-docker：新分頁中執行 codex login --device-auth，在主機瀏覽器輸入代碼登入；\
             或在主機設定環境變數 OPENAI_API_KEY"
                .into(),
        );
    }
    if !env.git.as_deref().is_some_and(deps::git_ok) {
        notes.push("安裝 git 2.20 以上（建立 worktree 需要），安裝前無法開啟 agt".into());
    }
    if !notes.is_empty() {
        section("接下來");
        for n in notes {
            println!("  • {n}");
        }
    }
    Ok(())
}

/// 「不要再提醒」的 Agent 清單（一行一個名稱），放在設定檔旁邊。
fn skip_file() -> PathBuf {
    config_path().with_file_name("setup-skip.txt")
}

fn skipped() -> Vec<String> {
    std::fs::read_to_string(skip_file())
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// 只看 PATH（不執行任何程式），哪些 Agent 可能可以使用。每次啟動都會呼叫，必須很快。
/// 包含 advisor 版：沒加入的話開啟 agt 時會提醒一次（選「不要再提醒」後不再詢問）。
fn quick_available() -> Vec<&'static str> {
    let mut v = vec![];
    if on_path("claude") {
        v.push("claude-advisor");
    }
    if on_path("sbx") {
        v.extend(["claude-sbx", "claude-sbx-advisor"]);
    }
    if on_path("docker") {
        v.extend([
            "claude-docker",
            "agy-docker",
            "copilot-docker",
            "codex-docker",
            "claude-docker-advisor",
        ]);
    }
    v
}

/// 每次開啟 agt 時呼叫：偵測到新的可用 Agent（例如剛安裝 Docker Sandboxes），
/// 而且設定檔中還沒有、也沒有選過「不要再提醒」時，詢問是否加入。
/// 沒有新的 Agent 時只檢查 PATH，不會拖慢啟動。
pub fn offer_new_agents() -> anyhow::Result<()> {
    let path = config_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let skip = skipped();
    let pending: Vec<&str> = quick_available()
        .into_iter()
        .filter(|n| !has_agent(&existing, n) && !skip.iter().any(|s| s == n))
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    // 有新的候選才做完整偵測（會執行 docker / sbx）
    let env = detect();
    let all = candidates(&env);
    let fresh: Vec<&Candidate> = all.iter().filter(|c| pending.contains(&c.name)).collect();
    if fresh.is_empty() {
        return Ok(());
    }
    let opts = Options {
        yes: false,
        build: false,
        install: false,
    };
    println!(
        "{} {}",
        "✻".with(crossterm::style::Color::Rgb {
            r: 215,
            g: 119,
            b: 87
        }),
        "偵測到可以使用、但設定檔中還沒有的 Agent：".bold()
    );
    let items: Vec<Item> = fresh
        .iter()
        .map(|c| Item {
            label: c.name,
            desc: c.desc,
        })
        .collect();
    let defaults: Vec<bool> = fresh.iter().map(|c| c.default_on).collect();
    let on = prompt::multi_select("要加入哪些 Agent？", &items, &defaults)?;
    let mut chosen: Vec<&Candidate> = vec![];
    let mut rest: Vec<&Candidate> = vec![];
    for (c, o) in fresh.into_iter().zip(on) {
        if o {
            chosen.push(c);
        } else {
            rest.push(c);
        }
    }
    if !rest.is_empty() {
        let names: Vec<&str> = rest.iter().map(|c| c.name).collect();
        let again = prompt::select(
            &format!("沒有加入的 {} 下次還要提醒嗎？", names.join("、")),
            &[
                Item {
                    label: "下次再提醒",
                    desc: "",
                },
                Item {
                    label: "不要再提醒",
                    desc: "之後可以執行 agt setup 加入",
                },
            ],
            1,
        )?;
        if again == 1 {
            let mut list = skip;
            list.extend(names.iter().map(|s| s.to_string()));
            let _ = std::fs::create_dir_all(skip_file().parent().unwrap_or(Path::new(".")));
            std::fs::write(skip_file(), list.join("\n") + "\n")
                .with_context(|| format!("無法寫入 {}", skip_file().display()))?;
        }
    }
    if chosen.is_empty() {
        println!();
        return Ok(());
    }
    let chosen = add_bases(&all, chosen, &existing);
    write_config(&path, &existing, &chosen)?;
    let names: Vec<&str> = chosen.iter().map(|c| c.name).collect();
    prompt::status('✓', &format!("已寫入設定檔：{}", names.join("、")));
    let config_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    after_choose(&opts, true, &env, &chosen, &config_dir)?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_blocks_are_valid_toml() {
        for name in [
            "claude-sbx",
            "claude-docker",
            "agy-docker",
            "copilot-docker",
            "codex-docker",
        ] {
            let block = example_block(name).unwrap_or_else(|| panic!("找不到 {name}"));
            assert!(block.starts_with(&format!("[agents.{name}]\n")), "{block}");
            let v: toml::Value =
                toml::from_str(&block).unwrap_or_else(|e| panic!("{name}: {e}\n{block}"));
            assert!(v["agents"][name]["command"].is_str());
        }
        // agy-docker 的範例後面接著說明文字（單獨的 #），不能被收進來
        assert!(!example_block("agy-docker").unwrap().contains("copilot"));
        // copilot-docker 的範例後面接著說明文字（單獨的 #），不能被收進來
        assert!(
            !example_block("copilot-docker")
                .unwrap()
                .contains("任何 Agent")
        );
        assert!(example_block("nope").is_none());
        // advisor 版：只有 extends 與 extra_args，後面的範例不能被收進來
        for name in [
            "claude-advisor",
            "claude-sbx-advisor",
            "claude-docker-advisor",
        ] {
            let block = example_block(name).unwrap_or_else(|| panic!("找不到 {name}"));
            let v: toml::Value =
                toml::from_str(&block).unwrap_or_else(|e| panic!("{name}: {e}\n{block}"));
            let a = v["agents"][name].as_table().unwrap();
            assert_eq!(a.len(), 2, "{block}");
            assert!(a["extends"].is_str());
            assert!(a["extra_args"].as_array().unwrap().len() >= 2);
        }
    }

    #[test]
    fn advisor_adds_missing_base() {
        let env = Env {
            git: Some("2.44".into()),
            claude: true,
            agy: false,
            copilot: false,
            codex: false,
            docker: Docker::Running("29".into()),
            sbx: Sbx::Ready("v0.53.0".into(), true),
            wsl: None,
            winget: false,
        };
        let all = candidates(&env);
        let names = |v: &[&Candidate]| v.iter().map(|c| c.name).collect::<Vec<_>>();
        let pick = |ns: &[&str]| -> Vec<&Candidate> {
            ns.iter()
                .map(|n| all.iter().find(|c| c.name == *n).unwrap())
                .collect()
        };
        // 只選 advisor：被繼承的 claude-sbx 一併加入，放在前面
        let out = with_bases(&all, pick(&["claude-sbx-advisor"]), "");
        assert_eq!(names(&out), ["claude-sbx", "claude-sbx-advisor"]);
        // 兩個都選：不重複
        let out = with_bases(&all, pick(&["claude-sbx", "claude-sbx-advisor"]), "");
        assert_eq!(names(&out), ["claude-sbx", "claude-sbx-advisor"]);
        // 設定檔中已有 claude-docker：不再加入
        let existing = "[agents.claude-docker]\ncommand = \"docker\"\n";
        let out = with_bases(&all, pick(&["claude-docker-advisor"]), existing);
        assert_eq!(names(&out), ["claude-docker-advisor"]);
        // 繼承內建的 claude：不需要加入任何東西
        let out = with_bases(&all, pick(&["claude-advisor"]), "");
        assert_eq!(names(&out), ["claude-advisor"]);
        // 寫入的設定檔是合法的 TOML
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        let out = with_bases(&all, pick(&["claude-sbx-advisor", "claude-advisor"]), "");
        write_config(&path, "", &out).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(v["agents"]["claude-sbx-advisor"]["extends"].as_str(), Some("claude-sbx"));
        assert!(v["agents"]["claude-sbx"]["args"].is_array());
        assert_eq!(v["agents"]["claude-advisor"]["extends"].as_str(), Some("claude"));
    }

    #[test]
    fn detects_existing_agents() {
        let cfg = "[general]\n\n[agents.claude-sbx]\ncommand = \"sbx\"\n";
        assert!(has_agent(cfg, "claude-sbx"));
        assert!(!has_agent(cfg, "claude-docker"));
        assert!(!has_agent("# [agents.claude-docker]", "claude-docker"));
        // 引號、行尾註解、inline table 都要認得，否則會附加重複的區段
        assert!(has_agent(
            "[agents.\"claude-docker\"]\ncommand = \"docker\"\n",
            "claude-docker"
        ));
        assert!(has_agent(
            "[agents.claude-docker]  # 我的設定\ncommand = \"docker\"\n",
            "claude-docker"
        ));
        assert!(has_agent(
            "[agents]\nclaude-docker = { command = \"docker\" }\n",
            "claude-docker"
        ));
    }

    #[test]
    fn writes_config_once_with_blocks() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("agt").join("config.toml");
        let env = Env {
            git: Some("2.44".into()),
            claude: true,
            agy: false,
            copilot: false,
            codex: false,
            docker: Docker::Running("29".into()),
            sbx: Sbx::Ready("v0.53.0".into(), true),
            wsl: None,
            winget: false,
        };
        let all = candidates(&env);
        let chosen: Vec<&Candidate> = all
            .iter()
            .filter(|c| {
                c.name != "agy-docker" && c.name != "copilot-docker" && c.name != "codex-docker"
            })
            .collect();
        write_config(&path, "", &chosen).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: toml::Value = toml::from_str(&text).unwrap();
        assert!(v["agents"]["claude-sbx"]["on_close"].is_array());
        assert!(v["agents"]["claude-docker"]["args"].is_array());
        assert!(has_agent(&text, "claude-sbx") && !has_agent(&text, "agy-docker"));
        // 再加入一個：附加在後面，原本的內容保留
        let agy: Vec<&Candidate> = all.iter().filter(|c| c.name == "agy-docker").collect();
        write_config(&path, &text, &agy).unwrap();
        let text2 = std::fs::read_to_string(&path).unwrap();
        assert!(text2.starts_with(&text));
        let v2: toml::Value = toml::from_str(&text2).unwrap();
        assert!(v2["agents"]["agy-docker"].is_table());
    }

    #[test]
    fn docker_files_are_embedded() {
        let d = tempfile::tempdir().unwrap();
        let dir = write_docker_files(d.path(), "claude").unwrap();
        let df = std::fs::read_to_string(dir.join("Dockerfile")).unwrap();
        assert!(df.contains("COPY entrypoint.sh"));
        let ep = std::fs::read_to_string(dir.join("entrypoint.sh")).unwrap();
        assert!(ep.starts_with("#!/bin/sh") && !ep.contains('\r'));
        for sub in ["agy", "copilot", "codex"] {
            assert!(
                write_docker_files(d.path(), sub)
                    .unwrap()
                    .join("Dockerfile")
                    .exists()
            );
        }
    }
}
