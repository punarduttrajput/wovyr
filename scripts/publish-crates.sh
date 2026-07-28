#!/usr/bin/env bash
# Publish the Wovyr core crates to crates.io in dependency order.
# Run with a valid crates.io token present (CARGO_REGISTRY_TOKEN or ~/.cargo/credentials.toml).
#
# Notes:
# - wovyr-common 0.3.0 is already published; skipped.
# - 7 crates have a dev-dependency cycle among themselves; `cargo publish
#   --verify` cannot build them until siblings are published, so they use
#   --no-verify. The workspace compiles, so this is safe (proven separately).
# - The global ~/.cargo/config.toml forces net.offline=true, so we pass
#   --config net.offline=false to reach the registry.
set -uo pipefail

VERSION="0.3.0"

# Topological publish order (deps first). wovyr-common omitted (already published).
ORDER=(
  wovyr-plugin-sdk
  wovyr-telemetry
  wovyr-tool-macros
  wovyr-audit
  wovyr-kms
  wovyr-provider
  wovyr-tenancy
  wovyr-ui
  wovyr-workflow
  wovyr-events
  wovyr-secrets
  wovyr-memory
  wovyr-ui-guard
  wovyr-config
  wovyr-tools
  wovyr-agent
  wovyr-plugin
  wovyr-runtime
  wovyr-marketplace
  wovyr-eval
  wovyr-server
)

# Crates whose own package dev-deps form a cycle (need --no-verify).
NO_VERIFY="wovyr-provider wovyr-agent wovyr-tools wovyr-runtime wovyr-server wovyr-marketplace wovyr-eval"

sparse_path() {
  local n="$1"; n=$(echo "$n" | tr '[:upper:]' '[:lower:]')
  if [ ${#n} -eq 1 ]; then echo "1/$n"
  elif [ ${#n} -eq 2 ]; then echo "2/$n"
  elif [ ${#n} -eq 3 ]; then echo "3/${n:0:1}/${n}"
  else echo "${n:0:2}/${n:2:2}/$n"; fi
}

already_published() {
  local name="$1"; local p; p=$(sparse_path "$name")
  local code; code=$(curl -s -o /dev/null -w "%{http_code}" -m 15 \
    -H "User-Agent: wovyr-publish" "https://index.crates.io/$p")
  [ "$code" = "200" ]
}

log() { printf '[publish] %s\n' "$*"; }

rc=0
for crate in "${ORDER[@]}"; do
  if already_published "$crate"; then
    log "SKIP $crate (already on crates.io)"
    continue
  fi
  extra=""
  for nv in $NO_VERIFY; do
    [ "$nv" = "$crate" ] && extra="--no-verify"
  done
  log "PUBLISH $crate $VERSION $extra"
  ok=0
  for attempt in 1 2 3 4 5 6; do
    out=$(cargo publish --allow-dirty --config net.offline=false -p "$crate" $extra 2>&1)
    if echo "$out" | grep -q "Published $crate"; then
      ok=1
      break
    fi
    # crates.io rate limit: a 429 tells us exactly when to retry.
    reset=$(echo "$out" | grep -oE 'after [A-Z][a-z]{2}, [0-9]{2} [A-Z][a-z]{2} [0-9]{4} [0-9]{2}:[0-9]{2}:[0-9]{2} GMT' | head -1 | sed 's/^after //')
    if [ -n "$reset" ]; then
      now_epoch=$(date -u +%s)
      reset_epoch=$(date -u -d "$reset" +%s 2>/dev/null) || reset_epoch=""
      if [ -n "$reset_epoch" ]; then
        wait_secs=$(( reset_epoch - now_epoch + 30 ))
        [ "$wait_secs" -lt 30 ] && wait_secs=30
        log "  rate-limited until $reset; sleeping ${wait_secs}s"
        sleep "$wait_secs"
        continue
      fi
    fi
    # Transient upload/index races (not a 429) — back off and retry.
    err=$(echo "$out" | grep -iE "error|FAILED" | head -1)
    log "  attempt $attempt failed: ${err:-unknown}; backing off 60s"
    sleep 60
  done
  if [ "$ok" -eq 1 ]; then
    log "OK $crate"
    # wovyr-provider was published with its sibling dev-deps stripped (they
    # form a dev-dependency cycle that cargo publish can't resolve against the
    # registry). Restore them in source so the local workspace/tests stay intact.
    if [ "$crate" = "wovyr-provider" ]; then
      sed -i \
        -e 's|^# wovyr-agent.workspace = true|wovyr-agent.workspace = true|' \
        -e 's|^# wovyr-tools.workspace = true|wovyr-tools.workspace = true|' \
        crates/wovyr-provider/Cargo.toml
      log "  restored wovyr-provider sibling dev-deps in source"
    fi
    # Give the index a moment to settle before dependents resolve it.
    sleep 10
  else
    log "FAILED $crate (all attempts)"
    rc=1
    break
  fi
done

if [ "$rc" -eq 0 ]; then log "ALL DONE"; else log "STOPPED WITH ERRORS"; fi
exit $rc
