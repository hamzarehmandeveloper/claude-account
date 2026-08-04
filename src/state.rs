use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;

const LOCK_EX: i32 = 2;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default = "state_version")]
    pub version: u32,
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub real_claude: Option<PathBuf>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub config_dir: PathBuf,
    pub created_at: u64,
    #[serde(
        rename = "authentication",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    imported_authentication: Option<ImportedAuthentication>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ImportedAuthentication {
    #[serde(alias = "oauth")]
    OAuth,
    Api,
}

impl Profile {
    pub fn new(config_dir: PathBuf) -> Self {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            config_dir,
            created_at,
            imported_authentication: None,
        }
    }
}

impl State {
    pub fn case_colliding_profile_name(&self, candidate: &str) -> Option<&str> {
        self.profiles.keys().find_map(|existing| {
            let existing = existing.as_str();
            (existing != candidate && existing.eq_ignore_ascii_case(candidate)).then_some(existing)
        })
    }
}

fn state_version() -> u32 {
    1
}

pub struct StateLock {
    _file: File,
}

pub struct ProfileReservation {
    _file: File,
}

#[derive(Clone, Copy)]
enum CaseCollisionValidation {
    Enforce,
    AllowForExplicitResolution,
}

impl StateLock {
    pub fn acquire(paths: &AppPaths) -> Result<Self> {
        ensure_private_dir(&paths.config_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&paths.lock_file)
            .with_context(|| format!("failed to open {}", paths.lock_file.display()))?;

        lock_exclusive(&file).context("failed to lock profile state")?;

        Ok(Self { _file: file })
    }
}

impl ProfileReservation {
    pub fn acquire(paths: &AppPaths, name: &str) -> Result<Self> {
        ensure_private_dir(&paths.profile_reservations_dir)?;
        let path = paths
            .profile_reservations_dir
            .join(format!("{}.lock", profile_reservation_key(name)));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to protect {}", path.display()))?;
        lock_exclusive(&file)
            .with_context(|| format!("failed to reserve profile name `{name}`"))?;

        Ok(Self { _file: file })
    }
}

fn lock_exclusive(file: &File) -> io::Result<()> {
    loop {
        let result = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn profile_reservation_key(name: &str) -> String {
    if cfg!(target_os = "macos") {
        name.to_ascii_lowercase()
    } else {
        name.to_owned()
    }
}

pub fn load(paths: &AppPaths) -> Result<State> {
    load_with_case_collision_validation(paths, CaseCollisionValidation::Enforce)
}

/// Load version-checked state while bypassing only the macOS case-collision check.
pub fn load_for_case_collision_resolution(paths: &AppPaths) -> Result<State> {
    load_with_case_collision_validation(paths, CaseCollisionValidation::AllowForExplicitResolution)
}

fn load_with_case_collision_validation(
    paths: &AppPaths,
    collision_validation: CaseCollisionValidation,
) -> Result<State> {
    let state = match File::open(&paths.state_file) {
        Ok(file) => {
            let state: State = serde_json::from_reader(file)
                .with_context(|| format!("failed to parse {}", paths.state_file.display()))?;
            if state.version != 1 {
                anyhow::bail!("unsupported state version {}", state.version);
            }
            state
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => State {
            version: state_version(),
            ..State::default()
        },
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", paths.state_file.display()));
        }
    };
    validate_platform_state(&state, collision_validation)?;
    Ok(state)
}

fn validate_platform_state(
    state: &State,
    collision_validation: CaseCollisionValidation,
) -> Result<()> {
    if let Some((name, _)) = state
        .profiles
        .iter()
        .find(|(_, profile)| profile.imported_authentication == Some(ImportedAuthentication::Api))
    {
        anyhow::bail!(
            "profile `{name}` is an API profile created by claude-account-macos; this release supports OAuth profiles only and will not read or migrate its API key; use claude-account-macos to remove that API profile or continue using it for that profile"
        );
    }

    #[cfg(target_os = "macos")]
    {
        if matches!(collision_validation, CaseCollisionValidation::Enforce) {
            for name in state.profiles.keys() {
                if let Some(existing) = state.case_colliding_profile_name(name) {
                    anyhow::bail!(
                        "profiles `{name}` and `{existing}` differ only by letter case; macOS profile names must be unique ignoring ASCII case; run `claude account resolve-case-collision {name}` to unregister exactly `{name}` without deleting local data or credentials"
                    );
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    let _ = (state, collision_validation);

    Ok(())
}

pub fn save(paths: &AppPaths, state: &State) -> Result<()> {
    ensure_private_dir(&paths.config_dir)?;
    let temporary = temporary_state_path(&paths.state_file);
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        serde_json::to_writer_pretty(&mut file, state).context("failed to serialize state")?;
        file.write_all(b"\n")
            .context("failed to finish state file")?;
        file.sync_all().context("failed to sync state file")?;
        fs::rename(&temporary, &paths.state_file).with_context(|| {
            format!(
                "failed to replace {} with {}",
                paths.state_file.display(),
                temporary.display()
            )
        })?;
        fs::set_permissions(&paths.state_file, fs::Permissions::from_mode(0o600))
            .context("failed to protect state file")?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create directory {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to protect directory {}", path.display()))?;
    Ok(())
}

fn temporary_state_path(state_file: &Path) -> PathBuf {
    let filename = state_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    state_file.with_file_name(format!("{filename}.tmp.{}.{}", std::process::id(), nonce))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_reservation_keys_serialize_names_according_to_platform() {
        let upper = profile_reservation_key("Work");
        let lower = profile_reservation_key("work");
        if cfg!(target_os = "macos") {
            assert_eq!(upper, "work");
            assert_eq!(upper, lower);
        } else {
            assert_eq!(upper, "Work");
            assert_ne!(upper, lower);
        }
    }

    #[test]
    fn state_round_trip_preserves_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(temp.path().join("config"), temp.path().join("data"));
        let mut state = State {
            version: 1,
            ..State::default()
        };
        state.active = Some("work".to_owned());
        state
            .profiles
            .insert("work".to_owned(), Profile::new(paths.profile_dir("work")));

        let _lock = StateLock::acquire(&paths).unwrap();
        save(&paths, &state).unwrap();
        let loaded = load(&paths).unwrap();

        assert_eq!(loaded.active.as_deref(), Some("work"));
        assert!(loaded.profiles.contains_key("work"));
    }

    #[test]
    fn imported_oauth_profile_remains_compatible() {
        for authentication in ["o_auth", "oauth"] {
            let profile: Profile = serde_json::from_value(serde_json::json!({
                "config_dir": "/tmp/work",
                "created_at": 0,
                "authentication": authentication
            }))
            .unwrap();

            assert_eq!(
                profile.imported_authentication,
                Some(ImportedAuthentication::OAuth)
            );
        }
    }

    #[test]
    fn load_rejects_imported_api_profiles_without_reading_their_keys() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(temp.path().join("config"), temp.path().join("data"));
        let _lock = StateLock::acquire(&paths).unwrap();
        fs::write(
            &paths.state_file,
            format!(
                r#"{{"version":1,"profiles":{{"gateway":{{"config_dir":"{}","created_at":0,"authentication":"api"}}}}}}"#,
                paths.profile_dir("gateway").display()
            ),
        )
        .unwrap();

        let error = load(&paths).unwrap_err();

        assert!(error.to_string().contains("API profile"));
        assert!(error
            .to_string()
            .contains("will not read or migrate its API key"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn load_rejects_case_colliding_profile_names() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(temp.path().join("config"), temp.path().join("data"));
        let mut state = State {
            version: 1,
            ..State::default()
        };
        state
            .profiles
            .insert("work".to_owned(), Profile::new(paths.profile_dir("work")));
        state
            .profiles
            .insert("Work".to_owned(), Profile::new(paths.profile_dir("Work")));

        save(&paths, &state).unwrap();
        let error = load(&paths).unwrap_err();

        assert!(error.to_string().contains("differ only by letter case"));
    }

    #[test]
    fn collision_resolution_load_still_rejects_unsupported_versions() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(temp.path().join("config"), temp.path().join("data"));
        let _lock = StateLock::acquire(&paths).unwrap();
        fs::write(
            &paths.state_file,
            r#"{"version":2,"active":null,"real_claude":null,"profiles":{}}"#,
        )
        .unwrap();

        let error = load_for_case_collision_resolution(&paths).unwrap_err();

        assert!(error.to_string().contains("unsupported state version 2"));
    }

    #[test]
    fn collision_resolution_load_still_rejects_malformed_state() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(temp.path().join("config"), temp.path().join("data"));
        let _lock = StateLock::acquire(&paths).unwrap();
        fs::write(&paths.state_file, b"not json\n").unwrap();

        let error = load_for_case_collision_resolution(&paths).unwrap_err();

        assert!(error.to_string().contains("failed to parse"));
    }
}
