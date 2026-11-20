//! 設定檔中的 Agent 區段：偵測結果轉成可勾選的項目、沿用的 Agent、寫入設定檔。

use super::*;

pub(super) fn candidates(env: &Env) -> Vec<Candidate> {
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
pub(super) fn add_bases<'a>(
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
pub(super) fn with_bases<'a>(
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
pub(super) fn write_config(
    path: &Path,
    existing: &str,
    chosen: &[&Candidate],
) -> anyhow::Result<()> {
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
