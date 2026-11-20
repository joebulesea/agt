//! 開啟 agt 時提醒加入新偵測到的 Agent（含「不要再提醒」清單）。

use super::*;

/// 「不要再提醒」的 Agent 清單（一行一個名稱），放在設定檔旁邊。
pub(super) fn skip_file() -> PathBuf {
    config_path().with_file_name("setup-skip.txt")
}

pub(super) fn skipped() -> Vec<String> {
    std::fs::read_to_string(skip_file())
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// 只看 PATH（不執行任何程式），哪些 Agent 可能可以使用。每次啟動都會呼叫，必須很快。
/// 包含 advisor 版：沒加入的話開啟 agt 時會提醒一次（選「不要再提醒」後不再詢問）。
pub(super) fn quick_available() -> Vec<&'static str> {
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
