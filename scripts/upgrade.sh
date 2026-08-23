#!/bin/sh
set -eu

case "$0" in
  */*) SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)" ;;
  *) SCRIPT_DIR="" ;;
esac
if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/install.sh" ]; then
  exec "$SCRIPT_DIR/install.sh" "$@"
fi

REPO="${TAPID_REPO:-LimeTip/tapid}"
printf '%s' "$REPO" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' || {
  printf 'upgrader: repository must be OWNER/REPO\n' >&2
  exit 1
}
INSTALLER_URL="https://raw.githubusercontent.com/$REPO/main/scripts/install.sh"
tmp_file="$(mktemp "${TMPDIR:-/tmp}/tapid-upgrade.XXXXXX")"
cleanup() { rm -f "$tmp_file"; }
trap cleanup 0 1 2 15
command -v curl >/dev/null 2>&1 || { printf 'upgrader: curl is required\n' >&2; exit 1; }
curl -fsSL "$INSTALLER_URL" -o "$tmp_file" || { printf 'upgrader: could not download installer\n' >&2; exit 1; }
if sh "$tmp_file" "$@"; then
  status=0
else
  status=$?
fi
rm -f "$tmp_file"
trap - 0 1 2 15
exit "$status"
