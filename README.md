# claude-account

[![CI](https://github.com/hamzarehmandeveloper/claude-account/actions/workflows/ci.yml/badge.svg)](https://github.com/hamzarehmandeveloper/claude-account/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/hamzarehmandeveloper/claude-account)](https://github.com/hamzarehmandeveloper/claude-account/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A cross-platform profile switcher for Claude Code on Linux and macOS. It gives
Claude Code an isolated configuration and credential-storage directory for each
account and transparently forwards normal commands to the official executable.

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

- Linux, macOS 10.15 or later on Intel, or macOS 11 or later on Apple Silicon
- A working Claude Code installation
- Claude Code 2.1.144 or later on macOS, for profile-scoped Keychain credentials
- Rust 1.85 or later to build from source

## Install a release

Each release produces archives for Linux x86_64, macOS Apple Silicon, and
macOS Intel:

| Platform | Target |
| --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu` |
| macOS Apple Silicon | `aarch64-apple-darwin` |
| macOS Intel | `x86_64-apple-darwin` |

Set `VERSION` to a tag from the [releases page][releases]. The following picks
the archive for the current machine, downloads it and its checksum, and verifies
it with the checksum tool available on the platform:

```bash
VERSION=vX.Y.Z
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  *) echo "No release for this platform" >&2; exit 1 ;;
esac
ARCHIVE="claude-account-${VERSION}-${TARGET}.tar.gz"
curl -fLO "https://github.com/hamzarehmandeveloper/claude-account/releases/download/${VERSION}/${ARCHIVE}"
curl -fLO "https://github.com/hamzarehmandeveloper/claude-account/releases/download/${VERSION}/${ARCHIVE}.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum --check "${ARCHIVE}.sha256"
else
  shasum -a 256 --check "${ARCHIVE}.sha256"
fi
tar -xzf "${ARCHIVE}"
./claude-account install
```

The installer prints one `export PATH=...` line. Add it to your shell startup
file (`~/.zshrc` for the default macOS shell or `~/.bashrc` for Bash) and open a
new terminal. The shim lives in its own directory; it does not replace the
official Claude executable.

Release archives are checksum-protected but are not currently signed or
notarized. If macOS Gatekeeper blocks a downloaded binary, build from source
instead of bypassing the warning.

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

On macOS, profile names must also be unique when compared without ASCII letter
case, preventing two names from sharing one directory on common
case-insensitive filesystems.

If a state file created on a case-sensitive system already contains variants
such as `Work` and `work`, normal commands fail closed on macOS. Choose the
exact spelling to unregister and run the explicit recovery command:

```bash
claude account resolve-case-collision work
# Or invoke the manager directly:
claude-account resolve-case-collision work
```

This only unregisters that exact name from the state file. It does not run
Claude Code, log out, delete or rename either profile directory, or touch
Keychain credentials. The command names the surviving profile and, when the
removed name was active, the `claude account use NAME` step that completes
recovery. It refuses profiles that do not have a case-colliding sibling.

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

Linux defaults:

```text
~/.config/claude-account/state.json
~/.local/share/claude-account/profiles/<name>/
~/.local/share/claude-account/bin/claude
~/.local/share/claude-account/libexec/claude-account
```

macOS defaults, matching the tested `claude-account-macos` layout:

```text
~/Library/Application Support/claude-account/state.json
~/Library/Application Support/claude-account/profiles/<name>/
~/Library/Application Support/claude-account/bin/claude
~/Library/Application Support/claude-account/libexec/claude-account
```

The standard `XDG_CONFIG_HOME` and `XDG_DATA_HOME` variables are respected on
both platforms. `CLAUDE_ACCOUNT_HOME` can place all application data under one
absolute directory, which is especially useful for tests.

On macOS, an existing XDG-style installation is reused automatically when no
Application Support state exists. If state exists in both layouts, the program
fails closed and asks you to select one explicitly with `CLAUDE_ACCOUNT_HOME`;
it never guesses between two account registries.

Existing OAuth profiles created by
[`Kerber0ss/claude-account-macos`](https://github.com/Kerber0ss/claude-account-macos)
remain readable in the native Application Support layout. Its API-key profiles
are intentionally not imported in this release: the program reports a clear
error without reading or migrating their keys.

The state file contains profile names, directory paths, and the real Claude
executable path. It never contains access or refresh tokens.

## macOS credential isolation

Claude Code stores subscription credentials in macOS Keychain. Claude Code
2.1.144 and later supports configuration-directory-scoped Keychain entries,
allowing each claude-account profile to keep an independent login.
claude-account passes both `CLAUDE_CONFIG_DIR` and
`CLAUDE_SECURESTORAGE_CONFIG_DIR` as the same private profile directory and
overrides inherited values for every managed Claude process. It checks the
Claude Code version before login, launch, and logout on macOS so an older global
Keychain entry cannot silently select or log out the wrong account.

claude-account never reads, copies, or writes Keychain credential contents;
Claude Code continues to own login, token refresh, and secure storage.

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
