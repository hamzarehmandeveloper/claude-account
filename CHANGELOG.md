# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/hamzarehmandeveloper/claude-account/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/hamzarehmandeveloper/claude-account/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hamzarehmandeveloper/claude-account/releases/tag/v0.1.0
