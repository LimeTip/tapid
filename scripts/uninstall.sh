#!/bin/sh
set -eu

INSTALL_DIR="${TAPID_INSTALL_DIR:-$HOME/.local/bin}"
BINARY="$INSTALL_DIR/tapid"

usage() {
  cat <<'USAGE'
Usage: uninstall.sh [--install-dir DIR]

Remove the Tapid CLI binary only. Project files such as node_modules,
.tapid-store, and tapid.lock are never removed by this script.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --install-dir)
      [ "$#" -ge 2 ] || { printf 'uninstaller: --install-dir requires a value\n' >&2; exit 1; }
      INSTALL_DIR="$2"
      BINARY="$INSTALL_DIR/tapid"
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'uninstaller: unknown option: %s\n' "$1" >&2; exit 1 ;;
  esac
done

case "$INSTALL_DIR" in
  /*) ;;
  *) printf 'uninstaller: install directory must be an absolute path\n' >&2; exit 1 ;;
esac

if [ -e "$BINARY" ] || [ -L "$BINARY" ]; then
  [ -L "$BINARY" ] && { printf 'uninstaller: refusing to remove symlink: %s\n' "$BINARY" >&2; exit 1; }
  [ -f "$BINARY" ] || { printf 'uninstaller: refusing to remove non-regular path: %s\n' "$BINARY" >&2; exit 1; }
  rm -f "$BINARY"
  printf 'Removed %s\n' "$BINARY"
else
  printf 'Tapid is not installed at %s\n' "$BINARY"
fi
