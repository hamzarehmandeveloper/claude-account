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

        loop {
            let result = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
            if result == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error).context("failed to lock profile state");
            }
        }

        Ok(Self { _file: file })
    }
}

pub fn load(paths: &AppPaths) -> Result<State> {
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
    validate_platform_state(&state)?;
    Ok(state)
}

fn validate_platform_state(state: &State) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        for name in state.profiles.keys() {
            if let Some(existing) = state.case_colliding_profile_name(name) {
                anyhow::bail!(
                    "profiles `{name}` and `{existing}` differ only by letter case; macOS profile names must be unique ignoring ASCII case"
                );
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    let _ = state;

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
}
