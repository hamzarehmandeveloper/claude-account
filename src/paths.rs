use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const APP_DIRECTORY: &str = "claude-account";

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

        let configured_config_root = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map(|path| path.join(APP_DIRECTORY));
        let configured_data_root = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map(|path| path.join(APP_DIRECTORY));

        let (config_root, data_root) = match (configured_config_root, configured_data_root) {
            (Some(config), Some(data)) => (config, data),
            (config, data) => {
                let (default_config, default_data) = platform_default_roots(&home)?;
                (
                    config.unwrap_or(default_config),
                    data.unwrap_or(default_data),
                )
            }
        };

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

fn platform_default_roots(home: &Path) -> Result<(PathBuf, PathBuf)> {
    #[cfg(target_os = "macos")]
    {
        macos_default_roots(home)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(linux_default_roots(home))
    }
}

fn linux_default_roots(home: &Path) -> (PathBuf, PathBuf) {
    (
        home.join(".config").join(APP_DIRECTORY),
        home.join(".local/share").join(APP_DIRECTORY),
    )
}

#[cfg(any(target_os = "macos", test))]
fn macos_default_roots(home: &Path) -> Result<(PathBuf, PathBuf)> {
    let application_support = home.join("Library/Application Support").join(APP_DIRECTORY);
    let (legacy_config, legacy_data) = linux_default_roots(home);
    let application_support_state = application_support.join("state.json");
    let legacy_state = legacy_config.join("state.json");

    match (application_support_state.exists(), legacy_state.exists()) {
        (true, true) => bail!(
            "found claude-account state in both {} and {}; set CLAUDE_ACCOUNT_HOME to the installation you want to use, then remove or archive the other state after verifying its profiles",
            application_support_state.display(),
            legacy_state.display()
        ),
        (false, true) => Ok((legacy_config, legacy_data)),
        _ => Ok((application_support.clone(), application_support)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn linux_roots_keep_the_existing_xdg_style_layout() {
        let home = Path::new("/home/example");
        let (config, data) = linux_default_roots(home);

        assert_eq!(
            config,
            PathBuf::from("/home/example/.config/claude-account")
        );
        assert_eq!(
            data,
            PathBuf::from("/home/example/.local/share/claude-account")
        );
    }

    #[test]
    fn macos_roots_default_to_application_support() {
        let temp = tempfile::tempdir().unwrap();
        let expected = temp
            .path()
            .join("Library/Application Support/claude-account");

        let (config, data) = macos_default_roots(temp.path()).unwrap();

        assert_eq!(config, expected);
        assert_eq!(data, config);
    }

    #[test]
    fn macos_roots_reuse_an_existing_xdg_style_installation() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy_config, legacy_data) = linux_default_roots(temp.path());
        fs::create_dir_all(&legacy_config).unwrap();
        fs::write(legacy_config.join("state.json"), b"{}\n").unwrap();

        let (config, data) = macos_default_roots(temp.path()).unwrap();

        assert_eq!(config, legacy_config);
        assert_eq!(data, legacy_data);
    }

    #[test]
    fn macos_roots_refuse_ambiguous_existing_installations() {
        let temp = tempfile::tempdir().unwrap();
        let native = temp
            .path()
            .join("Library/Application Support/claude-account");
        let (legacy, _) = linux_default_roots(temp.path());
        for directory in [&native, &legacy] {
            fs::create_dir_all(directory).unwrap();
            fs::write(directory.join("state.json"), b"{}\n").unwrap();
        }

        let error = macos_default_roots(temp.path()).unwrap_err();

        assert!(error.to_string().contains("state in both"));
        assert!(error.to_string().contains("CLAUDE_ACCOUNT_HOME"));
    }
}
