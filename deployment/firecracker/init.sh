#!/bin/sh
# Wovyr Firecracker guest agent — runs as PID 1 (init) inside the microVM.
#
# One-shot execution protocol over block devices (see crates/wovyr-tools/src/sandbox/firecracker.rs
# `FirecrackerSandbox`):
#   - /dev/vdb (input, read-only): the raw shell command, zero-padded.
#   - /dev/vdc (output, writable): the result, written as:
#       WOVYRR1
#       <exit code>
#       <base64(stdout) with no newlines>
#       <base64(stderr) with no newlines>
#       WOVYREOF
# The agent then reboots, which makes Firecracker exit; the host reads /dev/vdc back.

mount -t devtmpfs dev /dev 2>/dev/null
mount -t proc proc /proc 2>/dev/null
mount -t tmpfs tmp /tmp 2>/dev/null

CMD=$(dd if=/dev/vdb bs=4096 2>/dev/null | tr -d '\000')
OUT=$(eval "$CMD" 2>/tmp/err); RC=$?
ERR=$(cat /tmp/err 2>/dev/null)

{
  printf 'WOVYRR1\n'
  printf '%s\n' "$RC"
  printf '%s' "$OUT" | base64 | tr -d '\n'; printf '\n'
  printf '%s' "$ERR" | base64 | tr -d '\n'; printf '\n'
  printf 'WOVYREOF\n'
} > /dev/vdc 2>/dev/null
sync

# `reboot=k` in the kernel cmdline makes a guest reboot stop the microVM cleanly.
reboot -f 2>/dev/null
poweroff -f 2>/dev/null
while true; do :; done
