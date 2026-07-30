use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::paths::AppPaths;
use crate::state;

const AUTH_ENVIRONMENT: [&str; 3] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];
#[cfg(target_os = "macos")]
const MINIMUM_MACOS_CLAUDE_VERSION: (u64, u64, u64) = (2, 1, 144);

pub fn exec_active_profile(paths: &AppPaths, arguments: &[OsString]) -> Result<()> {
    let state = state::load(paths)?;
    let active = state
        .active
        .as_deref()
        .context("no active profile; run `claude account add NAME` or `claude account use NAME`")?;
    let profile = state
        .profiles
        .get(active)
        .with_context(|| format!("active profile `{active}` does not exist"))?;
    let real_claude = state
        .real_claude
        .as_deref()
        .context("real Claude executable is not configured; run `claude-account install`")?;
    validate_executable(real_claude)?;
    validate_platform_support(real_claude)?;

    let mut command = managed_command(real_claude, &profile.config_dir);
    command.args(arguments);
    let error = command.exec();
    Err(error).with_context(|| format!("failed to execute {}", real_claude.display()))
}

pub fn managed_command(real_claude: &Path, config_dir: &Path) -> Command {
    let mut command = command_without_auth_environment(real_claude);
    command.env("CLAUDE_CONFIG_DIR", config_dir);

    command
}

fn command_without_auth_environment(program: &Path) -> Command {
    let mut command = Command::new(program);
    if env::var_os("CLAUDE_ACCOUNT_PRESERVE_AUTH_ENV").as_deref() != Some("1".as_ref()) {
        for variable in AUTH_ENVIRONMENT {
            command.env_remove(variable);
        }
    }
    command
}

pub fn validate_platform_support(real_claude: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let output = command_without_auth_environment(real_claude)
            .arg("--version")
            .output()
            .with_context(|| {
                format!(
                    "failed to query Claude Code version from {}",
                    real_claude.display()
                )
            })?;
        if !output.status.success() {
            bail!(
                "failed to query Claude Code version from {}; macOS requires Claude Code 2.1.144 or later for isolated Keychain credentials",
                real_claude.display()
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reported = if stdout.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        let version = parse_claude_version(reported).with_context(|| {
            format!(
                "could not parse Claude Code version from `{reported}`; macOS requires Claude Code 2.1.144 or later for isolated Keychain credentials"
            )
        })?;
        if version < MINIMUM_MACOS_CLAUDE_VERSION {
            bail!(
                "Claude Code 2.1.144 or later is required on macOS for isolated Keychain credentials; found {}.{}.{}; update Claude Code and retry",
                version.0,
                version.1,
                version.2
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    let _ = real_claude;

    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn parse_claude_version(value: &str) -> Option<(u64, u64, u64)> {
    value.split_whitespace().find_map(|word| {
        let version = word.trim_start_matches('v').split('-').next()?;
        let mut parts = version.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        (parts.next().is_none()).then_some((major, minor, patch))
    })
}

pub fn resolve_real_claude(
    configured: Option<&Path>,
    current_executable: &Path,
    paths: &AppPaths,
) -> Result<PathBuf> {
    if let Some(explicit) = env::var_os("CLAUDE_ACCOUNT_REAL_CLAUDE") {
        let explicit = PathBuf::from(explicit);
        validate_distinct_executable(&explicit, current_executable)?;
        return Ok(explicit);
    }

    if let Some(configured) = configured {
        if validate_distinct_executable(configured, current_executable).is_ok() {
            return Ok(configured.to_path_buf());
        }
    }

    let path = env::var_os("PATH").context("PATH is not set")?;
    for directory in env::split_paths(&path) {
        let candidate = if directory.as_os_str().is_empty() {
            env::current_dir()?.join("claude")
        } else {
            directory.join("claude")
        };
        if candidate == paths.shim {
            continue;
        }
        if validate_distinct_executable(&candidate, current_executable).is_ok() {
            return Ok(candidate);
        }
    }

    bail!(
        "could not find the real `claude` executable; pass it with \
         `claude-account install --real /path/to/claude`"
    )
}

pub fn validate_executable(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("Claude executable does not exist: {}", path.display()))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        bail!("path is not executable: {}", path.display());
    }
    Ok(())
}

fn validate_distinct_executable(candidate: &Path, current_executable: &Path) -> Result<()> {
    validate_executable(candidate)?;
    let candidate_canonical = fs::canonicalize(candidate)
        .with_context(|| format!("failed to resolve {}", candidate.display()))?;
    let current_canonical = fs::canonicalize(current_executable)
        .with_context(|| format!("failed to resolve {}", current_executable.display()))?;
    if candidate_canonical == current_canonical {
        bail!("candidate points back to the claude-account wrapper");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_code_version_output() {
        assert_eq!(
            parse_claude_version("2.1.144 (Claude Code)"),
            Some((2, 1, 144))
        );
        assert_eq!(
            parse_claude_version("v2.2.0-beta.1 (Claude Code)"),
            Some((2, 2, 0))
        );
        assert_eq!(parse_claude_version("Claude Code"), None);
    }
}
