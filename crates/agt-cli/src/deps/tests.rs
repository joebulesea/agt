use super::*;

fn env() -> Env {
    Env {
        git: Some("2.44.0.windows.1".into()),
        claude: true,
        agy: true,
        copilot: true,
        codex: true,
        docker: Docker::Running("29".into()),
        sbx: Sbx::Ready("v0.53.0".into(), true),
        wsl: Some(true),
        winget: true,
    }
}

#[test]
fn nothing_to_do_when_all_present() {
    assert!(plan(&env(), Os::Windows).is_empty());
}

#[test]
fn order_and_defaults() {
    let e = Env {
        git: None,
        claude: false,
        agy: false,
        copilot: false,
        codex: false,
        docker: Docker::Missing,
        sbx: Sbx::Missing,
        ..env()
    };
    let t = plan(&e, Os::Windows);
    let deps: Vec<Dep> = t.iter().map(|t| t.dep).collect();
    assert_eq!(
        deps,
        [
            Dep::Git,
            Dep::DockerDesktop,
            Dep::Sbx,
            Dep::Claude,
            Dep::Copilot,
            Dep::Codex,
            Dep::Agy
        ]
    );
    // 只有必要項目預設勾選
    let on: Vec<Dep> = t.iter().filter(|t| t.default_on).map(|t| t.dep).collect();
    assert_eq!(on, [Dep::Git]);
    // 每一項最後都可以略過
    assert!(t.iter().all(|t| t.methods.last() == Some(&Method::Skip)));
}

#[test]
fn winget_only_on_windows_with_winget() {
    let e = Env { git: None, ..env() };
    let first = |os, winget| {
        plan(
            &Env {
                winget,
                ..e.clone()
            },
            os,
        )[0]
        .methods
        .first()
        .cloned()
        .unwrap()
    };
    assert_eq!(first(Os::Windows, true), Method::Winget("Git.Git"));
    assert_eq!(
        first(Os::Windows, false),
        Method::Open("https://git-scm.com/downloads")
    );
    assert!(matches!(first(Os::Linux, true), Method::Open(_)));
    assert!(matches!(first(Os::Mac, false), Method::Open(_)));
    // Copilot 沒有 winget ID：只有開啟網頁與顯示指令
    let c = plan(
        &Env {
            copilot: false,
            ..env()
        },
        Os::Windows,
    );
    assert!(!c[0].methods.iter().any(|m| matches!(m, Method::Winget(_))));
    assert!(
        c[0].methods
            .contains(&Method::Manual("npm install -g @github/copilot"))
    );
}

#[test]
fn not_running_and_not_signed_in() {
    let e = Env {
        docker: Docker::NotRunning,
        sbx: Sbx::NotSignedIn("v0.53.0".into()),
        ..env()
    };
    let t = plan(&e, Os::Windows);
    assert_eq!(t[0].problem, Problem::NotRunning);
    assert_eq!(t[0].methods[0], Method::StartDocker);
    assert_eq!(t[1].methods, [Method::SbxLogin, Method::Skip]);
    // Linux 上不自動啟動 Docker
    let l = plan(&e, Os::Linux);
    assert!(!l[0].methods.contains(&Method::StartDocker));
}

#[test]
fn old_git_is_outdated() {
    assert!(git_ok("2.20.0") && git_ok("2.44.0.windows.1") && git_ok("3.0"));
    assert!(!git_ok("2.19.1") && !git_ok("1.9"));
    assert!(git_ok("?"));
    let t = plan(
        &Env {
            git: Some("2.17.1".into()),
            ..env()
        },
        Os::Windows,
    );
    assert_eq!(t[0].problem, Problem::Outdated("2.17.1".into()));
    assert!(t[0].default_on);
}

#[test]
fn yes_never_installs_without_install_flag() {
    let e = Env {
        git: None,
        sbx: Sbx::Missing,
        ..env()
    };
    let t = plan(&e, Os::Windows);
    assert!(auto_methods(&t, false).is_empty());
    // --install：只處理預設勾選（必要）的項目，只用 winget
    assert_eq!(
        auto_methods(&t, true),
        [(Dep::Git, Method::Winget("Git.Git"))]
    );
    // 沒有 winget 時什麼都不做
    let t = plan(&Env { winget: false, ..e }, Os::Windows);
    assert!(auto_methods(&t, true).is_empty());
}

#[test]
fn every_dep_has_requirement_and_manual() {
    for r in REQUIREMENTS {
        assert!(r.url.starts_with("https://"));
        for os in [Os::Windows, Os::Mac, Os::Linux] {
            assert!(!manual(r.dep, os).is_empty());
        }
    }
}
