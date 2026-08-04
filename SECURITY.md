# Security policy

## Supported versions

Security fixes are provided for the latest released version.

## Reporting a vulnerability

Please use GitHub's private **Report a vulnerability** feature on the
repository's Security tab. Do not open a public issue for a suspected
credential exposure, path traversal, arbitrary command execution, or unsafe
deletion vulnerability.

Include:

- The affected version, operating system, and OS version
- Reproduction steps using placeholder credentials
- The expected and observed behavior
- Any proposed fix, if available

Never include real Claude credentials, access tokens, refresh tokens, API keys,
or the contents of `.credentials.json`.

## Security model

claude-account does not parse or copy Claude credentials. It creates an
isolated `CLAUDE_CONFIG_DIR` and delegates authentication to the official
Claude Code executable. On macOS, Claude Code 2.1.144 or later supports
configuration-directory-scoped Keychain entries. claude-account forces both
`CLAUDE_CONFIG_DIR` and `CLAUDE_SECURESTORAGE_CONFIG_DIR` to the same private
profile directory and rejects older versions before login, launch, or logout.
Local profile directories are owner-only (`0700`), profile-name reservation
directories are owner-only (`0700`), and registry and reservation files are
owner-readable and owner-writable (`0600`).

OAuth state from `claude-account-macos` is accepted for compatibility. API
profiles from that project are rejected without reading or migrating their API
keys because API-key management is outside this release's security boundary.

The program runs with the invoking user's permissions. Anyone who can modify
that user's configuration or executable search path is already within the same
local trust boundary. A custom `CLAUDE_ACCOUNT_HOME` should point to a private
local directory; filesystem ACLs or network-share permissions can grant access
that POSIX `0600` and `0700` mode bits alone do not remove.
