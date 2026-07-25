<!--
File: docs/11-cli/installation.md
Document ID: CLI-001
-->

# CLI Installation

**Document ID:** CLI-001  
**File Path:** `docs/11-cli/installation.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document describes how to install, update, and verify the `wovyr` CLI across platforms.

---

# 2. Supported Platforms

| OS | Architectures |
|----|---------------|
| Linux | x86_64, aarch64 |
| macOS | x86_64 (Intel), aarch64 (Apple Silicon) |
| Windows | x86_64 |

The CLI is a single statically-linked Rust binary with no runtime dependencies.

---

# 3. Install Methods

## 3.1 Install script (Linux/macOS)

```bash
curl -fsSL https://get.wovyr.example.com/install.sh | sh
```

Installs the latest stable release to `~/.wovyr/bin` and adds it to `PATH`.

## 3.2 Homebrew (macOS/Linux)

```bash
brew install wovyr-ai/tap/wovyr
```

## 3.3 Windows

```powershell
winget install Wovyr.CLI
# or Scoop:
scoop install wovyr
```

## 3.4 Container image

```bash
docker run --rm -v "$PWD:/work" ghcr.io/wovyr-ai/cli:latest version
```

## 3.5 Direct download

Signed binaries and checksums are published per release; verify the signature
before use (see [§6](#6-verifying-the-download)).

---

# 4. Versioning & Channels

| Channel | Use |
|---------|-----|
| `stable` | Production (default) |
| `beta` | Pre-release testing |

The CLI follows the platform [API versioning](../09-api/overview.md#3-base-url--versioning)
and warns when it is older than the target server's API.

---

# 5. Updating

```bash
wovyr upgrade            # self-update to latest on the current channel
wovyr upgrade --channel beta
```

Package-manager installs update through their package manager
(`brew upgrade wovyr`, `winget upgrade Wovyr.CLI`).

---

# 6. Verifying the Download

Releases are signed (Sigstore-style, consistent with
[plugin signing](../08-plugin-sdk/distribution.md#3-signing)):

```bash
wovyr verify ./wovyr                 # verifies the running binary's signature
# or manually compare the published SHA-256 checksum
```

---

# 7. Shell Completion

```bash
wovyr completion bash   > /etc/bash_completion.d/wovyr
wovyr completion zsh    > "${fpath[1]}/_wovyr"
wovyr completion fish   > ~/.config/fish/completions/wovyr.fish
wovyr completion powershell | Out-String | Invoke-Expression
```

---

# 8. First Run

```bash
wovyr version           # confirm install
wovyr login             # authenticate (see Configuration)
wovyr doctor            # environment diagnostics
```

`wovyr doctor` checks connectivity, auth, version compatibility, and local toolchain
prerequisites for `--local` execution and plugin builds.

---

# 9. Uninstall

```bash
wovyr uninstall         # removes the binary; prompts about ~/.wovyr config
# package managers: brew uninstall wovyr / winget uninstall Wovyr.CLI
```

---

# 10. Dependencies

- [`11-cli/configuration.md`](configuration.md)
- [`08-plugin-sdk/distribution.md`](../08-plugin-sdk/distribution.md#3-signing)

---

# 11. Related Documents

- [`11-cli/index.md`](index.md)
- [`11-cli/commands.md`](commands.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial CLI Installation |
