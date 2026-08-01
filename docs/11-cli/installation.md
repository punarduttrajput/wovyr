<!--
File: docs/11-cli/installation.md
Document ID: CLI-001
-->

# CLI Installation

**Document ID:** CLI-001  
**File Path:** `docs/11-cli/installation.md`  
**Version:** 2.0.0  
**Status:** Shipped — every install method below is a real, published artifact.  
**Owner:** AI Platform Team  
**Last Updated:** 2026-08-01

---

# 1. Purpose

How to install, update, and verify the `wovyr` CLI.

> **Scope note.** Version 1.0.0 of this document described an install script at
> `get.wovyr.example.com`, a Homebrew tap, winget/Scoop packages, a
> `ghcr.io/wovyr-ai/cli` image, release channels, and the commands `wovyr
> upgrade`, `verify`, `completion`, `version`, `doctor`, and `uninstall` — none of
> which exist. It was rewritten to describe what actually ships.

---

# 2. Supported Platforms

Release binaries are built for:

| OS | Architecture | Artifact |
|----|--------------|----------|
| Linux | x86_64 | `wovyr-linux-x86_64.tar.gz` |
| macOS | aarch64 (Apple Silicon) | `wovyr-macos-aarch64.tar.gz` |
| Windows | x86_64 | `wovyr-windows-x86_64.zip` |

Other targets (Linux aarch64, Intel macOS) are not pre-built — install from
source with `cargo`, which works anywhere the Rust toolchain does.

The CLI is a single Rust binary. Optional features (`mistralrs`, `tiered-memory`,
`postgres`, `plugin-wasi`, `otlp`, `redis`) are compile-time and therefore only
available via a `cargo` install or a source build.

---

# 3. Install Methods

## 3.1 crates.io (recommended)

```bash
cargo install wovyr-cli          # installs a `wovyr` binary
```

Requires Rust 1.85+ (edition 2024). This is the path the
[README quickstart](../../README.md) uses.

With optional features:

```bash
cargo install wovyr-cli --features tiered-memory,postgres
```

## 3.2 Release binaries

Each `v*` tag publishes the archives in §2 to the
[GitHub Releases page](https://github.com/punarduttrajput/wovyr/releases), each
with a `.sha256` companion file. Download, verify (§5), extract, and put `wovyr`
on your `PATH`.

## 3.3 Container image

```bash
docker run --rm ghcr.io/punarduttrajput/wovyr:latest --version
```

Tagged with both the release version and `latest`. This image runs the server
too — see [docker.md](../12-deployment/docker.md).

## 3.4 From a clone

```bash
git clone https://github.com/punarduttrajput/wovyr && cd wovyr
cargo build -p wovyr-cli                 # target/debug/wovyr
```

Every `wovyr …` invocation in the docs is equivalent to
`cargo run -p wovyr-cli -- …`, so a clone needs no install step at all.

---

# 4. Versioning

There are no release channels. The CLI version is the workspace version, kept in
lockstep with the release tag (DX-101) — see [CHANGELOG.md](../../CHANGELOG.md).

```bash
wovyr --version
```

Note that the roadmap milestone names (`v1.0`…`v1.6`) are planning labels and run
ahead of the package version by design; the
[README](../../README.md#two-version-numbers-and-why) explains the split. The CLI
does **not** currently check its version against a target server's API version.

---

# 5. Verifying a Download

Release archives ship a SHA-256 checksum. Compare it yourself:

```bash
sha256sum -c wovyr-linux-x86_64.tar.gz.sha256      # Linux
shasum -a 256 -c wovyr-macos-aarch64.tar.gz.sha256 # macOS
Get-FileHash wovyr-windows-x86_64.zip -Algorithm SHA256   # Windows
```

There is no `wovyr verify` command and release binaries are **not** signed today.
The Sigstore-shaped keyless signing described in
[ADR-0009](../17-adr/ADR-0009-keyless-signing.md) applies to **plugin packages**
(`wovyr plugin keyless-sign`), not to the CLI binary itself.

---

# 6. Updating

Re-run the install method you used:

```bash
cargo install wovyr-cli --force      # crates.io
docker pull ghcr.io/punarduttrajput/wovyr:latest
```

There is no `wovyr upgrade` self-update command.

---

# 7. First Run

```bash
wovyr --version                      # confirm the install
wovyr agents run --local -f examples/agents/hello.yaml --input '{"message":"Hi"}'
```

The local run needs no server, no API key, and no login — it falls back to a
deterministic mock provider. To talk to a server, see
[configuration §4](configuration.md#4-authentication).

There is no `wovyr doctor`. If a local run misbehaves, set `WOVYR_LOG=debug`.

---

# 8. Shell Completion

Not implemented — there is no `wovyr completion` command.

---

# 9. Uninstall

```bash
cargo uninstall wovyr-cli            # or just delete the extracted binary
```

This leaves `~/.wovyr` in place. That directory holds real data — KMS root key,
secrets, memory, workflow checkpoints, installed plugins — so remove it only
deliberately, and take a [backup](../12-deployment/backup-and-restore.md) first
if any of it matters. There is no `wovyr uninstall` command.

---

# 10. Related Documents

- [`11-cli/index.md`](index.md)
- [`11-cli/configuration.md`](configuration.md)
- [`11-cli/commands.md`](commands.md)
- [`19-implementation-guide/release-process.md`](../19-implementation-guide/release-process.md)

---

# 11. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 2.0.0 | 2026-08-01 | Rewritten against real artifacts: `cargo install wovyr-cli`, the three published release archives, and `ghcr.io/punarduttrajput/wovyr`. Removed the fictional install script, Homebrew tap, winget/Scoop packages, release channels, and the `upgrade`/`verify`/`completion`/`version`/`doctor`/`uninstall` commands; corrected the signing claim to scope it to plugin packages |
| 1.0.0 | 2026-06-27 | Initial CLI Installation (target-state, largely unimplemented) |
