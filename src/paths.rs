use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_file: PathBuf,
    pub lock_file: PathBuf,
    pub profile_reservations_dir: PathBuf,
    pub profiles_dir: PathBuf,
    pub shim_dir: PathBuf,
    pub shim: PathBuf,
    pub installed_executable: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        if let Some(root) = env::var_os("CLAUDE_ACCOUNT_HOME") {
            let root = absolute_path(root, "CLAUDE_ACCOUNT_HOME")?;
            return Ok(Self::from_roots(root.clone(), root));
        }

        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .context("HOME is not set to an absolute path")?;

        let config_root = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config"))
            .join("claude-account");

        let data_root = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"))
            .join("claude-account");

        Ok(Self::from_roots(config_root, data_root))
    }

    pub fn from_roots(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        let shim_dir = data_dir.join("bin");
        Self {
            state_file: config_dir.join("state.json"),
            lock_file: config_dir.join("state.lock"),
            profile_reservations_dir: config_dir.join("profile-reservations"),
            profiles_dir: data_dir.join("profiles"),
            shim: shim_dir.join("claude"),
            installed_executable: data_dir.join("libexec/claude-account"),
            config_dir,
            data_dir,
            shim_dir,
        }
    }

    pub fn profile_dir(&self, name: &str) -> PathBuf {
        self.profiles_dir.join(name)
    }
}

fn absolute_path(value: impl AsRef<Path>, variable: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value.as_ref());
    if !path.is_absolute() {
        bail!("{variable} must contain an absolute path");
    }
    Ok(path)
}
