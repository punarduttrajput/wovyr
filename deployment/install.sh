#!/usr/bin/env bash
# Bare-metal systemd installer for the Wovyr AI Platform single-node appliance
# (RM-AIM-P3 DEP-301, ADR-0010 Path A).
#
# Installs the `wovyr` binary as a systemd-managed service running as a
# dedicated, unprivileged system user. Idempotent — safe to re-run (e.g. after
# building a new binary): re-running only updates the installed binary and
# unit file, and never touches existing `/var/lib/wovyr/.wovyr` durable state or
# an existing `/etc/wovyr/wovyr.env` (so operator edits to the env file survive
# an upgrade).
#
# Usage (run as root, e.g. via sudo):
#   sudo ./deployment/install.sh [--binary PATH] [--prefix DIR]
#
#   --binary PATH   Path to a pre-built `wovyr` release binary. Defaults to
#                    target/release/wovyr under the repo root (this script's
#                    grandparent directory); if that doesn't exist and cargo
#                    is on PATH, builds it first via
#                    `cargo build --release -p wovyr-cli`.
#   --prefix DIR    Install prefix for the binary. Default: /usr/local/bin.
#                    The installed unit's ExecStart is rewritten to match.
#
# What this does, in order:
#   1. Creates a dedicated system user+group `wovyr` (home: /var/lib/wovyr, no
#      login shell) if one doesn't already exist.
#   2. Creates /var/lib/wovyr/.wovyr (0700, owned by wovyr:wovyr) — the durable
#      state root every wovyr-server/wovyr-cli store lives under
#      (crates/wovyr-config/src/root.rs).
#   3. Installs (or builds + installs) the binary to <prefix>/wovyr.
#   4. Installs the systemd unit to /etc/systemd/system/wovyr.service.
#   5. Installs deployment/systemd/wovyr.env.example to /etc/wovyr/wovyr.env
#      (0640, root:wovyr) — only if that file doesn't already exist.
#   6. Runs `systemctl daemon-reload`.
#
# This script deliberately does NOT enable or start the service — review
# /etc/wovyr/wovyr.env first (the shipped default is loopback-only with
# disabled-loopback auth, safe only because nothing but this host can reach
# it). Once you're satisfied with the config:
#   sudo systemctl enable --now wovyr
#   sudo systemctl status wovyr
#   journalctl -u wovyr -f
#
# See docs/12-deployment/systemd.md for the full walkthrough.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BINARY_PATH=""
PREFIX="/usr/local/bin"

while [ $# -gt 0 ]; do
  case "$1" in
    --binary)
      BINARY_PATH="$2"
      shift 2
      ;;
    --prefix)
      PREFIX="$2"
      shift 2
      ;;
    -h | --help)
      sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

if [ "$(id -u)" -ne 0 ]; then
  echo "error: this script must be run as root (e.g. via sudo)" >&2
  exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
  echo "error: systemctl not found — this script targets systemd hosts only" >&2
  exit 1
fi

# --- 1. Resolve (and, if needed, build) the binary --------------------------

if [ -z "$BINARY_PATH" ]; then
  BINARY_PATH="$REPO_ROOT/target/release/wovyr"
fi

if [ ! -f "$BINARY_PATH" ]; then
  if command -v cargo >/dev/null 2>&1; then
    echo "==> $BINARY_PATH not found; building it via cargo build --release -p wovyr-cli"
    (cd "$REPO_ROOT" && cargo build --release -p wovyr-cli)
  else
    echo "error: binary not found at $BINARY_PATH and cargo is not on PATH" >&2
    echo "       build it first (cargo build --release -p wovyr-cli) or pass --binary" >&2
    exit 1
  fi
fi

# --- 2. System user + state directory ----------------------------------------

WOVYR_HOME="/var/lib/wovyr"

if ! getent group wovyr >/dev/null 2>&1; then
  echo "==> creating group 'wovyr'"
  groupadd --system wovyr
fi

if ! id -u wovyr >/dev/null 2>&1; then
  echo "==> creating system user 'wovyr' (home: $WOVYR_HOME, no login shell)"
  useradd --system --gid wovyr --home-dir "$WOVYR_HOME" --create-home \
    --shell /usr/sbin/nologin wovyr
fi

echo "==> ensuring $WOVYR_HOME/.wovyr exists with owner-only permissions"
install -d -m 0700 -o wovyr -g wovyr "$WOVYR_HOME/.wovyr"

# --- 3. Install the binary ----------------------------------------------------

echo "==> installing binary to $PREFIX/wovyr"
install -d -m 0755 "$PREFIX"
install -m 0755 "$BINARY_PATH" "$PREFIX/wovyr"

# --- 4. Install the systemd unit ----------------------------------------------

echo "==> installing systemd unit to /etc/systemd/system/wovyr.service"
# The shipped unit hardcodes ExecStart=/usr/local/bin/wovyr (the default
# --prefix); when a custom --prefix is given, rewrite that one line so the
# installed unit actually points at where the binary was just installed,
# instead of silently referencing a path that doesn't exist.
sed "s#^ExecStart=/usr/local/bin/wovyr #ExecStart=$PREFIX/wovyr #" \
  "$SCRIPT_DIR/systemd/wovyr.service" >/etc/systemd/system/wovyr.service
chmod 0644 /etc/systemd/system/wovyr.service

# --- 5. Install the environment file (never overwriting an existing one) ------

install -d -m 0755 /etc/wovyr
if [ -f /etc/wovyr/wovyr.env ]; then
  echo "==> /etc/wovyr/wovyr.env already exists — leaving it untouched"
else
  echo "==> installing default environment file to /etc/wovyr/wovyr.env"
  install -m 0640 -o root -g wovyr "$SCRIPT_DIR/systemd/wovyr.env.example" /etc/wovyr/wovyr.env
fi

# --- 6. Reload systemd ---------------------------------------------------------

echo "==> systemctl daemon-reload"
systemctl daemon-reload

cat <<EOF

Install complete.

Next steps:
  1. Review /etc/wovyr/wovyr.env (auth mode, TLS, provider API keys) before
     exposing this node beyond localhost.
  2. sudo systemctl enable --now wovyr
  3. sudo systemctl status wovyr
  4. curl http://127.0.0.1:8080/healthz   (or your configured WOVYR_BIND_ADDR)

See $REPO_ROOT/docs/12-deployment/systemd.md for the full walkthrough.
EOF
