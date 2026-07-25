#!/usr/bin/env bash
# Build a minimal ext4 rootfs carrying the Wovyr guest agent (init.sh) as /init,
# for the Firecracker microVM sandbox. Uses Docker to export an Alpine filesystem and
# `mkfs.ext4 -d` to populate the image without a loop mount or root.
#
# Usage: deployment/firecracker/build-rootfs.sh [OUT_DIR]
# Produces $OUT_DIR/rootfs.ext4. Pair it with a guest kernel (see README).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="${1:-$HERE/assets}"
IMAGE="${WOVYR_FC_BASE_IMAGE:-alpine:latest}"
SIZE_MB="${WOVYR_FC_ROOTFS_MB:-128}"

mkdir -p "$OUT_DIR"
ROOTDIR="$(mktemp -d)"
trap 'rm -rf "$ROOTDIR"' EXIT

echo "exporting $IMAGE filesystem..."
cid="$(docker create "$IMAGE")"
docker export "$cid" | tar -C "$ROOTDIR" -xf -
docker rm "$cid" >/dev/null

install -m 0755 "$HERE/init.sh" "$ROOTDIR/init"

echo "building ext4 image (${SIZE_MB}MiB)..."
dd if=/dev/zero of="$OUT_DIR/rootfs.ext4" bs=1M count="$SIZE_MB" status=none
mkfs.ext4 -F -q -d "$ROOTDIR" "$OUT_DIR/rootfs.ext4"

echo "rootfs ready: $OUT_DIR/rootfs.ext4"
