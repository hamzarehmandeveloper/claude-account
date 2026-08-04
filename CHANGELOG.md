# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-08-04

### Added

- macOS account isolation with a Claude Code 2.1.144 minimum-version guard for
  profile-scoped Keychain credentials.
- Native CI and release archives for Apple Silicon and Intel macOS.
- An explicit `resolve-case-collision` recovery command for legacy state that
  contains names differing only by ASCII letter case.
- Native macOS storage under `~/Library/Application Support/claude-account`,
  compatible with OAuth profiles created by `Kerber0ss/claude-account-macos`.
- Automatic reuse of an existing XDG-style macOS installation when it is the
  only existing account registry.

### Changed

- Reject macOS profile names that differ only by ASCII letter case to prevent
  collisions on case-insensitive filesystems.
- Force managed Claude processes to use the selected profile's configuration
  and secure-storage directories, and ignore inherited authentication tokens
  unless `CLAUDE_ACCOUNT_PRESERVE_AUTH_ENV=1` is set.
- Print shell startup guidance for both zsh and Bash.

### Fixed

- Serialize `account use` with profile removal so concurrent commands cannot
  leave the active profile pointing to a removed account.
- Fail closed when both native and XDG-style macOS account registries exist,
  rather than selecting one silently.
- Detect API profiles created by `claude-account-macos` and reject them without
  reading or migrating their API keys; OAuth profiles remain compatible.

### Contributors

- macOS behavior and native layout were validated by
  [@Kerber0ss](https://github.com/Kerber0ss).
- Cross-platform isolation and test hardening were contributed by
  [@Yiminnn](https://github.com/Yiminnn).

## [0.1.1] - 2026-07-30

### Fixed

- Complete Claude Code onboarding after a verified `account add` login so the
  first normal `claude` launch does not ask the user to authenticate again.
- Require `auth status --json` to explicitly report `loggedIn: true` before a
  profile is registered.

## [0.1.0] - 2026-07-30

### Added

- Linux-only Claude Code profile isolation through `CLAUDE_CONFIG_DIR`.
- `add`, `use`, `list`, `current`, and `remove` account commands.
- Transparent forwarding of normal Claude Code commands and arguments.
- Official Claude Code login, status verification, and logout integration.
- Atomic state writes, process locking, strict filesystem permissions, and
  profile-name validation.
- Safe profile removal with separate unregister and permanent purge modes.
- Non-invasive shim installation that preserves the official Claude launcher.
- Unit and end-to-end lifecycle tests.

[Unreleased]: https://github.com/hamzarehmandeveloper/claude-account/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/hamzarehmandeveloper/claude-account/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/hamzarehmandeveloper/claude-account/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hamzarehmandeveloper/claude-account/releases/tag/v0.1.0
