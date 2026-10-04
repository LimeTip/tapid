#!/usr/bin/env bash
set -euo pipefail

mode="${1:-}"
case "$mode" in
  test|nextest|coverage) ;;
  *) printf 'usage: %s {test|nextest|coverage}\n' "$0" >&2; exit 2 ;;
esac

if [[ "$(id -u)" -ne 0 ]]; then
  printf 'run this setup script as root inside the namespace-capable Ubuntu container\n' >&2
  exit 2
fi

apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential ca-certificates curl git pkg-config libssl-dev python3 util-linux >/dev/null

workspace_uid="$(stat -c %u /work)"
workspace_gid="$(stat -c %g /work)"
if [[ "$workspace_uid" -eq 0 ]]; then
  workspace_uid=12345
  workspace_gid=12345
fi

mkdir -p /tmp/tapid-cargo /tmp/tapid-rustup /tmp/tapid-target /tmp/tapid-home
chown -R "$workspace_uid:$workspace_gid" /tmp/tapid-cargo /tmp/tapid-rustup /tmp/tapid-target /tmp/tapid-home

setpriv --reuid="$workspace_uid" --regid="$workspace_gid" --clear-groups \
  --inh-caps=+sys_admin --ambient-caps=+sys_admin -- \
  env HOME=/tmp/tapid-home \
    CARGO_HOME=/tmp/tapid-cargo \
    RUSTUP_HOME=/tmp/tapid-rustup \
    CARGO_TARGET_DIR=/tmp/tapid-target \
    PATH="${PATH:-/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin}" \
    bash -c '
      set -euo pipefail
      curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
      source "$CARGO_HOME/env"
      case "$1" in
        test)
          cargo test --workspace --all-features --locked
          ;;
        nextest)
          cargo nextest run --workspace --all-features --locked
          ;;
        coverage)
          rustup component add llvm-tools-preview
          cargo llvm-cov --workspace --all-features --locked --lcov --output-path /tmp/tapid-coverage-lcov.info
          ;;
      esac
    ' _ "$mode"

if [[ "$mode" == coverage ]]; then
  install -m 0644 /tmp/tapid-coverage-lcov.info /work/lcov.info
fi
