use std::fs;
#[cfg(target_os = "macos")]
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::Stdio;
use std::process::{Command, Output};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

fn invoke(
    program: &Path,
    account_home: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(program);
    command
        .env("CLAUDE_ACCOUNT_HOME", account_home)
        .env("ANTHROPIC_API_KEY", "must-not-leak")
        .args(arguments);
    for &(name, value) in environment {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn run(program: &Path, account_home: &Path, arguments: &[&str]) -> Output {
    run_with_environment(program, account_home, arguments, &[])
}

fn run_with_environment(
    program: &Path,
    account_home: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> Output {
    let output = invoke(program, account_home, arguments, environment);

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
fn complete_profile_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("real-claude");
    let calls = temp.path().join("calls.log");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               exit 0\n\
             fi\n\
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

    let install = run(
        binary,
        &account_home,
        &["install", "--real", fake_claude.to_str().unwrap()],
    );
    let install_output = String::from_utf8(install.stdout).unwrap();
    assert!(install_output.contains("~/.zshrc"));
    assert!(install_output.contains("~/.bashrc"));
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

#[test]
fn help_names_supported_platforms() {
    let temp = tempfile::tempdir().unwrap();
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    let help = run(binary, temp.path(), &["--help"]);
    let help = String::from_utf8(help.stdout).unwrap();

    assert!(help.contains("Linux and macOS"));
}

#[cfg(target_os = "macos")]
#[test]
fn install_rejects_claude_without_profile_scoped_keychain_support() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("old-claude");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &fake_claude,
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then\n\
           printf '2.1.143 (Claude Code)\\n'\n\
         fi\n\
         exit 0\n",
    )
    .unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(binary)
        .env("CLAUDE_ACCOUNT_HOME", &account_home)
        .args(["install", "--real", fake_claude.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("2.1.144 or later"), "{error}");
}

#[cfg(target_os = "macos")]
#[test]
fn version_probe_times_out_and_reaps_hanging_claude() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("hanging-claude");
    let probe_pid = temp.path().join("probe.pid");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '%s\\n' \"$$\" > '{}'\n\
               exec /bin/sleep 60\n\
             fi\n\
             exit 0\n",
            probe_pid.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    let started = Instant::now();
    let output = invoke(
        binary,
        &account_home,
        &["install", "--real", fake_claude.to_str().unwrap()],
        &[],
    );
    let elapsed = started.elapsed();

    assert!(!output.status.success());
    assert!(
        elapsed < Duration::from_secs(5),
        "version probe took {elapsed:?}"
    );
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("timed out querying Claude Code version"),
        "{error}"
    );
    assert!(error.contains("2.1.144 or later"), "{error}");

    let pid = fs::read_to_string(&probe_pid).unwrap();
    let still_running = Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!still_running, "timed-out version probe was not reaped");
}

#[cfg(target_os = "macos")]
#[test]
fn old_claude_is_rejected_before_every_credential_path() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("old-claude");
    let calls = temp.path().join("calls.log");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));
    let shim = seed_installed_profile(&account_home, binary, &fake_claude, "work");

    fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{}'\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.143 (Claude Code)\\n'\n\
             fi\n\
             exit 0\n",
            calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    for arguments in [
        &["account", "add", "personal"][..],
        &["forwarded", "command"][..],
        &["account", "remove", "work", "--force"][..],
    ] {
        let output = invoke(&shim, &account_home, arguments, &[]);
        assert!(
            !output.status.success(),
            "old Claude unexpectedly ran: {}",
            arguments.join(" ")
        );
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("2.1.144 or later"), "{error}");
    }

    let logged_calls = fs::read_to_string(calls).unwrap();
    assert_eq!(
        logged_calls.lines().collect::<Vec<_>>(),
        vec!["--version", "--version", "--version"]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn managed_children_force_profile_scoped_secure_storage() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("real-claude");
    let calls = temp.path().join("calls.log");
    let inherited_shared_dir = temp.path().join("shared-secure-storage");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               exit 0\n\
             fi\n\
             printf '%s|%s|%s\\n' \"$CLAUDE_CONFIG_DIR\" \"$CLAUDE_SECURESTORAGE_CONFIG_DIR\" \"$*\" >> '{}'\n\
             if [ \"$1 $2 $3\" = \"auth status --json\" ]; then\n\
               printf '{{\"loggedIn\":true}}\\n'\n\
             fi\n\
             exit 0\n",
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
    let shared = inherited_shared_dir.to_str().unwrap();
    let preserve_and_shared = [
        ("CLAUDE_ACCOUNT_PRESERVE_AUTH_ENV", "1"),
        ("CLAUDE_SECURESTORAGE_CONFIG_DIR", shared),
    ];
    run_with_environment(
        &shim,
        &account_home,
        &["account", "add", "work"],
        &preserve_and_shared,
    );
    run_with_environment(
        &shim,
        &account_home,
        &["forwarded", "command"],
        &[
            ("CLAUDE_ACCOUNT_PRESERVE_AUTH_ENV", "1"),
            ("CLAUDE_SECURESTORAGE_CONFIG_DIR", ""),
        ],
    );
    run_with_environment(
        &shim,
        &account_home,
        &["account", "remove", "work", "--force"],
        &preserve_and_shared,
    );

    let profile_dir = account_home.join("profiles/work");
    let expected = profile_dir.to_str().unwrap();
    let logged_calls = fs::read_to_string(calls).unwrap();
    assert_eq!(
        logged_calls.lines().collect::<Vec<_>>(),
        vec![
            format!("{expected}|{expected}|auth login"),
            format!("{expected}|{expected}|auth status --json"),
            format!("{expected}|{expected}|forwarded command"),
            format!("{expected}|{expected}|auth logout"),
        ]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn concurrent_case_variant_adds_start_only_one_login() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("real-claude");
    let login_calls = temp.path().join("login-calls.log");
    let first_login_started = temp.path().join("first-login-started");
    let release_login = temp.path().join("release-login");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2\" = \"auth login\" ]; then\n\
               printf '%s\\n' \"$CLAUDE_CONFIG_DIR\" >> '{}'\n\
               : > '{}'\n\
               while [ ! -e '{}' ]; do sleep 0.01; done\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2 $3\" = \"auth status --json\" ]; then\n\
               printf '{{\"loggedIn\":true}}\\n'\n\
             fi\n\
             exit 0\n",
            login_calls.display(),
            first_login_started.display(),
            release_login.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    run(
        binary,
        &account_home,
        &["install", "--real", fake_claude.to_str().unwrap()],
    );

    let first = Command::new(binary)
        .env("CLAUDE_ACCOUNT_HOME", &account_home)
        .env("ANTHROPIC_API_KEY", "must-not-leak")
        .args(["account", "add", "Work"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_until(Duration::from_secs(2), || first_login_started.exists()),
        "first add did not reach auth login"
    );

    let second = Command::new(binary)
        .env("CLAUDE_ACCOUNT_HOME", &account_home)
        .env("ANTHROPIC_API_KEY", "must-not-leak")
        .args(["account", "add", "work"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second_reached_login = wait_until(Duration::from_secs(2), || {
        fs::read_to_string(&login_calls)
            .map(|calls| calls.lines().count() > 1)
            .unwrap_or(false)
    });

    fs::write(&release_login, b"release\n").unwrap();
    let first_output = first.wait_with_output().unwrap();
    let second_output = second.wait_with_output().unwrap();

    assert!(
        !second_reached_login,
        "both case-variant adds reached auth login before the first registered:\n{}",
        fs::read_to_string(&login_calls).unwrap()
    );
    let logged_logins = fs::read_to_string(&login_calls).unwrap();
    assert_eq!(
        logged_logins.lines().collect::<Vec<_>>(),
        vec![account_home.join("profiles/Work").to_str().unwrap()]
    );
    assert!(
        first_output.status.success(),
        "first add failed: {}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    assert!(!second_output.status.success());
    let second_error = String::from_utf8(second_output.stderr).unwrap();
    assert!(
        second_error.contains("differs only by letter case"),
        "{second_error}"
    );

    let profiles = run(binary, &account_home, &["account", "list"]);
    assert_eq!(String::from_utf8(profiles.stdout).unwrap().trim(), "* Work");
}

#[cfg(target_os = "macos")]
fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    predicate()
}

#[cfg(target_os = "macos")]
fn seed_installed_profile(
    account_home: &Path,
    binary: &Path,
    fake_claude: &Path,
    profile_name: &str,
) -> std::path::PathBuf {
    let shim = account_home.join("bin/claude");
    let profile_dir = account_home.join("profiles").join(profile_name);
    fs::create_dir_all(shim.parent().unwrap()).unwrap();
    fs::create_dir_all(&profile_dir).unwrap();
    symlink(binary, &shim).unwrap();
    let mut profiles = serde_json::Map::new();
    profiles.insert(
        profile_name.to_owned(),
        serde_json::json!({
            "config_dir": profile_dir,
            "created_at": 0
        }),
    );
    let state = serde_json::json!({
        "version": 1,
        "active": profile_name,
        "real_claude": fake_claude,
        "profiles": profiles
    });
    fs::write(
        account_home.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
    shim
}
