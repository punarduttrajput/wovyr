# Firecracker microVM sandbox assets

The [`FirecrackerSandbox`](../../crates/wovyr-tools/src/sandbox/firecracker.rs) runs a command
inside a [Firecracker](https://github.com/firecracker-microvm/firecracker) microVM. It
needs two host assets — a guest **kernel** and a **rootfs** carrying the Wovyr guest
agent (`init.sh`) as `/init`. Neither is checked in (they are large binaries); build
them with the steps below.

## Prerequisites

- `/dev/kvm` (hardware virtualization) and the `firecracker` binary on `PATH`.
- `docker` and `mkfs.ext4` (for the rootfs build).

## 1. Guest kernel

Download a prebuilt uncompressed `vmlinux` from the Firecracker CI bucket, e.g.:

```bash
mkdir -p deployment/firecracker/assets
curl -L -o deployment/firecracker/assets/vmlinux.bin \
  https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.12/x86_64/vmlinux-5.10.233
```

## 2. Rootfs (with the guest agent)

```bash
deployment/firecracker/build-rootfs.sh        # → deployment/firecracker/assets/rootfs.ext4
```

This exports an Alpine filesystem via Docker, installs [`init.sh`](init.sh) as `/init`,
and writes an ext4 image.

## 3. Run the capability-gated test

The microVM integration test skips unless KVM + `firecracker` are present **and** the
asset paths are provided:

```bash
WOVYR_FC_KERNEL=deployment/firecracker/assets/vmlinux.bin \
WOVYR_FC_ROOTFS=deployment/firecracker/assets/rootfs.ext4 \
  cargo test -p wovyr-tools --test sandbox_backends firecracker -- --nocapture
```

## Execution protocol

One-shot, over block devices (no vsock): the host writes the command to a read-only
input drive (`/dev/vdb`), boots the kernel + rootfs (whose `/init` is the agent), and
the agent runs the command, writes a base64-framed result to the writable output drive
(`/dev/vdc`), and reboots — which makes Firecracker exit. The host reads the result
back. The frame format is documented in [`init.sh`](init.sh).
