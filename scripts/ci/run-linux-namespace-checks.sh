#!/usr/bin/env bash
set -euo pipefail

mode="${1:-}"
test_filter="${2:-}"
case "$mode" in
  test|test-filter|nextest|coverage) ;;
  *) printf 'usage: %s {test|test-filter <pattern>|nextest|coverage}\n' "$0" >&2; exit 2 ;;
esac
if [[ "$mode" == test-filter && -z "$test_filter" ]]; then
  printf 'test-filter requires a non-empty pattern\n' >&2
  exit 2
fi

if [[ "$(id -u)" -ne 0 ]]; then
  printf 'run this setup script as root inside the namespace-capable Ubuntu container\n' >&2
  exit 2
fi

apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential ca-certificates curl git pkg-config libssl-dev python3 util-linux >/dev/null

mkdir -p /tmp/tapid-cargo /tmp/tapid-rustup /tmp/tapid-target /tmp/tapid-home

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
        test-filter)
          cargo test --workspace --all-features --locked "$2"
          ;;
        nextest)
          cargo nextest run --workspace --all-features --locked
          ;;
        coverage)
          rustup component add llvm-tools-preview
          cargo llvm-cov --workspace --all-features --locked --lcov --output-path /tmp/tapid-coverage-lcov.info
          ;;
      esac
    ' _ "$mode" "$test_filter"

if [[ "$mode" == coverage ]]; then
  install -m 0644 /tmp/tapid-coverage-lcov.info /work/lcov.info
fi
