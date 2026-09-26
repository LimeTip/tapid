#!/bin/sh
set -eu

INSTALL_DIR="${TAPID_INSTALL_DIR:-$HOME/.local/bin}"
BINARY="$INSTALL_DIR/tapid"
MARKER="$INSTALL_DIR/.tapid-managed"
PATH_MARKER_BEGIN="# tapid-path-managed-v1"
PATH_MARKER_END="# end tapid-path-managed-v1"

usage() {
  cat <<'USAGE'
Usage: uninstall.sh [--install-dir DIR]

Remove the Tapid CLI binary and its versioned managed PATH block. Project
files such as node_modules, .tapid-store, and tapid.lock are never removed.
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

remove_path_block() {
  shell_name="${SHELL-}"; shell_name="${shell_name##*/}"
  case "$shell_name" in
    bash)
      if [ -f "$HOME/.bash_profile" ]; then PATH_RC="$HOME/.bash_profile"; else PATH_RC="$HOME/.bashrc"; fi
      ;;
    sh|dash|ksh) PATH_RC="$HOME/.profile" ;;
    *) return 1 ;;
  esac
  [ -e "$PATH_RC" ] || return 0
  [ ! -L "$PATH_RC" ] && [ -f "$PATH_RC" ] && [ -O "$PATH_RC" ] && [ -r "$PATH_RC" ] && [ -w "$PATH_RC" ] || return 1
  begin_count="$(grep -Fxc "$PATH_MARKER_BEGIN" "$PATH_RC" || true)"
  end_count="$(grep -Fxc "$PATH_MARKER_END" "$PATH_RC" || true)"
  [ "$begin_count" -eq 0 ] && [ "$end_count" -eq 0 ] && return 0
  [ "$begin_count" -eq 1 ] && [ "$end_count" -eq 1 ] || return 1
  path_tmp="$(mktemp "$PATH_RC.tapid.XXXXXX")" || return 1
  if ! awk -v begin="$PATH_MARKER_BEGIN" -v end="$PATH_MARKER_END" '
    $0 == begin { if (inside != 0) bad=1; inside++; next }
    $0 == end { if (inside != 1) bad=1; inside=0; next }
    !inside { print }
    END { if (inside != 0) bad=1; exit bad }
  ' "$PATH_RC" > "$path_tmp"; then
    rm -f "$path_tmp"; return 1
  fi
  mv -f "$path_tmp" "$PATH_RC" || { rm -f "$path_tmp"; return 1; }
  ! grep -Fqx "$PATH_MARKER_BEGIN" "$PATH_RC" && ! grep -Fqx "$PATH_MARKER_END" "$PATH_RC"
}

if [ -e "$MARKER" ] || [ -L "$MARKER" ]; then
  [ -f "$MARKER" ] && [ ! -L "$MARKER" ] && [ -O "$MARKER" ] || { printf 'uninstaller: refusing foreign install marker: %s\n' "$MARKER" >&2; exit 1; }
  [ "$(cat "$MARKER")" = 'tapid-managed-v1' ] || { printf 'uninstaller: refusing invalid install marker: %s\n' "$MARKER" >&2; exit 1; }
fi

if [ -e "$BINARY" ] || [ -L "$BINARY" ]; then
  [ -L "$BINARY" ] && { printf 'uninstaller: refusing to remove symlink: %s\n' "$BINARY" >&2; exit 1; }
  [ -f "$BINARY" ] || { printf 'uninstaller: refusing to remove non-regular path: %s\n' "$BINARY" >&2; exit 1; }
  rm -f "$BINARY"
  printf 'Removed %s\n' "$BINARY"
else
  printf 'Tapid is not installed at %s\n' "$BINARY"
fi

if ! remove_path_block; then
  printf 'uninstaller: refusing unsafe or unsupported shell startup file\n' >&2
  exit 1
fi
[ -e "$MARKER" ] || [ -L "$MARKER" ] || exit 0
rm -f "$MARKER"
