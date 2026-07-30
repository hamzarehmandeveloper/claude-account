use std::env;
use std::ffi::OsString;
use std::fs;
#[cfg(target_os = "macos")]
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "macos")]
use std::process::{Output, Stdio};
#[cfg(target_os = "macos")]
use std::thread::{self, JoinHandle};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

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
#[cfg(target_os = "macos")]
const MACOS_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const VERSION_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    #[cfg(target_os = "macos")]
    command.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", config_dir);

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
        let output = query_claude_version(real_claude)?;
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

#[cfg(target_os = "macos")]
fn query_claude_version(real_claude: &Path) -> Result<Output> {
    let mut command = command_without_auth_environment(real_claude);
    command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to query Claude Code version from {}",
            real_claude.display()
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .context("Claude Code version probe did not capture stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("Claude Code version probe did not capture stderr")?;
    let stdout_reader = read_version_stream(stdout);
    let stderr_reader = read_version_stream(stderr);
    let started = Instant::now();

    loop {
        if let Some(status) = child.try_wait().with_context(|| {
            format!(
                "failed while querying Claude Code version from {}",
                real_claude.display()
            )
        })? {
            return Ok(Output {
                status,
                stdout: collect_version_stream(stdout_reader, "stdout")?,
                stderr: collect_version_stream(stderr_reader, "stderr")?,
            });
        }

        if started.elapsed() >= MACOS_VERSION_PROBE_TIMEOUT {
            child.kill().with_context(|| {
                format!(
                    "failed to terminate timed-out Claude Code version probe from {}",
                    real_claude.display()
                )
            })?;
            child.wait().with_context(|| {
                format!(
                    "failed to reap timed-out Claude Code version probe from {}",
                    real_claude.display()
                )
            })?;
            bail!(
                "timed out querying Claude Code version from {}; macOS requires Claude Code 2.1.144 or later for isolated Keychain credentials",
                real_claude.display()
            );
        }

        thread::sleep(VERSION_PROBE_POLL_INTERVAL);
    }
}

#[cfg(target_os = "macos")]
fn read_version_stream<R>(mut stream: R) -> JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut contents = Vec::new();
        stream.read_to_end(&mut contents)?;
        Ok(contents)
    })
}

#[cfg(target_os = "macos")]
fn collect_version_stream(
    reader: JoinHandle<io::Result<Vec<u8>>>,
    stream_name: &str,
) -> Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("Claude Code version probe {stream_name} reader panicked"))?
        .with_context(|| format!("failed to read Claude Code version probe {stream_name}"))
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
