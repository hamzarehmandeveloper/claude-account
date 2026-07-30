# claude-account

[![CI](https://github.com/hamzarehmandeveloper/claude-account/actions/workflows/ci.yml/badge.svg)](https://github.com/hamzarehmandeveloper/claude-account/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/hamzarehmandeveloper/claude-account)](https://github.com/hamzarehmandeveloper/claude-account/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A Linux-only profile switcher for Claude Code. It gives Claude Code an isolated
`CLAUDE_CONFIG_DIR` for each account and transparently forwards normal commands
to the official Claude executable.

```bash
claude account add work
claude account add personal
claude account use work
claude account list
claude account current
claude account remove personal

claude
claude "fix this bug in main.py"
```

Claude Code itself performs login, logout, credential storage, and token
refresh. `claude-account` never reads or copies credential contents.

> [!IMPORTANT]
> This is an independent community project. It is not made, endorsed, or
> supported by Anthropic. Claude and Claude Code are products of Anthropic.

## Requirements

- Linux
- A working Claude Code installation
- Rust 1.85 or later to build from source

## Install a release

Download `claude-account-v0.1.1-x86_64-unknown-linux-gnu.tar.gz` and its
`.sha256` file from the [latest release][releases], then verify and install it:

```bash
sha256sum --check claude-account-v0.1.1-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf claude-account-v0.1.1-x86_64-unknown-linux-gnu.tar.gz
./claude-account install
```

The installer prints one `export PATH=...` line. Add that line to `~/.bashrc`
and open a new terminal. The shim lives in its own directory; it does not
replace the official Claude executable.

Confirm the installation:

```bash
type -a claude
claude account list
```

The claude-account shim should appear before the official Claude executable.

## Build from source

```bash
git clone https://github.com/hamzarehmandeveloper/claude-account.git
cd claude-account
cargo build --locked --release
./target/release/claude-account install
```

## Commands

### Add an account

```bash
claude account add work
claude account add personal --email you@example.com
claude account add company --sso
claude account add api-billing --console
```

This opens Claude Code's official login flow. The first profile becomes active.
Adding another profile does not switch the active profile. The command also
completes Claude Code's local onboarding state, so the next `claude` launch
uses the saved login without asking you to authenticate again.

### Switch accounts

```bash
claude account use work
```

Switching affects newly launched Claude processes. Existing sessions keep the
account with which they were started.

### Inspect profiles

```bash
claude account list
claude account current
```

`current` prints only the profile name, making it safe to use in scripts.

### Remove an account

```bash
claude account remove personal
```

This runs Claude Code's official `auth logout` inside the profile and
unregisters it. Settings and session history are preserved, allowing the same
profile name to reuse them later.

To delete all local data belonging to the profile:

```bash
claude account remove personal --purge --yes
```

Removing the active profile is refused unless `--force` is supplied.
`--purge` permanently deletes that profile's settings, sessions, plugins, and
history in addition to its stored login.

### Get help

```bash
claude account --help
claude account add --help
claude account remove --help
```

All non-account commands and flags are passed unchanged to the official Claude
executable:

```bash
claude
claude -p "explain this project"
claude --model opus
claude auth status --text
```

## Storage

By default:

```text
~/.config/claude-account/state.json
~/.local/share/claude-account/profiles/<name>/
~/.local/share/claude-account/bin/claude
~/.local/share/claude-account/libexec/claude-account
```

The standard `XDG_CONFIG_HOME` and `XDG_DATA_HOME` variables are respected.
`CLAUDE_ACCOUNT_HOME` can place all application data under one absolute
directory, which is especially useful for tests.

The state file contains profile names, directory paths, and the real Claude
executable path. It never contains access or refresh tokens.

## Authentication environment variables

To guarantee that the selected profile is actually used, the wrapper removes
these variables from the child Claude process:

- `ANTHROPIC_API_KEY`
- `ANTHROPIC_AUTH_TOKEN`
- `CLAUDE_CODE_OAUTH_TOKEN`

Set `CLAUDE_ACCOUNT_PRESERVE_AUTH_ENV=1` if you intentionally want those
variables to override profile authentication.

## Development

```bash
cargo fmt --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution workflow and
[SECURITY.md](SECURITY.md) for private vulnerability reporting.

## License

Released under the [MIT License](LICENSE).

[releases]: https://github.com/hamzarehmandeveloper/claude-account/releases
