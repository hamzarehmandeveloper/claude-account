use std::env;
use std::ffi::OsString;
#[cfg(any(target_os = "macos", test))]
use std::fmt;
use std::fs;
#[cfg(target_os = "macos")]
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "macos")]
use std::process::{Child, ExitStatus, Output, Stdio};
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
#[cfg(any(target_os = "macos", test))]
const MINIMUM_MACOS_CLAUDE_VERSION: (u64, u64, u64) = (2, 1, 144);
#[cfg(target_os = "macos")]
const MACOS_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const VERSION_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(target_os = "macos")]
const MACOS_ESRCH: i32 = 3;
#[cfg(target_os = "macos")]
const MACOS_SIGKILL: i32 = 9;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

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
        if !version.supports_isolated_macos_keychain() {
            bail!(
                "Claude Code 2.1.144 or later is required on macOS for isolated Keychain credentials; found {version}; update Claude Code and retry"
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    let _ = real_claude;

    Ok(())
}

#[cfg(target_os = "macos")]
fn query_claude_version(real_claude: &Path) -> Result<Output> {
    let deadline = Instant::now() + MACOS_VERSION_PROBE_TIMEOUT;
    let mut command = command_without_auth_environment(real_claude);
    command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = command.spawn().with_context(|| {
        format!(
            "failed to query Claude Code version from {}",
            real_claude.display()
        )
    })?;
    let mut probe = VersionProbe::new(child);
    let stdout = probe
        .child
        .stdout
        .take()
        .context("Claude Code version probe did not capture stdout")?;
    let stderr = probe
        .child
        .stderr
        .take()
        .context("Claude Code version probe did not capture stderr")?;
    let mut stdout_reader = Some(read_version_stream(stdout));
    let mut stderr_reader = Some(read_version_stream(stderr));
    let mut status = None;

    loop {
        if Instant::now() >= deadline {
            return timeout_version_probe(&mut probe, real_claude);
        }

        if status.is_none() {
            status = probe.try_wait().with_context(|| {
                format!(
                    "failed while querying Claude Code version from {}",
                    real_claude.display()
                )
            })?;
        }

        let streams_finished = stdout_reader.as_ref().is_some_and(JoinHandle::is_finished)
            && stderr_reader.as_ref().is_some_and(JoinHandle::is_finished);
        if streams_finished {
            if let Some(status) = status.take() {
                let stdout = collect_version_stream(stdout_reader.take().unwrap(), "stdout")?;
                let stderr = collect_version_stream(stderr_reader.take().unwrap(), "stderr")?;
                if Instant::now() >= deadline {
                    return timeout_version_probe(&mut probe, real_claude);
                }
                probe.mark_complete();
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(VERSION_PROBE_POLL_INTERVAL.min(remaining));
    }
}

#[cfg(target_os = "macos")]
fn timeout_version_probe(probe: &mut VersionProbe, real_claude: &Path) -> Result<Output> {
    probe.terminate_and_reap().with_context(|| {
        format!(
            "timed out querying Claude Code version from {}; failed to clean up the version probe",
            real_claude.display()
        )
    })?;
    bail!(
        "timed out querying Claude Code version from {}; macOS requires Claude Code 2.1.144 or later for isolated Keychain credentials",
        real_claude.display()
    )
}

#[cfg(target_os = "macos")]
struct VersionProbe {
    child: Child,
    process_group: i32,
    reaped: bool,
    complete: bool,
}

#[cfg(target_os = "macos")]
impl VersionProbe {
    fn new(child: Child) -> Self {
        Self {
            process_group: child.id() as i32,
            child,
            reaped: false,
            complete: false,
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn terminate_and_reap(&mut self) -> Result<()> {
        let terminate_result = terminate_process_group(self.process_group);
        let reap_result = if self.reaped {
            Ok(())
        } else {
            self.child.wait().map(|_| ()).map_err(anyhow::Error::from)
        };
        if reap_result.is_ok() {
            self.reaped = true;
        }
        if terminate_result.is_ok() && reap_result.is_ok() {
            self.complete = true;
        }

        terminate_result.context("failed to terminate Claude Code version probe process group")?;
        reap_result.context("failed to reap Claude Code version probe")?;
        Ok(())
    }

    fn mark_complete(&mut self) {
        debug_assert!(self.reaped);
        self.complete = true;
    }
}

#[cfg(target_os = "macos")]
impl Drop for VersionProbe {
    fn drop(&mut self) {
        if self.complete {
            return;
        }

        let _ = terminate_process_group(self.process_group);
        if !self.reaped {
            let _ = self.child.wait();
            self.reaped = true;
        }
    }
}

#[cfg(target_os = "macos")]
fn terminate_process_group(process_group: i32) -> io::Result<()> {
    // A negative pid asks kill(2) to signal every process in this process group.
    let result = unsafe { kill(-process_group, MACOS_SIGKILL) };
    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(MACOS_ESRCH) {
        Ok(())
    } else {
        Err(error)
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
#[derive(Debug, Eq, PartialEq)]
struct ClaudeVersion {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Option<String>,
}

#[cfg(any(target_os = "macos", test))]
impl ClaudeVersion {
    fn supports_isolated_macos_keychain(&self) -> bool {
        let numeric_core = (self.major, self.minor, self.patch);
        numeric_core > MINIMUM_MACOS_CLAUDE_VERSION
            || (numeric_core == MINIMUM_MACOS_CLAUDE_VERSION && self.prerelease.is_none())
    }
}

#[cfg(any(target_os = "macos", test))]
impl fmt::Display for ClaudeVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(prerelease) = &self.prerelease {
            write!(formatter, "-{prerelease}")?;
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_claude_version(value: &str) -> Option<ClaudeVersion> {
    value.split_whitespace().find_map(|word| {
        let version = word.strip_prefix('v').unwrap_or(word);
        let version = version.split('+').next()?;
        let (numeric_core, prerelease) = match version.split_once('-') {
            Some((_numeric_core, "")) => return None,
            Some((numeric_core, prerelease)) => (numeric_core, Some(prerelease.to_owned())),
            None => (version, None),
        };
        let mut parts = numeric_core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        (parts.next().is_none()).then_some(ClaudeVersion {
            major,
            minor,
            patch,
            prerelease,
        })
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
            Some(ClaudeVersion {
                major: 2,
                minor: 1,
                patch: 144,
                prerelease: None,
            })
        );
        assert_eq!(
            parse_claude_version("v2.2.0-beta.1 (Claude Code)"),
            Some(ClaudeVersion {
                major: 2,
                minor: 2,
                patch: 0,
                prerelease: Some("beta.1".to_owned()),
            })
        );
        assert_eq!(parse_claude_version("Claude Code"), None);
    }

    #[test]
    fn applies_semver_precedence_at_macos_minimum() {
        let supports = |version| {
            parse_claude_version(version)
                .unwrap()
                .supports_isolated_macos_keychain()
        };

        assert!(!supports("2.1.143"));
        assert!(!supports("2.1.144-beta.1"));
        assert!(supports("2.1.144"));
        assert!(supports("2.1.144+build.1"));
        assert!(supports("2.1.145-beta.1"));
    }

    #[test]
    fn displays_prerelease_in_version_errors() {
        assert_eq!(
            parse_claude_version("2.1.144-beta.1").unwrap().to_string(),
            "2.1.144-beta.1"
        );
    }
}
