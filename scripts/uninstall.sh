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
      MARKER="$INSTALL_DIR/.tapid-managed"
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

# Produce literal, duplicate-safe POSIX activation without evaluating the path.
path_payload() {
  quoted_install="$(printf '%s' "$INSTALL_DIR" | sed "s/'/'\\\\''/g")"
  printf "case :\$%s: in *:'%s':*) ;; *) export PATH='%s'\$%s ;; esac" '{PATH-}' "$quoted_install" "$quoted_install" '{PATH+:$PATH}'
}

# Receipt fields are data, never shell input. Keep this parser identical in both
# standalone scripts; generated installers cannot depend on sibling files.
safe_path_ancestors() {
  ancestor="$1"
  while [ "$ancestor" != / ]; do
    [ ! -L "$ancestor" ] || return 1
    ancestor="${ancestor%/*}"; [ -n "$ancestor" ] || ancestor=/
  done
}

# A two-profile receipt contains two independently recoverable v1 records.
read_path_receipt() {
  if printf 'tapid-path-v1\nunix\ninstall\n2\n' | cmp -s - "$PATH_RECEIPT/manifest"; then
    multi_root="$PATH_RECEIPT"
    [ -d "$multi_root" ] && [ ! -L "$multi_root" ] && [ -O "$multi_root" ] || return 1
    safe_path_ancestors "$multi_root" || return 1
    multi_count=0
    for multi_entry in "$multi_root"/* "$multi_root"/.[!.]* "$multi_root"/..?*; do
      [ -e "$multi_entry" ] || { [ ! -L "$multi_entry" ] || return 1; continue; }
      case "${multi_entry##*/}" in manifest) [ -f "$multi_entry" ] && [ ! -L "$multi_entry" ] && [ -O "$multi_entry" ] || return 1 ;; 1|2) ;; *) return 1 ;; esac
      multi_count=$((multi_count + 1))
    done
    [ "$multi_count" -eq 3 ] || return 1
    PATH_RECEIPT="$multi_root/1"; read_profile_receipt || return 1
    first_target="$receipt_target"
    PATH_RECEIPT="$multi_root/2"; read_profile_receipt || return 1
    [ "$first_target" != "$receipt_target" ] || return 1
    PATH_RECEIPT="$multi_root"; receipt_records=2
  else
    read_profile_receipt || return 1
    receipt_records=1
  fi
}

read_profile_receipt() {
  [ -d "$PATH_RECEIPT" ] && [ ! -L "$PATH_RECEIPT" ] && [ -O "$PATH_RECEIPT" ] || return 1
  safe_path_ancestors "$PATH_RECEIPT" || return 1
  receipt_count=0
  for receipt_file in "$PATH_RECEIPT"/* "$PATH_RECEIPT"/.[!.]* "$PATH_RECEIPT"/..?*; do
    [ -e "$receipt_file" ] || { [ ! -L "$receipt_file" ] || return 1; continue; }
    case "${receipt_file##*/}" in manifest|install|target|payload|before|after|state) ;; *) return 1 ;; esac
    [ -f "$receipt_file" ] && [ ! -L "$receipt_file" ] && [ -O "$receipt_file" ] && [ -r "$receipt_file" ] || return 1
    receipt_limit=65536
    case "${receipt_file##*/}" in before|after) receipt_limit=8388608 ;; esac
    [ "$(wc -c < "$receipt_file" | tr -d '[:space:]')" -le "$receipt_limit" ] || return 1
    receipt_count=$((receipt_count + 1))
  done
  [ "$receipt_count" -eq 7 ] || return 1
  printf 'tapid-path-v1\nunix\ninstall\n1\n' | cmp -s - "$PATH_RECEIPT/manifest" || return 1
  printf '%s' "$INSTALL_DIR" | cmp -s - "$PATH_RECEIPT/install" || return 1
  receipt_target="$(cat "$PATH_RECEIPT/target")"
  case "$receipt_target" in /*) ;; *) return 1 ;; esac
  case "$receipt_target" in *'
'*|*"$(printf '\r')"*) return 1 ;; esac
  printf '%s' "$receipt_target" | cmp -s - "$PATH_RECEIPT/target" || return 1
  receipt_payload="$(cat "$PATH_RECEIPT/payload")"
  if [ "$receipt_payload" != "$(path_payload)" ]; then
    [ "$INSTALL_DIR" = "$HOME/.local/bin" ] && [ "$receipt_payload" = 'export PATH="$HOME/.local/bin:$PATH"' ] || return 1
  fi
  printf '%s\n' "$receipt_payload" | cmp -s - "$PATH_RECEIPT/payload" || return 1
  # The after snapshot must be exactly the before bytes plus the owned block.
  # Reject receipts that could otherwise replace a profile with arbitrary bytes.
  if grep -Fqx "$PATH_MARKER_BEGIN" "$PATH_RECEIPT/before" || grep -Fqx "$PATH_MARKER_END" "$PATH_RECEIPT/before"; then return 1; fi
  if printf 'preexisting\n' | cmp -s - "$PATH_RECEIPT/state"; then
    cmp -s "$PATH_RECEIPT/before" "$PATH_RECEIPT/after" || return 1
    grep -Fqx "$receipt_payload" "$PATH_RECEIPT/before" || { [ "$INSTALL_DIR" = "$HOME/.local/bin" ] && grep -Fqx 'export PATH="$HOME/.local/bin:$PATH"' "$PATH_RECEIPT/before"; } || return 1
    receipt_state=preexisting
    return 0
  fi
  {
    cat "$PATH_RECEIPT/before" &&
    { [ ! -s "$PATH_RECEIPT/before" ] || [ "$(tail -c 1 "$PATH_RECEIPT/before" | od -An -tu1 | tr -d '[:space:]')" = 10 ] || printf '\n'; } &&
    printf '%s\n' "$PATH_MARKER_BEGIN" "$receipt_payload" "$PATH_MARKER_END"
  } | cmp -s - "$PATH_RECEIPT/after" || return 1
  if printf 'pending\n' | cmp -s - "$PATH_RECEIPT/state"; then receipt_state=pending
  elif printf 'completed\n' | cmp -s - "$PATH_RECEIPT/state"; then receipt_state=completed
  else return 1; fi
}

remove_path_block() {
  PATH_RECEIPT="$INSTALL_DIR/.tapid-path-v1"
  receipt_present=0
  if [ -e "$PATH_RECEIPT" ] || [ -L "$PATH_RECEIPT" ]; then
    read_path_receipt || return 1
    receipt_present=1
  fi
  [ "$receipt_present" -eq 1 ] || [ "$INSTALL_DIR" = "$HOME/.local/bin" ] || return 0
  if [ "$receipt_present" -eq 1 ]; then
    if [ "$receipt_records" -eq 2 ]; then
      cleanup_root="$PATH_RECEIPT"
      for record in 1 2; do
        (PATH_RECEIPT="$cleanup_root/$record"; read_profile_receipt && PATH_RC="$receipt_target" && remove_profile_block) || return 1
      done
    else
      PATH_RC="$receipt_target"
      remove_profile_block
    fi
  else
    # Legacy installers had no receipt. Limit discovery to their default files;
    # exact payload validation below is still required before removing anything.
    for PATH_RC in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zprofile" "$HOME/.zshrc"; do
      remove_profile_block || return 1
    done
  fi
}

remove_profile_block() {
  [ "${receipt_state-}" != preexisting ] || return 0
  safe_path_ancestors "$PATH_RC" || return 1
  [ -e "$PATH_RC" ] || return 0
  [ ! -L "$PATH_RC" ] && [ -f "$PATH_RC" ] && [ -O "$PATH_RC" ] && [ -r "$PATH_RC" ] && [ -w "$PATH_RC" ] || return 1
  [ "$(wc -c < "$PATH_RC" | tr -d '[:space:]')" -le 8388608 ] || return 1
  if [ "$receipt_present" -eq 1 ] && [ "$receipt_state" = pending ]; then
    cmp -s "$PATH_RC" "$PATH_RECEIPT/before" && return 0
    cmp -s "$PATH_RC" "$PATH_RECEIPT/after" || return 1
  fi
  begin_count="$(grep -Fxc "$PATH_MARKER_BEGIN" "$PATH_RC" || true)"
  end_count="$(grep -Fxc "$PATH_MARKER_END" "$PATH_RC" || true)"
  [ "$begin_count" -eq 0 ] && [ "$end_count" -eq 0 ] && return 0
  [ "$begin_count" -eq 1 ] && [ "$end_count" -eq 1 ] || return 1
  (
    path_tmp="$(mktemp "$PATH_RC.tapid.XXXXXX")" || exit 1
    path_original="$(mktemp "$PATH_RC.tapid.XXXXXX")" || { rm -f "$path_tmp"; exit 1; }
    trap 'rm -f "$path_tmp" "$path_original"' 0
    cat "$PATH_RC" > "$path_original" && cmp -s "$PATH_RC" "$path_original" || exit 1
    final_byte="$(tail -c 1 "$path_original" | od -An -tu1 | tr -d '[:space:]')"
    line_count="$(awk 'END { print NR }' "$path_original")"
    TAPID_PATH_PAYLOAD="${receipt_payload:-export PATH=\"\$HOME/.local/bin:\$PATH\"}" awk -v begin="$PATH_MARKER_BEGIN" -v end="$PATH_MARKER_END" -v final="$final_byte" -v lines="$line_count" '
      $0 == begin { if (inside != 0 || seen) bad=1; inside=1; seen=1; next }
      $0 == end { if (inside != 2) bad=1; inside=0; next }
      inside == 1 { if ($0 != ENVIRON["TAPID_PATH_PAYLOAD"]) bad=1; inside=2; next }
      inside == 2 { bad=1; next }
      !inside { printf "%s%s", $0, (NR == lines && final != 10 ? "" : "\n") }
      END { if (inside != 0 || !seen) bad=1; exit bad }
    ' "$path_original" > "$path_tmp" || exit 1
    safe_path_ancestors "$PATH_RC" && [ -f "$PATH_RC" ] && [ -O "$PATH_RC" ] && cmp -s "$PATH_RC" "$path_original" || exit 1
    cat "$path_tmp" > "$path_original" || exit 1
    mv -f "$path_tmp" "$PATH_RC" || exit 1
    [ ! -L "$PATH_RC" ] && [ -f "$PATH_RC" ] && [ -O "$PATH_RC" ] && cmp -s "$path_original" "$PATH_RC"
  ) || return 1
  ! grep -Fqx "$PATH_MARKER_BEGIN" "$PATH_RC" && ! grep -Fqx "$PATH_MARKER_END" "$PATH_RC"
}

PATH_RECEIPT="$INSTALL_DIR/.tapid-path-v1"
if [ ! -e "$MARKER" ] && [ ! -L "$MARKER" ] && [ ! -e "$BINARY" ] && [ ! -L "$BINARY" ]; then
  # PATH writes precede executable activation. A valid receipt alone permits
  # cleanup after interruption, but never authorizes removal of a foreign binary.
  read_path_receipt || { printf 'uninstaller: refusing foreign install marker: %s\n' "$MARKER" >&2; exit 1; }
else
  [ -f "$MARKER" ] && [ ! -L "$MARKER" ] && [ -O "$MARKER" ] || { printf 'uninstaller: refusing foreign install marker: %s\n' "$MARKER" >&2; exit 1; }
  [ "$(cat "$MARKER")" = 'tapid-managed-v1' ] || { printf 'uninstaller: refusing invalid install marker: %s\n' "$MARKER" >&2; exit 1; }
fi

if [ -e "$BINARY" ] || [ -L "$BINARY" ]; then
  [ -L "$BINARY" ] && { printf 'uninstaller: refusing to remove symlink: %s\n' "$BINARY" >&2; exit 1; }
  [ -f "$BINARY" ] || { printf 'uninstaller: refusing to remove non-regular path: %s\n' "$BINARY" >&2; exit 1; }
fi

if ! remove_path_block; then
  printf 'uninstaller: refusing unsafe or unsupported shell startup file\n' >&2
  exit 1
fi

if [ -e "$BINARY" ]; then
  rm -f "$BINARY"
  printf 'Removed %s\n' "$BINARY"
else
  printf 'Tapid is not installed at %s\n' "$BINARY"
fi

if [ -d "$PATH_RECEIPT" ]; then
  read_path_receipt || exit 1
  if [ "$receipt_records" -eq 2 ]; then
    for record in 1 2; do
      rm -f "$PATH_RECEIPT/$record/manifest" "$PATH_RECEIPT/$record/install" "$PATH_RECEIPT/$record/target" "$PATH_RECEIPT/$record/payload" "$PATH_RECEIPT/$record/before" "$PATH_RECEIPT/$record/after" "$PATH_RECEIPT/$record/state"
      rmdir "$PATH_RECEIPT/$record"
    done
    rm -f "$PATH_RECEIPT/manifest"
    rmdir "$PATH_RECEIPT"
  else
    rm -f "$PATH_RECEIPT/manifest" "$PATH_RECEIPT/install" "$PATH_RECEIPT/target" "$PATH_RECEIPT/payload" "$PATH_RECEIPT/before" "$PATH_RECEIPT/after" "$PATH_RECEIPT/state"
    rmdir "$PATH_RECEIPT"
  fi
fi
rm -f "$MARKER"
