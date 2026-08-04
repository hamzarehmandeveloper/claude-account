use std::env;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::paths::AppPaths;
use crate::process;
use crate::state::{self, Profile, ProfileReservation, StateLock};

#[derive(Debug, Parser)]
#[command(
    name = "claude account",
    version,
    about = "Manage isolated Claude Code accounts across Linux and macOS"
)]
pub struct AccountCli {
    #[command(subcommand)]
    command: AccountCommand,
}

#[derive(Debug, Subcommand)]
enum AccountCommand {
    /// Create a profile and open Claude Code's normal login flow
    Add {
        /// Profile name, such as work or personal
        name: String,
        /// Pre-fill the email address in Claude's login flow
        #[arg(long)]
        email: Option<String>,
        /// Force SSO authentication
        #[arg(long)]
        sso: bool,
        /// Authenticate with Anthropic Console instead of a subscription
        #[arg(long)]
        console: bool,
    },
    /// Select the profile used by future Claude processes
    Use { name: String },
    /// List registered profiles
    List,
    /// Print only the active profile name
    Current,
    /// Safely unregister one exact name from legacy case-colliding state
    ResolveCaseCollision {
        /// Exact profile name to unregister
        name: String,
    },
    /// Log out and unregister a profile
    Remove {
        name: String,
        /// Also delete settings, sessions, plugins, and history
        #[arg(long, requires = "yes")]
        purge: bool,
        /// Confirm permanent deletion with --purge
        #[arg(long)]
        yes: bool,
        /// Allow removing the active profile
        #[arg(long)]
        force: bool,
    },
    /// Install the transparent `claude` shim
    Install {
        /// Absolute path to the real Claude Code executable
        #[arg(long)]
        real: Option<PathBuf>,
    },
}

impl AccountCli {
    pub fn run(self, paths: &AppPaths) -> Result<()> {
        match self.command {
            AccountCommand::Add {
                name,
                email,
                sso,
                console,
            } => add(paths, &name, email.as_deref(), sso, console),
            AccountCommand::Use { name } => use_profile(paths, &name),
            AccountCommand::List => list(paths),
            AccountCommand::Current => current(paths),
            AccountCommand::ResolveCaseCollision { name } => resolve_case_collision(paths, &name),
            AccountCommand::Remove {
                name,
                purge,
                yes: _,
                force,
            } => remove(paths, &name, purge, force),
            AccountCommand::Install { real } => install(paths, real.as_deref()),
        }
    }
}

fn add(paths: &AppPaths, name: &str, email: Option<&str>, sso: bool, console: bool) -> Result<()> {
    validate_profile_name(name)?;
    let _profile_reservation = ProfileReservation::acquire(paths, name)?;
    let current_executable = env::current_exe().context("failed to locate this executable")?;
    let initial_real_claude = {
        let _state_lock = StateLock::acquire(paths)?;
        let state = state::load(paths)?;
        if state.profiles.contains_key(name) {
            bail!("profile `{name}` already exists");
        }
        validate_new_profile_name(&state, name)?;
        state.real_claude.clone()
    };

    let real_claude =
        process::resolve_real_claude(initial_real_claude.as_deref(), &current_executable, paths)?;
    process::validate_platform_support(&real_claude)?;
    let profile_dir = paths.profile_dir(name);
    state::ensure_private_dir(&profile_dir)?;

    println!("Logging in profile `{name}` using Claude Code...");
    let mut login = process::managed_command(&real_claude, &profile_dir);
    login.args(["auth", "login"]);
    if let Some(email) = email {
        login.args(["--email", email]);
    }
    if sso {
        login.arg("--sso");
    }
    if console {
        login.arg("--console");
    }
    let login_status = login.status().context("failed to start Claude login")?;
    if !login_status.success() {
        bail!(
            "Claude login failed for `{name}`; the profile directory was preserved so you can retry"
        );
    }

    let verification = process::managed_command(&real_claude, &profile_dir)
        .args(["auth", "status", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .context("failed to verify Claude login")?;
    if !verification.status.success() {
        bail!("Claude did not report a valid login for profile `{name}`");
    }
    let auth_status: AuthStatus = serde_json::from_slice(&verification.stdout)
        .context("Claude returned an invalid response from `auth status --json`")?;
    if !auth_status.logged_in {
        bail!("Claude did not report a valid login for profile `{name}`");
    }

    complete_claude_onboarding(&profile_dir)?;

    let first_profile;
    {
        let _state_lock = StateLock::acquire(paths)?;
        let mut state = state::load(paths)?;
        if state.profiles.contains_key(name) {
            bail!("profile `{name}` was added by another process");
        }
        validate_new_profile_name(&state, name)?;

        first_profile = state.profiles.is_empty();
        if state.real_claude == initial_real_claude {
            state.real_claude = Some(real_claude);
        }
        state
            .profiles
            .insert(name.to_owned(), Profile::new(profile_dir));
        if first_profile {
            state.active = Some(name.to_owned());
        }
        state::save(paths, &state)?;
    }

    if first_profile {
        println!("Added `{name}` and made it active.");
    } else {
        println!("Added `{name}`. Activate it with `claude account use {name}`.");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct AuthStatus {
    #[serde(rename = "loggedIn")]
    logged_in: bool,
}

fn complete_claude_onboarding(profile_dir: &Path) -> Result<()> {
    let config_path = profile_dir.join(".claude.json");
    let mut config = match fs::read(&config_path) {
        Ok(contents) => serde_json::from_slice::<Value>(&contents)
            .with_context(|| format!("failed to parse {}", config_path.display()))?,
        Err(error) if error.kind() == ErrorKind::NotFound => Value::Object(Map::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", config_path.display()));
        }
    };
    let config = config
        .as_object_mut()
        .with_context(|| format!("{} must contain a JSON object", config_path.display()))?;
    config.insert("hasCompletedOnboarding".to_owned(), Value::Bool(true));

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = profile_dir.join(format!(".claude.json.tmp.{}.{}", std::process::id(), nonce));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        serde_json::to_writer_pretty(&mut file, &config)
            .context("failed to serialize Claude onboarding state")?;
        file.write_all(b"\n")
            .context("failed to finish Claude onboarding state")?;
        file.sync_all()
            .context("failed to sync Claude onboarding state")?;
        fs::rename(&temporary, &config_path)
            .with_context(|| format!("failed to update {}", config_path.display()))?;
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to protect {}", config_path.display()))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn use_profile(paths: &AppPaths, name: &str) -> Result<()> {
    validate_profile_name(name)?;
    let _profile_reservation = ProfileReservation::acquire(paths, name)?;
    let _lock = StateLock::acquire(paths)?;
    let mut state = state::load(paths)?;
    if !state.profiles.contains_key(name) {
        bail!("profile `{name}` does not exist");
    }
    state.active = Some(name.to_owned());
    state::save(paths, &state)?;
    println!("Now using `{name}` for new Claude processes.");
    Ok(())
}

fn list(paths: &AppPaths) -> Result<()> {
    let state = state::load(paths)?;
    if state.profiles.is_empty() {
        println!("No profiles. Add one with `claude account add NAME`.");
        return Ok(());
    }
    for name in state.profiles.keys() {
        let marker = if state.active.as_deref() == Some(name) {
            "*"
        } else {
            " "
        };
        println!("{marker} {name}");
    }
    Ok(())
}

fn current(paths: &AppPaths) -> Result<()> {
    let state = state::load(paths)?;
    match state.active {
        Some(name) => {
            println!("{name}");
            Ok(())
        }
        None => bail!("no active profile"),
    }
}

fn resolve_case_collision(paths: &AppPaths, name: &str) -> Result<()> {
    validate_profile_name(name)?;
    let _profile_reservation = ProfileReservation::acquire(paths, name)?;
    let _lock = StateLock::acquire(paths)?;
    let mut state = state::load_for_case_collision_resolution(paths)?;
    let profile = state
        .profiles
        .get(name)
        .cloned()
        .with_context(|| format!("profile `{name}` does not exist"))?;
    state
        .case_colliding_profile_name(name)
        .with_context(|| format!("profile `{name}` does not have a case-colliding sibling"))?;

    state.profiles.remove(name);
    let removed_was_active = state.active.as_deref() == Some(name);
    if removed_was_active {
        state.active = None;
    }
    let remaining_case_variants: Vec<String> = state
        .profiles
        .keys()
        .filter(|existing| existing.eq_ignore_ascii_case(name))
        .cloned()
        .collect();
    state::save(paths, &state)?;

    println!("Unregistered exact profile name `{name}` from account state.");
    println!(
        "Local data and credentials were preserved; Claude logout was not run and {} was not deleted.",
        profile.config_dir.display()
    );
    if let [survivor] = remaining_case_variants.as_slice() {
        println!("`{survivor}` remains registered.");
        if removed_was_active {
            println!(
                "Finish recovery with `claude account use {survivor}` if it should be active."
            );
        } else if state.active.as_deref() == Some(survivor.as_str()) {
            println!("`{survivor}` remains active; normal commands can resume.");
        } else {
            println!(
                "Normal commands can resume. Run `claude account use {survivor}` to activate the surviving profile."
            );
        }
    } else {
        let next = &remaining_case_variants[0];
        println!(
            "Case-colliding profiles still remain. Run `claude account resolve-case-collision {next}` again, leaving exactly one spelling registered."
        );
    }
    Ok(())
}

fn remove(paths: &AppPaths, name: &str, purge: bool, force: bool) -> Result<()> {
    remove_with_purge(paths, name, purge, force, |path| {
        fs::remove_dir_all(path).with_context(|| format!("failed to purge {}", path.display()))
    })
}

fn remove_with_purge(
    paths: &AppPaths,
    name: &str,
    purge: bool,
    force: bool,
    purge_directory: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    validate_profile_name(name)?;
    let _profile_reservation = ProfileReservation::acquire(paths, name)?;
    let (profile, real_claude) = {
        let _lock = StateLock::acquire(paths)?;
        let state = state::load(paths)?;
        let profile = state
            .profiles
            .get(name)
            .cloned()
            .with_context(|| format!("profile `{name}` does not exist"))?;
        let real_claude = state
            .real_claude
            .clone()
            .context("real Claude executable is not configured")?;
        let is_active = state.active.as_deref() == Some(name);
        if is_active && !force {
            bail!(
                "`{name}` is active; switch profiles first, or pass --force to leave no active profile"
            );
        }
        (profile, real_claude)
    };

    process::validate_platform_support(&real_claude)?;
    println!("Logging out profile `{name}`...");
    let logout_status = process::managed_command(&real_claude, &profile.config_dir)
        .args(["auth", "logout"])
        .status()
        .context("failed to start Claude logout")?;
    if !logout_status.success() {
        bail!("Claude logout failed; profile `{name}` was not removed");
    }

    {
        let _lock = StateLock::acquire(paths)?;
        let mut state = state::load(paths)?;
        state.profiles.remove(name);
        if state.active.as_deref() == Some(name) {
            state.active = None;
        }
        state::save(paths, &state)?;
    }

    if purge {
        let expected = paths.profile_dir(name);
        if profile.config_dir != expected {
            bail!(
                "refusing to purge unexpected directory {}; expected {}",
                profile.config_dir.display(),
                expected.display()
            );
        }
        let metadata = fs::symlink_metadata(&expected)
            .with_context(|| format!("failed to inspect {}", expected.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("refusing to purge a symlink or non-directory");
        }
        purge_directory(&expected)?;
        println!("Removed `{name}` and permanently deleted its local data.");
    } else {
        println!(
            "Removed `{name}`. Its non-credential data remains at {}.",
            profile.config_dir.display()
        );
    }
    Ok(())
}

fn install(paths: &AppPaths, explicit_real: Option<&Path>) -> Result<()> {
    let current_executable = env::current_exe().context("failed to locate this executable")?;
    let configured = {
        let _lock = StateLock::acquire(paths)?;
        state::load(paths)?.real_claude
    };
    let real_claude = match explicit_real {
        Some(path) => {
            if !path.is_absolute() {
                bail!("--real must be an absolute path");
            }
            process::validate_executable(path)?;
            path.to_path_buf()
        }
        None => process::resolve_real_claude(configured.as_deref(), &current_executable, paths)?,
    };
    process::validate_platform_support(&real_claude)?;

    state::ensure_private_dir(&paths.data_dir)?;
    state::ensure_private_dir(&paths.shim_dir)?;
    let libexec_dir = paths
        .installed_executable
        .parent()
        .context("invalid installation path")?;
    state::ensure_private_dir(libexec_dir)?;

    let same_executable = fs::canonicalize(&current_executable).ok()
        == fs::canonicalize(&paths.installed_executable).ok();
    if !same_executable {
        let temporary = paths
            .installed_executable
            .with_extension(format!("tmp.{}", std::process::id()));
        let mut destination = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        let mut source = fs::File::open(&current_executable)
            .with_context(|| format!("failed to open {}", current_executable.display()))?;
        std::io::copy(&mut source, &mut destination).context("failed to install executable")?;
        destination
            .sync_all()
            .context("failed to sync executable")?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
        fs::rename(&temporary, &paths.installed_executable)
            .context("failed to activate installed executable")?;
    }

    if let Ok(metadata) = fs::symlink_metadata(&paths.shim) {
        let points_to_us = metadata.file_type().is_symlink()
            && fs::canonicalize(&paths.shim).ok()
                == fs::canonicalize(&paths.installed_executable).ok();
        if !points_to_us {
            bail!(
                "refusing to replace existing non-managed path {}",
                paths.shim.display()
            );
        }
    }

    let temporary_shim = paths
        .shim
        .with_extension(format!("tmp.{}", std::process::id()));
    let _ = fs::remove_file(&temporary_shim);
    symlink(&paths.installed_executable, &temporary_shim)
        .context("failed to create Claude shim")?;
    fs::rename(&temporary_shim, &paths.shim).context("failed to activate Claude shim")?;

    {
        let _lock = StateLock::acquire(paths)?;
        let mut state = state::load(paths)?;
        state.real_claude = Some(real_claude.clone());
        state::save(paths, &state)?;
    }

    println!("Installed claude-account.");
    println!("Real Claude: {}", real_claude.display());
    println!("Shim: {}", paths.shim.display());
    println!();
    println!(
        "Add this line to your shell startup file (for example, ~/.zshrc or ~/.bashrc), then open a new terminal:"
    );
    println!("export PATH=\"{}:$PATH\"", paths.shim_dir.display());
    Ok(())
}

fn validate_new_profile_name(state: &state::State, name: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    if let Some(existing) = state.case_colliding_profile_name(name) {
        bail!(
            "profile `{name}` differs only by letter case from existing profile `{existing}`; choose another name on macOS"
        );
    }

    #[cfg(not(target_os = "macos"))]
    let _ = (state, name);

    Ok(())
}

fn validate_profile_name(name: &str) -> Result<()> {
    let mut characters = name.chars();
    let first = characters.next().context("profile name cannot be empty")?;
    if !first.is_ascii_alphanumeric()
        || !characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
        || name.len() > 32
    {
        bail!(
            "invalid profile name `{name}`; use 1-32 letters, numbers, hyphens, or underscores, \
             starting with a letter or number"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::os::fd::AsRawFd;

    #[cfg(target_os = "macos")]
    const LOCK_EX: i32 = 2;
    #[cfg(target_os = "macos")]
    const LOCK_NB: i32 = 4;

    #[cfg(target_os = "macos")]
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }

    #[test]
    fn profile_name_validation_blocks_path_traversal() {
        for invalid in ["", "../work", ".work", "work space", "work/personal"] {
            assert!(validate_profile_name(invalid).is_err(), "{invalid}");
        }
        for valid in ["work", "personal-2", "team_account"] {
            assert!(validate_profile_name(valid).is_ok(), "{valid}");
        }
    }

    #[test]
    fn add_uses_an_isolated_config_directory() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(temp.path().join("config"), temp.path().join("data"));
        let fake_claude = temp.path().join("claude-real");
        let log = temp.path().join("calls.log");
        let mut script = fs::File::create(&fake_claude).unwrap();
        writeln!(
            script,
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               exit 0\n\
             fi\n\
             printf '%s|%s\\n' \"$CLAUDE_CONFIG_DIR\" \"$*\" >> '{}'\n\
             if [ \"$1 $2 $3\" = \"auth status --json\" ]; then\n\
               printf '{{\"loggedIn\":true}}\\n'\n\
             fi\n\
             exit 0",
            log.display()
        )
        .unwrap();
        drop(script);
        fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

        {
            let _lock = StateLock::acquire(&paths).unwrap();
            let mut initial = state::load(&paths).unwrap();
            initial.real_claude = Some(fake_claude);
            state::save(&paths, &initial).unwrap();
        }

        add(&paths, "work", None, false, false).unwrap();
        let calls = fs::read_to_string(log).unwrap();
        let expected = paths.profile_dir("work").display().to_string();
        assert!(calls.contains(&format!("{expected}|auth login")));
        assert!(calls.contains(&format!("{expected}|auth status --json")));
        let claude_config: Value = serde_json::from_slice(
            &fs::read(paths.profile_dir("work").join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(claude_config["hasCompletedOnboarding"], true);
        assert_eq!(state::load(&paths).unwrap().active.as_deref(), Some("work"));
    }

    #[test]
    fn onboarding_update_preserves_existing_claude_state() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        state::ensure_private_dir(&profile).unwrap();
        let config_path = profile.join(".claude.json");
        fs::write(
            &config_path,
            r#"{"existing":{"setting":"preserved"},"hasCompletedOnboarding":false}"#,
        )
        .unwrap();

        complete_claude_onboarding(&profile).unwrap();

        let updated: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        assert_eq!(updated["existing"]["setting"], "preserved");
        assert_eq!(updated["hasCompletedOnboarding"], true);
        assert_eq!(
            fs::metadata(config_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn add_rejects_successful_command_that_reports_logged_out() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(temp.path().join("config"), temp.path().join("data"));
        let fake_claude = temp.path().join("claude-real");
        fs::write(
            &fake_claude,
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2 $3\" = \"auth status --json\" ]; then\n\
               printf '{\"loggedIn\":false}\\n'\n\
             fi\n\
             exit 0\n",
        )
        .unwrap();
        fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

        {
            let _lock = StateLock::acquire(&paths).unwrap();
            let mut initial = state::load(&paths).unwrap();
            initial.real_claude = Some(fake_claude);
            state::save(&paths, &initial).unwrap();
        }

        let error = add(&paths, "work", None, false, false).unwrap_err();
        assert!(error.to_string().contains("did not report a valid login"));
        assert!(!state::load(&paths).unwrap().profiles.contains_key("work"));
        assert!(!paths.profile_dir("work").join(".claude.json").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn add_rejects_profile_name_that_differs_only_by_case() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(temp.path().join("config"), temp.path().join("data"));
        let fake_claude = temp.path().join("claude-real");
        fs::write(
            &fake_claude,
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
               exit 0\n\
             fi\n\
             if [ \"$1 $2 $3\" = \"auth status --json\" ]; then\n\
               printf '{\"loggedIn\":true}\\n'\n\
             fi\n\
             exit 0\n",
        )
        .unwrap();
        fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

        {
            let _lock = StateLock::acquire(&paths).unwrap();
            let mut initial = state::load(&paths).unwrap();
            initial.real_claude = Some(fake_claude);
            initial
                .profiles
                .insert("Work".to_owned(), Profile::new(paths.profile_dir("Work")));
            initial.active = Some("Work".to_owned());
            state::save(&paths, &initial).unwrap();
        }

        let error = add(&paths, "work", None, false, false).unwrap_err();
        assert!(error.to_string().contains("differs only by letter case"));
        assert_eq!(state::load(&paths).unwrap().profiles.len(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn remove_holds_profile_reservation_during_purge() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(temp.path().join("config"), temp.path().join("data"));
        let fake_claude = temp.path().join("claude-real");
        fs::write(
            &fake_claude,
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               printf '2.1.144 (Claude Code)\\n'\n\
             fi\n\
             exit 0\n",
        )
        .unwrap();
        fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

        let profile_dir = paths.profile_dir("Work");
        state::ensure_private_dir(&profile_dir).unwrap();
        fs::write(profile_dir.join("old-data"), b"remove me\n").unwrap();
        {
            let _lock = StateLock::acquire(&paths).unwrap();
            let mut initial = state::load(&paths).unwrap();
            initial.real_claude = Some(fake_claude);
            initial
                .profiles
                .insert("Work".to_owned(), Profile::new(profile_dir.clone()));
            initial.active = Some("Work".to_owned());
            state::save(&paths, &initial).unwrap();
        }

        let reservation_path = paths.profile_reservations_dir.join("work.lock");
        let mut observed_purge = false;
        remove_with_purge(&paths, "Work", true, true, |purge_path| {
            observed_purge = true;
            assert_eq!(purge_path, profile_dir);
            assert!(!state::load(&paths).unwrap().profiles.contains_key("Work"));

            let reservation = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&reservation_path)
                .unwrap();
            let result = unsafe { flock(reservation.as_raw_fd(), LOCK_EX | LOCK_NB) };
            assert_eq!(result, -1, "profile reservation was released before purge");
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                ErrorKind::WouldBlock
            );

            fs::remove_dir_all(purge_path)?;
            Ok(())
        })
        .unwrap();
        assert!(observed_purge);

        let reservation = OpenOptions::new()
            .read(true)
            .write(true)
            .open(reservation_path)
            .unwrap();
        assert_eq!(
            unsafe { flock(reservation.as_raw_fd(), LOCK_EX | LOCK_NB) },
            0,
            "profile reservation remained locked after remove returned"
        );
    }
}
