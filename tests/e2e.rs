use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn run(program: &Path, account_home: &Path, arguments: &[&str]) -> Output {
    let output = Command::new(program)
        .env("CLAUDE_ACCOUNT_HOME", account_home)
        .env("ANTHROPIC_API_KEY", "must-not-leak")
        .args(arguments)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "command failed: {}\nstdout:\n{}\nstderr:\n{}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn complete_linux_profile_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("real-claude");
    let calls = temp.path().join("calls.log");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\n\
             printf '%s|%s|%s\\n' \"$CLAUDE_CONFIG_DIR\" \"$ANTHROPIC_API_KEY\" \"$*\" >> '{}'\n\
             if [ \"$1 $2 $3\" = \"auth status --json\" ]; then\n\
               printf '{{\"loggedIn\":true}}\\n'\n\
               exit 0\n\
             fi\n\
             if [ \"$1\" = \"auth\" ]; then exit 0; fi\n\
             printf 'forwarded:%s|config:%s\\n' \"$*\" \"$CLAUDE_CONFIG_DIR\"\n",
            calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    run(
        binary,
        &account_home,
        &["install", "--real", fake_claude.to_str().unwrap()],
    );
    let shim = account_home.join("bin/claude");
    assert!(shim.is_symlink());

    let first_add = run(&shim, &account_home, &["account", "add", "work"]);
    assert!(String::from_utf8_lossy(&first_add.stdout).contains("made it active"));
    let work_config: serde_json::Value =
        serde_json::from_slice(&fs::read(account_home.join("profiles/work/.claude.json")).unwrap())
            .unwrap();
    assert_eq!(work_config["hasCompletedOnboarding"], true);

    run(&shim, &account_home, &["account", "add", "personal"]);

    let profiles = run(&shim, &account_home, &["account", "list"]);
    let profiles = String::from_utf8(profiles.stdout).unwrap();
    assert!(profiles.contains("* work"));
    assert!(profiles.contains("  personal"));

    let current = run(&shim, &account_home, &["account", "current"]);
    assert_eq!(String::from_utf8(current.stdout).unwrap().trim(), "work");

    run(&shim, &account_home, &["account", "use", "personal"]);
    let current = run(&shim, &account_home, &["account", "current"]);
    assert_eq!(
        String::from_utf8(current.stdout).unwrap().trim(),
        "personal"
    );

    let forwarded = run(&shim, &account_home, &["fix this bug", "--model", "sonnet"]);
    let forwarded = String::from_utf8(forwarded.stdout).unwrap();
    assert!(forwarded.contains("forwarded:fix this bug --model sonnet"));
    assert!(forwarded.contains(account_home.join("profiles/personal").to_str().unwrap()));

    run(&shim, &account_home, &["account", "remove", "work"]);
    assert!(account_home.join("profiles/work").is_dir());

    run(
        &shim,
        &account_home,
        &[
            "account", "remove", "personal", "--force", "--purge", "--yes",
        ],
    );
    assert!(!account_home.join("profiles/personal").exists());

    let logged_calls = fs::read_to_string(calls).unwrap();
    assert!(logged_calls.contains("profiles/work||auth login"));
    assert!(logged_calls.contains("profiles/personal||auth login"));
    assert!(logged_calls.contains("profiles/personal||fix this bug --model sonnet"));
    assert!(
        !logged_calls.contains("must-not-leak"),
        "auth environment variable leaked to Claude"
    );
}
