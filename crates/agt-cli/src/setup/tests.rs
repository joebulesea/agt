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
    // copilot-docker 的範例後面接著 codex-docker 的範例，不能被收進來
    assert!(!example_block("copilot-docker").unwrap().contains("codex"));
    // codex-docker 的範例後面接著說明文字（單獨的 #），不能被收進來
    assert!(
        !example_block("codex-docker")
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
    assert_eq!(
        v["agents"]["claude-sbx-advisor"]["extends"].as_str(),
        Some("claude-sbx")
    );
    assert!(v["agents"]["claude-sbx"]["args"].is_array());
    assert_eq!(
        v["agents"]["claude-advisor"]["extends"].as_str(),
        Some("claude")
    );
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
