use std::fs;
#[cfg(target_os = "macos")]
use std::fs::OpenOptions;
#[cfg(target_os = "macos")]
use std::io::ErrorKind;
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
const LOCK_EX: i32 = 2;
#[cfg(target_os = "macos")]
const LOCK_NB: i32 = 4;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

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

#[test]
fn concurrent_use_waits_for_remove_and_cannot_reactivate_deleted_profile() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("real-claude");
    let remove_started = temp.path().join("remove-started");
    let release_remove = temp.path().join("release-remove");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2\" = \"auth logout\" ]; then\n\
               : > '{}'\n\
               attempts=0\n\
               while [ ! -e '{}' ] && [ \"$attempts\" -lt 500 ]; do\n\
                 attempts=$((attempts + 1))\n\
                 sleep 0.01\n\
               done\n\
               [ -e '{}' ] || exit 70\n\
               exit 0\n\
             fi\n\
             exit 0\n",
            remove_started.display(),
            release_remove.display(),
            release_remove.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    let shim = account_home.join("bin/claude");
    let work = account_home.join("profiles/work");
    let personal = account_home.join("profiles/personal");
    fs::create_dir_all(shim.parent().unwrap()).unwrap();
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&personal).unwrap();
    symlink(binary, &shim).unwrap();
    let state = serde_json::json!({
        "version": 1,
        "active": "personal",
        "real_claude": fake_claude,
        "profiles": {
            "personal": { "config_dir": personal, "created_at": 0 },
            "work": { "config_dir": work, "created_at": 0 }
        }
    });
    fs::write(
        account_home.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();

    let removing = Command::new(&shim)
        .env("CLAUDE_ACCOUNT_HOME", &account_home)
        .args(["account", "remove", "work"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let removing = ChildCleanupGuard::new(removing);
    let mut remove_release = FileReleaseGuard::new(release_remove);
    assert!(
        wait_until(Duration::from_secs(2), || remove_started.exists()),
        "remove did not reach Claude logout"
    );

    let selecting = Command::new(&shim)
        .env("CLAUDE_ACCOUNT_HOME", &account_home)
        .args(["account", "use", "work"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut selecting = ChildCleanupGuard::new(selecting);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        selecting.try_wait().unwrap().is_none(),
        "account use did not wait for the in-progress removal"
    );

    remove_release.release();
    let remove_output = removing.wait_with_output_bounded(Duration::from_secs(3));
    assert!(
        remove_output.status.success(),
        "remove failed: {}",
        String::from_utf8_lossy(&remove_output.stderr)
    );
    let use_output = selecting.wait_with_output_bounded(Duration::from_secs(3));
    assert!(!use_output.status.success());
    assert!(String::from_utf8_lossy(&use_output.stderr).contains("profile `work` does not exist"));

    let final_state: serde_json::Value =
        serde_json::from_slice(&fs::read(account_home.join("state.json")).unwrap()).unwrap();
    assert_eq!(final_state["active"], "personal");
    assert!(final_state["profiles"].get("work").is_none());
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
fn version_probe_timeout_terminates_direct_hang() {
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

    let mut probe_cleanup = ProcessCleanupGuard::new(probe_pid.clone());
    let (output, elapsed) = invoke_bounded(
        binary,
        &account_home,
        &["install", "--real", fake_claude.to_str().unwrap()],
        Duration::from_secs(5),
    );

    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("timed out querying Claude Code version"),
        "{error}"
    );
    assert!(error.contains("2.1.144 or later"), "{error}");

    let pid = probe_cleanup.pid();
    assert!(
        wait_until(Duration::from_secs(1), || !process_is_running(pid)),
        "direct version probe process {pid} remained alive after {elapsed:?}"
    );
    probe_cleanup.disarm();
}

#[cfg(target_os = "macos")]
#[test]
fn version_probe_timeout_covers_inherited_pipes_and_terminates_descendant() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("backgrounding-claude");
    let descendant_pid = temp.path().join("descendant.pid");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               /bin/sleep 60 2>/dev/null &\n\
               printf '%s\\n' \"$!\" > '{}'\n\
               exit 0\n\
             fi\n\
             exit 0\n",
            descendant_pid.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    let mut descendant_cleanup = ProcessCleanupGuard::new(descendant_pid.clone());
    let (output, elapsed) = invoke_bounded(
        binary,
        &account_home,
        &["install", "--real", fake_claude.to_str().unwrap()],
        Duration::from_secs(5),
    );

    assert!(!output.status.success());
    assert!(
        elapsed < Duration::from_secs(5),
        "version probe exceeded its bounded window: {elapsed:?}"
    );
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("timed out querying Claude Code version"),
        "{error}"
    );
    assert!(error.contains("2.1.144 or later"), "{error}");

    let pid = descendant_cleanup.pid();
    assert!(
        wait_until(Duration::from_secs(1), || !process_is_running(pid)),
        "version probe descendant {pid} remained alive after timeout"
    );
    descendant_cleanup.disarm();
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
               attempts=0\n\
               while [ ! -e '{}' ] && [ \"$attempts\" -lt 500 ]; do\n\
                 attempts=$((attempts + 1))\n\
                 sleep 0.01\n\
               done\n\
               [ -e '{}' ] || exit 70\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2 $3\" = \"auth status --json\" ]; then\n\
               printf '{{\"loggedIn\":true}}\\n'\n\
             fi\n\
             exit 0\n",
            login_calls.display(),
            first_login_started.display(),
            release_login.display(),
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
    let first = ChildCleanupGuard::new(first);
    let mut login_release = FileReleaseGuard::new(release_login.clone());
    assert!(
        wait_until(Duration::from_secs(2), || first_login_started.exists()),
        "first add did not reach auth login"
    );

    let reservation_file = account_home.join("profile-reservations/work.lock");
    assert_exclusively_locked(&reservation_file);

    let second = Command::new(binary)
        .env("CLAUDE_ACCOUNT_HOME", &account_home)
        .env("ANTHROPIC_API_KEY", "must-not-leak")
        .args(["account", "add", "work"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = ChildCleanupGuard::new(second);

    login_release.release();
    let first_output = first.wait_with_output_bounded(Duration::from_secs(3));
    let second_output = second.wait_with_output_bounded(Duration::from_secs(3));

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
    assert!(
        reservation_file.is_file(),
        "reservation file was removed after add completed"
    );

    let profiles = run(binary, &account_home, &["account", "list"]);
    assert_eq!(String::from_utf8(profiles.stdout).unwrap().trim(), "* Work");
}

#[cfg(target_os = "macos")]
fn assert_exclusively_locked(path: &Path) {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|error| panic!("failed to open reservation {}: {error}", path.display()));
    let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    assert_eq!(result, -1, "reservation {} was not locked", path.display());
    assert_eq!(
        std::io::Error::last_os_error().kind(),
        ErrorKind::WouldBlock,
        "reservation {} failed for an unexpected reason",
        path.display()
    );
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[cfg(target_os = "macos")]
#[test]
fn add_login_allows_unrelated_state_updates_and_merges_them() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let initial_claude = temp.path().join("initial-claude");
    let replacement_claude = temp.path().join("replacement-claude");
    let login_started = temp.path().join("login-started");
    let release_login = temp.path().join("release-login");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &initial_claude,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2\" = \"auth login\" ]; then\n\
               : > '{}'\n\
               attempts=0\n\
               while [ ! -e '{}' ] && [ \"$attempts\" -lt 500 ]; do\n\
                 attempts=$((attempts + 1))\n\
                 sleep 0.01\n\
               done\n\
               [ -e '{}' ] || exit 70\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2 $3\" = \"auth status --json\" ]; then\n\
               printf '{{\"loggedIn\":true}}\\n'\n\
             fi\n\
             exit 0\n",
            login_started.display(),
            release_login.display(),
            release_login.display()
        ),
    )
    .unwrap();
    fs::write(
        &replacement_claude,
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then\n\
           printf '2.1.144 (Claude Code)\\n'\n\
         fi\n\
         exit 0\n",
    )
    .unwrap();
    for fake in [&initial_claude, &replacement_claude] {
        fs::set_permissions(fake, fs::Permissions::from_mode(0o755)).unwrap();
    }

    run(
        binary,
        &account_home,
        &["install", "--real", initial_claude.to_str().unwrap()],
    );
    let work_dir = account_home.join("profiles/work");
    let personal_dir = account_home.join("profiles/personal");
    fs::create_dir_all(&work_dir).unwrap();
    fs::create_dir_all(&personal_dir).unwrap();
    let state_file = account_home.join("state.json");
    let mut seeded: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_file).unwrap()).unwrap();
    seeded["active"] = serde_json::json!("work");
    seeded["profiles"] = serde_json::json!({
        "personal": { "config_dir": personal_dir, "created_at": 0 },
        "work": { "config_dir": work_dir, "created_at": 0 }
    });
    fs::write(&state_file, serde_json::to_vec_pretty(&seeded).unwrap()).unwrap();

    let add = Command::new(binary)
        .env("CLAUDE_ACCOUNT_HOME", &account_home)
        .env("ANTHROPIC_API_KEY", "must-not-leak")
        .args(["account", "add", "new"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let add = ChildCleanupGuard::new(add);
    let mut login_release = FileReleaseGuard::new(release_login);
    assert!(
        wait_until(Duration::from_secs(2), || login_started.exists()),
        "add did not reach auth login"
    );

    let (use_output, _) = invoke_bounded(
        binary,
        &account_home,
        &["account", "use", "personal"],
        Duration::from_secs(2),
    );
    assert!(
        use_output.status.success(),
        "account use failed: {}",
        String::from_utf8_lossy(&use_output.stderr)
    );
    let (install_output, _) = invoke_bounded(
        binary,
        &account_home,
        &["install", "--real", replacement_claude.to_str().unwrap()],
        Duration::from_secs(3),
    );
    assert!(
        install_output.status.success(),
        "concurrent install failed: {}",
        String::from_utf8_lossy(&install_output.stderr)
    );

    login_release.release();
    let add_output = add.wait_with_output_bounded(Duration::from_secs(3));
    assert!(
        add_output.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add_output.stderr)
    );

    let final_state: serde_json::Value =
        serde_json::from_slice(&fs::read(state_file).unwrap()).unwrap();
    assert_eq!(final_state["active"], "personal");
    assert!(final_state["profiles"].get("work").is_some());
    assert!(final_state["profiles"].get("personal").is_some());
    assert!(final_state["profiles"].get("new").is_some());
    assert_eq!(
        final_state["real_claude"],
        serde_json::json!(replacement_claude)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn remove_purge_serializes_case_variant_add_until_deletion_finishes() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("real-claude");
    let remove_started = temp.path().join("remove-started");
    let release_remove = temp.path().join("release-remove");
    let login_started = temp.path().join("login-started");
    let events = temp.path().join("events.log");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));

    fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2\" = \"auth logout\" ]; then\n\
               printf 'logout\\n' >> '{}'\n\
               : > '{}'\n\
               attempts=0\n\
               while [ ! -e '{}' ] && [ \"$attempts\" -lt 500 ]; do\n\
                 attempts=$((attempts + 1))\n\
                 sleep 0.01\n\
               done\n\
               [ -e '{}' ] || exit 70\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2\" = \"auth login\" ]; then\n\
               printf 'login\\n' >> '{}'\n\
               : > '{}'\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2 $3\" = \"auth status --json\" ]; then\n\
               printf '{{\"loggedIn\":true}}\\n'\n\
             fi\n\
             exit 0\n",
            events.display(),
            remove_started.display(),
            release_remove.display(),
            release_remove.display(),
            events.display(),
            login_started.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    let shim = seed_installed_profile(&account_home, binary, &fake_claude, "Work");
    let old_data = account_home.join("profiles/Work/old-data");
    fs::write(&old_data, b"remove me\n").unwrap();

    let removing = Command::new(&shim)
        .env("CLAUDE_ACCOUNT_HOME", &account_home)
        .args(["account", "remove", "Work", "--force", "--purge", "--yes"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let removing = ChildCleanupGuard::new(removing);
    let mut remove_release = FileReleaseGuard::new(release_remove);
    assert!(
        wait_until(Duration::from_secs(2), || remove_started.exists()),
        "remove did not reach logout"
    );

    let reservation_file = account_home.join("profile-reservations/work.lock");
    assert_exclusively_locked(&reservation_file);

    let adding = Command::new(&shim)
        .env("CLAUDE_ACCOUNT_HOME", &account_home)
        .args(["account", "add", "work"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut adding = ChildCleanupGuard::new(adding);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        adding.try_wait().unwrap().is_none(),
        "case-variant add did not wait for removal"
    );
    assert!(
        !login_started.exists(),
        "case-variant add reached login before removal finished"
    );

    remove_release.release();
    let remove_output = removing.wait_with_output_bounded(Duration::from_secs(3));
    assert!(
        remove_output.status.success(),
        "remove failed: {}",
        String::from_utf8_lossy(&remove_output.stderr)
    );
    let add_output = adding.wait_with_output_bounded(Duration::from_secs(3));
    assert!(
        add_output.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add_output.stderr)
    );

    assert_eq!(fs::read_to_string(&events).unwrap(), "logout\nlogin\n");
    assert!(!old_data.exists(), "purge left old profile data behind");
    assert!(login_started.exists());
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(account_home.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["active"], "work");
    assert!(state["profiles"].get("Work").is_none());
    assert!(state["profiles"].get("work").is_some());
}

#[cfg(target_os = "macos")]
#[test]
fn legacy_case_collision_fails_closed_until_explicit_resolution() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("real-claude");
    let calls = temp.path().join("calls.log");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));
    let shim = seed_legacy_case_collision(&account_home, binary, &fake_claude, &calls, "work");
    let preserved_data = account_home.join("profiles/Work/preserve-me");

    for arguments in [
        &["account", "list"][..],
        &["account", "current"][..],
        &["account", "use", "Work"][..],
        &["account", "add", "personal"][..],
        &["account", "remove", "work", "--force"][..],
        &["forwarded", "command"][..],
    ] {
        let output = invoke(&shim, &account_home, arguments, &[]);
        assert!(
            !output.status.success(),
            "case-colliding state unexpectedly allowed: {}",
            arguments.join(" ")
        );
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains("claude account resolve-case-collision"),
            "{error}"
        );
    }
    assert!(!account_home.join("profiles/personal").exists());
    assert!(!calls.exists(), "normal commands invoked fake Claude");

    let recovery = run(
        &shim,
        &account_home,
        &["account", "resolve-case-collision", "work"],
    );
    let recovery_output = String::from_utf8(recovery.stdout).unwrap();
    assert!(
        recovery_output.contains("Local data and credentials were preserved"),
        "{recovery_output}"
    );
    assert!(
        recovery_output.contains("`Work` remains registered"),
        "{recovery_output}"
    );
    assert!(
        recovery_output.contains("claude account use Work"),
        "{recovery_output}"
    );
    assert_eq!(fs::read_to_string(&preserved_data).unwrap(), "keep\n");
    assert!(!calls.exists(), "recovery invoked fake Claude");

    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(account_home.join("state.json")).unwrap()).unwrap();
    assert!(state["profiles"].get("work").is_none());
    assert!(state["profiles"].get("Work").is_some());
    assert!(state["active"].is_null());

    let profiles = run(&shim, &account_home, &["account", "list"]);
    assert_eq!(String::from_utf8(profiles.stdout).unwrap(), "  Work\n");
    run(&shim, &account_home, &["account", "use", "Work"]);
    let current = run(&shim, &account_home, &["account", "current"]);
    assert_eq!(String::from_utf8(current.stdout).unwrap().trim(), "Work");
    assert!(
        !calls.exists(),
        "post-recovery state access invoked fake Claude"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn resolve_case_collision_preserves_a_different_active_profile() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("real-claude");
    let calls = temp.path().join("calls.log");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));
    seed_legacy_case_collision(&account_home, binary, &fake_claude, &calls, "Work");

    let recovery = run(binary, &account_home, &["resolve-case-collision", "work"]);

    let output = String::from_utf8(recovery.stdout).unwrap();
    assert!(output.contains("`Work` remains active"), "{output}");
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(account_home.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["active"], "Work");
    assert!(state["profiles"].get("work").is_none());
    assert!(state["profiles"].get("Work").is_some());
    assert!(!calls.exists(), "recovery invoked fake Claude");
    assert_eq!(
        fs::read_to_string(account_home.join("profiles/Work/preserve-me")).unwrap(),
        "keep\n"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn resolve_case_collision_refuses_a_non_colliding_profile() {
    let temp = tempfile::tempdir().unwrap();
    let account_home = temp.path().join("account-home");
    let fake_claude = temp.path().join("real-claude");
    let binary = Path::new(env!("CARGO_BIN_EXE_claude-account"));
    seed_installed_profile(&account_home, binary, &fake_claude, "work");

    let output = invoke(
        binary,
        &account_home,
        &["resolve-case-collision", "work"],
        &[],
    );

    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("does not have a case-colliding sibling"),
        "{error}"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(account_home.join("state.json")).unwrap()).unwrap();
    assert!(state["profiles"].get("work").is_some());
    assert_eq!(state["active"], "work");
    assert!(account_home.join("profiles/work").is_dir());
}

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
fn invoke_bounded(
    program: &Path,
    account_home: &Path,
    arguments: &[&str],
    timeout: Duration,
) -> (Output, Duration) {
    let child = Command::new(program)
        .env("CLAUDE_ACCOUNT_HOME", account_home)
        .env("ANTHROPIC_API_KEY", "must-not-leak")
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = ChildCleanupGuard::new(child);
    let started = Instant::now();
    let completed = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if started.elapsed() >= timeout {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let elapsed = started.elapsed();

    assert!(
        completed,
        "command did not terminate within {timeout:?}; cleanup guard terminated it"
    );

    (child.wait_with_output(), elapsed)
}

struct ChildCleanupGuard {
    child: Option<Child>,
}

impl ChildCleanupGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.as_mut().unwrap().try_wait()
    }

    #[cfg(target_os = "macos")]
    fn wait_with_output(mut self) -> Output {
        self.child.take().unwrap().wait_with_output().unwrap()
    }

    fn wait_with_output_bounded(mut self, timeout: Duration) -> Output {
        let started = Instant::now();
        while self.try_wait().unwrap().is_none() {
            assert!(
                started.elapsed() < timeout,
                "child did not terminate within {timeout:?}; cleanup guard terminated it"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        self.child.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for ChildCleanupGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct FileReleaseGuard {
    path: PathBuf,
    released: bool,
}

impl FileReleaseGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            released: false,
        }
    }

    fn release(&mut self) {
        fs::write(&self.path, b"release\n").unwrap();
        self.released = true;
    }
}

impl Drop for FileReleaseGuard {
    fn drop(&mut self) {
        if !self.released {
            let _ = fs::write(&self.path, b"release\n");
        }
    }
}

#[cfg(target_os = "macos")]
struct ProcessCleanupGuard {
    pid_file: PathBuf,
    pid: Option<u32>,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl ProcessCleanupGuard {
    fn new(pid_file: PathBuf) -> Self {
        Self {
            pid_file,
            pid: None,
            armed: true,
        }
    }

    fn pid(&mut self) -> u32 {
        if let Some(pid) = self.pid {
            return pid;
        }

        let pid = self
            .wait_for_recorded_pid(Duration::from_secs(1))
            .unwrap_or_else(|| {
                panic!(
                    "process did not record a valid pid in {}",
                    self.pid_file.display()
                )
            });
        self.pid = Some(pid);
        pid
    }

    fn wait_for_recorded_pid(&self, timeout: Duration) -> Option<u32> {
        let started = Instant::now();
        loop {
            if let Some(pid) = fs::read_to_string(&self.pid_file)
                .ok()
                .and_then(|value| value.trim().parse().ok())
            {
                return Some(pid);
            }
            if started.elapsed() >= timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(target_os = "macos")]
impl Drop for ProcessCleanupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let pid = self
            .pid
            .or_else(|| self.wait_for_recorded_pid(Duration::from_millis(250)));
        if let Some(pid) = pid {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = wait_until(Duration::from_secs(1), || !process_is_running(pid));
        }
    }
}

#[cfg(target_os = "macos")]
fn process_is_running(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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

#[cfg(target_os = "macos")]
fn seed_legacy_case_collision(
    account_home: &Path,
    binary: &Path,
    fake_claude: &Path,
    calls: &Path,
    active_profile: &str,
) -> std::path::PathBuf {
    let shim = account_home.join("bin/claude");
    let work_upper = account_home.join("profiles/Work");
    let work_lower = account_home.join("profiles/work");
    fs::create_dir_all(shim.parent().unwrap()).unwrap();
    fs::create_dir_all(&work_upper).unwrap();
    fs::write(work_upper.join("preserve-me"), "keep\n").unwrap();
    symlink(binary, &shim).unwrap();
    fs::write(
        fake_claude,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 89\n",
            calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(fake_claude, fs::Permissions::from_mode(0o755)).unwrap();
    let state = serde_json::json!({
        "version": 1,
        "active": active_profile,
        "real_claude": fake_claude,
        "profiles": {
            "Work": {
                "config_dir": work_upper,
                "created_at": 0
            },
            "work": {
                "config_dir": work_lower,
                "created_at": 0
            }
        }
    });
    fs::write(
        account_home.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
    shim
}
