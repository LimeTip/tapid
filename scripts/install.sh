#!/bin/sh
set -eu

REPO="${TAPID_REPO:-LimeTip/tapid}"
INSTALL_DIR="${TAPID_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="latest"
VERSION_SET=0
LEGACY_RELEASE=0
[ -z "${TAPID_REPO-}${TAPID_RELEASE_BASE_URL-}${TAPID_RELEASE_DISCOVERY_URL-}" ] || LEGACY_RELEASE=1
SOURCE_REF=""
SOURCE_REF_SET=0
STAGED_BINARY=""
STAGED_MARKER=""
PATH_UPDATED=0
PATH_RC=""
PATH_COMMAND=""
PATH_SHELL="auto"
CONFIGURE_PATH=0
PATH_MODE=""
PATH_MARKER_BEGIN="# tapid-path-managed-v1"
PATH_MARKER_END="# end tapid-path-managed-v1"
MAX_RECORD_BYTES=262144
MAX_CHECKSUM_BYTES=1048576
MAX_ARCHIVE_BYTES=536870912
MAX_BINARY_BYTES=536870912
# Release CI fills these values from the exact built archives before publication.
BOOTSTRAP_VERSION="@TAPID_BOOTSTRAP_VERSION@"
BOOTSTRAP_BASE_URL="@TAPID_BOOTSTRAP_BASE_URL@"
bootstrap_sha256() {
  case "$1" in
# @TAPID_BOOTSTRAP_PINS@
    *) return 1 ;;
  esac
}

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    fail "shasum or sha256sum is required"
  fi
}

extract_binary() {
  archive_path="$1"
  extract_path="$2"
  mkdir "$extract_path"
  members="$(tar -tzf "$archive_path")" || fail "cannot inspect release archive"
  printf '%s\n' "$members" | awk 'NF { count++; if ($0 != "tapid") invalid=1 } END { exit !(count == 1 && !invalid) }' || fail "release archive must contain exactly one member named tapid"
  entry_info="$(tar -tvzf "$archive_path")" || fail "cannot inspect release archive entry type"
  printf '%s\n' "$entry_info" | awk 'NF { count++; if (substr($0, 1, 1) != "-" || $NF != "tapid") invalid=1 } END { exit !(count == 1 && !invalid) }' || fail "release archive tapid member must be a regular file"
  probe_bytes="$(tar -xOzf "$archive_path" tapid | dd bs=1048576 count=513 2>/dev/null | wc -c | tr -d '[:space:]')"
  [ "$probe_bytes" -le "$MAX_BINARY_BYTES" ] || fail "release binary exceeds the size limit"
  (ulimit -f 1048576; tar -xzf "$archive_path" -C "$extract_path" tapid) || fail "cannot extract tapid"
  [ -f "$extract_path/tapid" ] && [ ! -L "$extract_path/tapid" ] || fail "release archive tapid member must be a regular file"
  [ "$(wc -c < "$extract_path/tapid" | tr -d '[:space:]')" -le "$MAX_BINARY_BYTES" ] || fail "release binary exceeds the size limit"
}


usage() {
  cat <<'USAGE'
Usage: install.sh [options]

Install the latest stable Tapid release by default.

Options:
  --version VERSION     Install a specific stable release tag, e.g. v0.1.0
  --source-ref REF      Build from a source branch, tag, or commit (development)
  --install-dir DIR     Install the binary into DIR (default: ~/.local/bin)
  --shell SHELL         PATH shell preference: auto|bash|zsh|fish|posix
  --shell-mode MODE     Startup mode: login|interactive|both
                        Default: both for Bash/Zsh/fish; login for POSIX
  --configure-path      Opt in to persistent shell startup-file changes
  --repo OWNER/REPO     Use the legacy GitHub release layout for this repository
  -h, --help            Show this help

Environment:
  TAPID_REPO, TAPID_INSTALL_DIR
  TAPID_RELEASE_RECORD_URL overrides the release record endpoint
  TAPID_RELEASE_BASE_URL, TAPID_RELEASE_DISCOVERY_URL select the legacy layout

Default installs use the release record at tapid.dev.
Explicit versions through 0.0.10 use historical GitHub assets and SHA256SUMS.
Release tags use the v0.1.0 form; --version accepts either v0.1.0 or 0.1.0.
USAGE
}

fail() { printf 'tapid installer: %s\n' "$*" >&2; exit 1; }

valid_version() {
  printf '%s\n' "$1" | LC_ALL=C awk '
    NR != 1 { bad=1 }
    {
      if ($0 !~ /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/) bad=1
      split($0, parts, ".")
      for (i=1; i<=3; i++) if (length(parts[i]) > 20 || (length(parts[i]) == 20 && "x" parts[i] > "x18446744073709551615")) bad=1
    }
    END { exit (bad || NR != 1) }
  '
}

# Require exactly one ordered managed PATH block without changing surrounding bytes.
valid_path_block() {
  TAPID_PATH_PAYLOAD="$path_line" awk -v begin="$PATH_MARKER_BEGIN" -v end="$PATH_MARKER_END" '
    BEGIN { line=ENVIRON["TAPID_PATH_PAYLOAD"] }
    $0 == begin { if (state != 0 || seen) bad=1; state=1; seen=1; next }
    $0 == end { if (state != 2) bad=1; state=0; next }
    state == 1 { if ($0 != line) bad=1; state=2; next }
    state == 2 { bad=1 }
    END { exit (bad || state != 0 || !seen) }
  ' "$1"
}

# Produce literal, duplicate-safe POSIX activation without evaluating the path.
path_payload() {
  if [ "${selected_shell-}" = fish ]; then fish_path_payload "$PATH_MODE"; return; fi
  quoted_install="$(printf '%s' "$INSTALL_DIR" | sed "s/'/'\\\\''/g")"
  printf "case :\$%s: in *:'%s':*) ;; *) export PATH='%s'\$%s ;; esac" '{PATH-}' "$quoted_install" "$quoted_install" '{PATH+:$PATH}'
}


fish_path_payload() {
  fish_quoted="$(printf '%s' "$INSTALL_DIR" | sed "s/\\\\/\\\\\\\\/g; s/'/\\\\'/g")"
  case "$1" in
    login) fish_guard='status is-login' ;;
    interactive) fish_guard='status is-interactive' ;;
    both) fish_guard='status is-login; or status is-interactive' ;;
  esac
  printf "if %s; fish_add_path --path --prepend -- '%s'; end" "$fish_guard" "$fish_quoted"
}

valid_receipt_payload() {
  [ "$receipt_payload" = "$(path_payload)" ] && return 0
  for fish_mode in login interactive both; do
    [ "$receipt_payload" != "$(fish_path_payload "$fish_mode")" ] || return 0
  done
  [ "$INSTALL_DIR" = "$HOME/.local/bin" ] && [ "$receipt_payload" = 'export PATH="$HOME/.local/bin:$PATH"' ]
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

# A migration publishes a complete replacement receipt before touching profiles.
# Keep the old receipt at a fixed recovery location across the two renames.
recover_path_migration() (
  migration_root="$INSTALL_DIR/.tapid-path-v1"
  migration_previous="$INSTALL_DIR/.tapid-path-previous-v1"
  [ -e "$migration_previous" ] || [ -L "$migration_previous" ] || exit 0
  PATH_RECEIPT="$migration_previous"
  read_profile_receipt || exit 1
  if [ ! -e "$migration_root" ] && [ ! -L "$migration_root" ]; then
    mv "$migration_previous" "$migration_root"
    exit $?
  fi
  PATH_RECEIPT="$migration_root"
  read_path_receipt && [ "$receipt_records" -eq 2 ] || exit 1
  migration_match=0
  for migration_record in 1 2; do
    if (for migration_field in manifest install target payload before after state; do
      cmp -s "$migration_previous/$migration_field" "$migration_root/$migration_record/$migration_field" || exit 1
    done); then migration_match=1; fi
  done
  [ "$migration_match" -eq 1 ] || exit 1
  # Retire by rename so interruption during deletion cannot corrupt recovery.
  migration_retired="$(mktemp -d "$INSTALL_DIR/.tapid-path-retired.XXXXXX")" || exit 1
  mv "$migration_previous" "$migration_retired/receipt" || { rmdir "$migration_retired"; exit 1; }
  rm -rf "$migration_retired"
)

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
  valid_receipt_payload || return 1
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

complete_path_receipt() {
  receipt_state_tmp="$(mktemp "$INSTALL_DIR/.tapid-path-state.XXXXXX")" || return 1
  printf 'completed\n' > "$receipt_state_tmp" && mv -f "$receipt_state_tmp" "$PATH_RECEIPT/state" || { rm -f "$receipt_state_tmp"; return 1; }
  printf 'completed\n' | cmp -s - "$PATH_RECEIPT/state"
}

# Missing profiles match only an empty before snapshot, without creating files
# until their pending receipt has been published.
profile_matches_snapshot() {
  safe_path_ancestors "$1" && [ ! -L "$1" ] || return 1
  if [ -e "$1" ]; then
    [ -f "$1" ] && [ -O "$1" ] && cmp -s "$1" "$2"
  else
    [ ! -s "$2" ]
  fi
}

# Stage and verify an owned startup profile before atomically adding Tapid's PATH block.
configure_profile_path() {
  [ "$CONFIGURE_PATH" -eq 1 ] || return 0
  PATH_RECEIPT="${RECORD_RECEIPT:-$INSTALL_DIR/.tapid-path-v1}"
  PATH_RC="$SELECTED_PROFILE"
  path_line="$(path_payload)"
  if [ -e "$PATH_RECEIPT" ] || [ -L "$PATH_RECEIPT" ]; then
    read_profile_receipt || return 1
    [ "$PATH_RC" = "$receipt_target" ] || return 1
    if [ "$selected_shell" = fish ]; then [ "$path_line" = "$receipt_payload" ] || return 1; fi
    path_line="$receipt_payload"
  fi
  safe_path_ancestors "$PATH_RC" && safe_path_ancestors "$INSTALL_DIR" || return 1
  [ -d "$INSTALL_DIR" ] && [ -O "$INSTALL_DIR" ] && [ -w "$INSTALL_DIR" ] || return 1
  [ "$(printf %s "$PATH_RC" | wc -c | tr -d '[:space:]')" -le 65536 ] || return 1
  [ "$(printf %s "$INSTALL_DIR" | wc -c | tr -d '[:space:]')" -le 65536 ] || return 1
  if [ -L "$PATH_RC" ] || { [ -e "$PATH_RC" ] && { [ ! -f "$PATH_RC" ] || [ ! -O "$PATH_RC" ] || [ ! -r "$PATH_RC" ] || [ ! -w "$PATH_RC" ]; }; }; then
    return 1
  fi
  profile_source="$PATH_RC"
  [ -e "$PATH_RC" ] || profile_source=/dev/null
  [ "$(wc -c < "$profile_source" | tr -d '[:space:]')" -le 8388608 ] || return 1
  if [ -d "$PATH_RECEIPT" ]; then
    if [ "$receipt_state" = preexisting ]; then
      grep -Fqx "$receipt_payload" "$PATH_RC" || { [ "$INSTALL_DIR" = "$HOME/.local/bin" ] && grep -Fqx 'export PATH="$HOME/.local/bin:$PATH"' "$PATH_RC"; }
      return $?
    fi
    if [ "$receipt_state" = pending ]; then
      if profile_matches_snapshot "$PATH_RC" "$PATH_RECEIPT/before"; then
        receipt_copy="$(mktemp "$PATH_RC.tapid.XXXXXX")" || return 1
        cat "$PATH_RECEIPT/after" > "$receipt_copy" &&
          profile_matches_snapshot "$PATH_RC" "$PATH_RECEIPT/before" && mv -f "$receipt_copy" "$PATH_RC" || { rm -f "$receipt_copy"; return 1; }
      fi
      [ ! -L "$PATH_RC" ] && [ -f "$PATH_RC" ] && [ -O "$PATH_RC" ] &&
        cmp -s "$PATH_RC" "$PATH_RECEIPT/after" || return 1
      complete_path_receipt || return 1
      profile_source="$PATH_RC"
    fi
  fi
  begin_count="$(grep -Fxc "$PATH_MARKER_BEGIN" "$profile_source" || true)"
  end_count="$(grep -Fxc "$PATH_MARKER_END" "$profile_source" || true)"
  [ "$begin_count" -eq 0 ] && [ "$end_count" -eq 0 ] || {
    [ "$begin_count" -eq 1 ] && [ "$end_count" -eq 1 ] || return 1
    if ! valid_path_block "$profile_source"; then
      [ "$INSTALL_DIR" = "$HOME/.local/bin" ] && [ ! -e "$PATH_RECEIPT" ] || return 1
      path_line='export PATH="$HOME/.local/bin:$PATH"'
      valid_path_block "$profile_source" || return 1
    fi
  }
  if [ "$begin_count" -eq 0 ]; then
    record_state=pending
    if grep -Fqx "$path_line" "$profile_source" || { [ "$INSTALL_DIR" = "$HOME/.local/bin" ] && grep -Fqx 'export PATH="$HOME/.local/bin:$PATH"' "$profile_source"; }; then record_state=preexisting; fi
    path_tmp="$(mktemp "$PATH_RC.tapid.XXXXXX")" || return 1
    if ! cat "$profile_source" > "$path_tmp" || ! cmp -s "$profile_source" "$path_tmp"; then
      rm -f "$path_tmp"; return 1
    fi
    if [ "$record_state" != preexisting ]; then
      if ! {
        { [ ! -s "$path_tmp" ] || [ "$(tail -c 1 "$path_tmp" | od -An -tu1 | tr -d '[:space:]')" = 10 ] || printf '\n'; } &&
        printf '%s\n%s\n%s\n' "$PATH_MARKER_BEGIN" "$path_line" "$PATH_MARKER_END"
      } >> "$path_tmp" || ! valid_path_block "$path_tmp"; then
        rm -f "$path_tmp"; return 1
      fi
    fi
    path_expected_hash="$(sha256_file "$path_tmp")" || { rm -f "$path_tmp"; return 1; }
    receipt_stage="$(mktemp -d "$INSTALL_DIR/.tapid-path-stage.XXXXXX")" || { rm -f "$path_tmp"; return 1; }
    if ! (
      umask 077
      printf 'tapid-path-v1\nunix\ninstall\n1\n' > "$receipt_stage/manifest" &&
      printf '%s' "$INSTALL_DIR" > "$receipt_stage/install" &&
      printf '%s' "$PATH_RC" > "$receipt_stage/target" &&
      printf '%s\n' "$path_line" > "$receipt_stage/payload" &&
      cat "$profile_source" > "$receipt_stage/before" &&
      cat "$path_tmp" > "$receipt_stage/after" &&
      printf '%s\n' "$record_state" > "$receipt_stage/state"
    ); then rm -rf "$receipt_stage"; rm -f "$path_tmp"; return 1; fi
    PATH_RECEIPT="$receipt_stage"
    if ! read_profile_receipt; then
      rm -rf "$receipt_stage"; rm -f "$path_tmp"; return 1
    fi
    PATH_RECEIPT="${RECORD_RECEIPT:-$INSTALL_DIR/.tapid-path-v1}"
    if [ -e "$PATH_RECEIPT" ] || [ -L "$PATH_RECEIPT" ]; then
      rm -rf "$receipt_stage"; rm -f "$path_tmp"; return 1
    fi
    mv "$receipt_stage" "$PATH_RECEIPT" || { rm -rf "$receipt_stage"; rm -f "$path_tmp"; return 1; }
    if [ "${PREPARE_PATH:-0}" -eq 1 ] || [ "$record_state" = preexisting ]; then rm -f "$path_tmp"; return 0; fi
    profile_matches_snapshot "$PATH_RC" "$PATH_RECEIPT/before" || { rm -f "$path_tmp"; return 1; }
    mv -f "$path_tmp" "$PATH_RC" || { rm -f "$path_tmp"; return 1; }
    [ ! -L "$PATH_RC" ] && [ -f "$PATH_RC" ] && [ -O "$PATH_RC" ] || return 1
    [ "$(sha256_file "$PATH_RC")" = "$path_expected_hash" ] || return 1
    complete_path_receipt || return 1
  fi
  PATH="$INSTALL_DIR${PATH:+:$PATH}"; export PATH; PATH_UPDATED=1
}

# Reconcile only exact pending snapshots or a still-valid completed block.
profile_receipt_matches_target() {
  read_profile_receipt && preflight_profile "$receipt_target" || return 1
  if [ "$receipt_state" = pending ]; then
    profile_matches_snapshot "$receipt_target" "$PATH_RECEIPT/before" || cmp -s "$receipt_target" "$PATH_RECEIPT/after"
  elif [ "$receipt_state" = preexisting ]; then
    grep -Fqx "$receipt_payload" "$receipt_target" || { [ "$INSTALL_DIR" = "$HOME/.local/bin" ] && grep -Fqx 'export PATH="$HOME/.local/bin:$PATH"' "$receipt_target"; }
  else
    path_line="$receipt_payload"; valid_path_block "$receipt_target"
  fi
}

# Preflight the entire selected set before preparing or applying any record.
preflight_profile() {
  safe_path_ancestors "$1" || return 1
  [ -d "${1%/*}" ] && [ -O "${1%/*}" ] && [ -w "${1%/*}" ] || return 1
  [ "$(printf %s "$1" | wc -c | tr -d '[:space:]')" -le 65536 ] || return 1
  [ ! -L "$1" ] || return 1
  if [ -e "$1" ]; then
    [ -f "$1" ] && [ -O "$1" ] && [ -r "$1" ] && [ -w "$1" ] || return 1
    [ "$(wc -c < "$1" | tr -d '[:space:]')" -le 8388608 ] || return 1
  fi
}

configure_path() {
  [ "$CONFIGURE_PATH" -eq 1 ] || return 0
  recover_path_migration || return 1
  login_profile="$HOME/.profile"
  case "$selected_shell" in
    bash)
      for candidate in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
        if [ -e "$candidate" ] || [ -L "$candidate" ]; then login_profile="$candidate"; break; fi
      done
      interactive_profile="$HOME/.bashrc" ;;
    zsh) login_profile="$zsh_directory/.zprofile"; interactive_profile="$zsh_directory/.zshrc" ;;
    fish)
      safe_path_ancestors "$fish_directory" || return 1
      mkdir -p "$fish_directory/fish" || return 1
      SELECTED_PROFILE="$fish_directory/fish/config.fish"
      preflight_profile "$SELECTED_PROFILE" || return 1
      printf 'PATH preference: fish; startup mode: %s\n' "$PATH_MODE"
      configure_profile_path; return $? ;;
    posix) ;;
    *) return 0 ;;
  esac
  printf 'PATH preference: %s; startup mode: %s\n' "$selected_shell" "$PATH_MODE"
  SELECTED_PROFILE="$login_profile"
  [ "$PATH_MODE" != interactive ] || SELECTED_PROFILE="$interactive_profile"
  preflight_profile "$SELECTED_PROFILE" || return 1
  if [ "$PATH_MODE" != both ]; then configure_profile_path; return $?; fi
  preflight_profile "$interactive_profile" || return 1
  PATH_RECEIPT="$INSTALL_DIR/.tapid-path-v1"
  if [ -e "$PATH_RECEIPT" ] || [ -L "$PATH_RECEIPT" ]; then
    read_path_receipt || return 1
    if [ "$receipt_records" -eq 1 ]; then
      migration_old="$PATH_RECEIPT"
      migration_old_target="$receipt_target"
      case "$migration_old_target" in
        "$login_profile") migration_slot=1; migration_new="$interactive_profile" ;;
        "$interactive_profile") migration_slot=2; migration_new="$login_profile" ;;
        *) return 1 ;;
      esac
      # Validate every current target before publishing migration state.
      (PATH_RECEIPT="$migration_old"; profile_receipt_matches_target) || return 1
      if [ -e "$migration_new" ] && { grep -Fqx "$PATH_MARKER_BEGIN" "$migration_new" || grep -Fqx "$PATH_MARKER_END" "$migration_new"; }; then return 1; fi
      multi_stage="$(mktemp -d "$INSTALL_DIR/.tapid-path-stage.XXXXXX")" || return 1
      if ! (
        umask 077
        printf 'tapid-path-v1\nunix\ninstall\n2\n' > "$multi_stage/manifest" &&
        mkdir "$multi_stage/$migration_slot" &&
        cp "$migration_old/manifest" "$migration_old/install" "$migration_old/target" "$migration_old/payload" "$migration_old/before" "$migration_old/after" "$migration_old/state" "$multi_stage/$migration_slot/" &&
        (SELECTED_PROFILE="$migration_new"; RECORD_RECEIPT="$multi_stage/$((3 - migration_slot))"; PREPARE_PATH=1; configure_profile_path)
      ); then rm -rf "$multi_stage"; return 1; fi
      (PATH_RECEIPT="$multi_stage"; read_path_receipt) || { rm -rf "$multi_stage"; return 1; }
      # Recheck the source receipt and both targets after staging.
      (PATH_RECEIPT="$migration_old"; read_profile_receipt) || { rm -rf "$multi_stage"; return 1; }
      for migration_field in manifest install target payload before after state; do
        cmp -s "$migration_old/$migration_field" "$multi_stage/$migration_slot/$migration_field" || { rm -rf "$multi_stage"; return 1; }
      done
      for record in 1 2; do
        (PATH_RECEIPT="$multi_stage/$record"; profile_receipt_matches_target) || { rm -rf "$multi_stage"; return 1; }
      done
      migration_previous="$INSTALL_DIR/.tapid-path-previous-v1"
      [ ! -e "$migration_previous" ] && [ ! -L "$migration_previous" ] || { rm -rf "$multi_stage"; return 1; }
      mv "$migration_old" "$migration_previous" || { rm -rf "$multi_stage"; return 1; }
      mv "$multi_stage" "$migration_old" || return 1
      recover_path_migration || return 1
      PATH_RECEIPT="$migration_old"
    fi
  fi
  if [ ! -e "$PATH_RECEIPT" ] && [ ! -L "$PATH_RECEIPT" ]; then
    multi_stage="$(mktemp -d "$INSTALL_DIR/.tapid-path-stage.XXXXXX")" || return 1
    # Unreceipted blocks require explicit legacy migration, never adoption.
    for target in "$login_profile" "$interactive_profile"; do
      if [ -e "$target" ] && { grep -Fqx "$PATH_MARKER_BEGIN" "$target" || grep -Fqx "$PATH_MARKER_END" "$target"; }; then rm -rf "$multi_stage"; return 1; fi
    done
    if ! (
      umask 077
      printf 'tapid-path-v1\nunix\ninstall\n2\n' > "$multi_stage/manifest" &&
      (SELECTED_PROFILE="$login_profile"; RECORD_RECEIPT="$multi_stage/1"; PREPARE_PATH=1; configure_profile_path) &&
      (SELECTED_PROFILE="$interactive_profile"; RECORD_RECEIPT="$multi_stage/2"; PREPARE_PATH=1; configure_profile_path)
    ); then rm -rf "$multi_stage"; return 1; fi
    PATH_RECEIPT="$multi_stage"
    read_path_receipt || { rm -rf "$multi_stage"; return 1; }
    PATH_RECEIPT="$INSTALL_DIR/.tapid-path-v1"
    [ ! -e "$PATH_RECEIPT" ] && [ ! -L "$PATH_RECEIPT" ] || { rm -rf "$multi_stage"; return 1; }
    mv "$multi_stage" "$PATH_RECEIPT" || return 1
  fi
  read_path_receipt && [ "$receipt_records" -eq 2 ] || return 1
  multi_root="$PATH_RECEIPT"
  printf '%s' "$login_profile" | cmp -s - "$multi_root/1/target" || return 1
  printf '%s' "$interactive_profile" | cmp -s - "$multi_root/2/target" || return 1
  # Validate both records and current targets before resuming either write.
  for record in 1 2; do
    (PATH_RECEIPT="$multi_root/$record"; profile_receipt_matches_target) || return 1
  done
  (SELECTED_PROFILE="$login_profile"; RECORD_RECEIPT="$multi_root/1"; configure_profile_path) || return 1
  (SELECTED_PROFILE="$interactive_profile"; RECORD_RECEIPT="$multi_root/2"; configure_profile_path) || return 1
  PATH_RC="$login_profile and $interactive_profile"; PATH_UPDATED=1
}

print_path_guidance() {
  PATH_COMMAND="$(path_payload)"
  if [ "$selected_shell" = fish ]; then
    fish_path_payload "$PATH_MODE" >/dev/null
    PATH_COMMAND="fish_add_path --path --prepend -- '$fish_quoted'"
  fi
  if [ "$PATH_UPDATED" -eq 1 ]; then
    printf 'Tapid was installed and PATH persistence was verified in %s.\n' "$PATH_RC"
    printf 'To enable it in the current shell, run: %s\n' "$PATH_COMMAND"
  else
    printf 'Persistent PATH was not changed.\n'
    printf 'To enable Tapid in the selected shell (POSIX if unknown), run: %s\n' "$PATH_COMMAND"
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) [ "$#" -ge 2 ] || fail "--version requires a value"; VERSION="$2"; VERSION_SET=1; shift 2 ;;
    --source-ref) [ "$#" -ge 2 ] || fail "--source-ref requires a value"; SOURCE_REF="$2"; SOURCE_REF_SET=1; shift 2 ;;
    --install-dir) [ "$#" -ge 2 ] || fail "--install-dir requires a value"; INSTALL_DIR="$2"; shift 2 ;;
    --shell) [ "$#" -ge 2 ] || fail "--shell requires a value"; PATH_SHELL="$2"; shift 2 ;;
    --shell-mode) [ "$#" -ge 2 ] || fail "--shell-mode requires a value"; [ -n "$2" ] || fail "--shell-mode requires a non-empty value"; PATH_MODE="$2"; shift 2 ;;
    --configure-path) CONFIGURE_PATH=1; shift ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a value"; REPO="$2"; LEGACY_RELEASE=1; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown option: $1" ;;
  esac
done

case "$PATH_SHELL" in
  auto|bash|zsh|fish|posix) ;;
  *) fail "--shell must be auto, bash, zsh, fish, or posix" ;;
esac

selected_shell="$PATH_SHELL"
if [ "$selected_shell" = auto ]; then selected_shell="${SHELL-}"; selected_shell="${selected_shell##*/}"; fi
case "$selected_shell" in sh|dash|ksh) selected_shell=posix ;; esac
zsh_directory="$HOME"
if [ "$selected_shell" = zsh ] && [ "${ZDOTDIR+x}" = x ]; then
  case "$ZDOTDIR" in /*) ;; *) fail "ZDOTDIR must be an absolute non-empty directory" ;; esac
  case "$ZDOTDIR" in *'
'*|*"$(printf '\r')"*) fail "ZDOTDIR cannot contain line breaks" ;; esac
  zsh_directory="$ZDOTDIR"
fi
case "$PATH_MODE" in
  '') case "$selected_shell" in bash|zsh|fish) PATH_MODE=both ;; *) PATH_MODE=login ;; esac ;;
  login|interactive|both) case "$selected_shell" in bash|zsh|fish) ;; posix) [ "$PATH_MODE" = login ] || fail "POSIX supports only login mode" ;; *) fail "--shell-mode requires a supported shell" ;; esac ;;
  *) fail "--shell-mode must be login, interactive, or both" ;;
esac

if [ "$selected_shell" = fish ]; then
  fish_directory="${XDG_CONFIG_HOME-$HOME/.config}"
  case "$fish_directory" in /*) ;; *) fail "XDG_CONFIG_HOME must be an absolute non-empty directory" ;; esac
  case "$fish_directory" in *'
'*|*"$(printf '\r')"*) fail "XDG_CONFIG_HOME cannot contain line breaks" ;; esac
  if [ "$CONFIGURE_PATH" -eq 1 ]; then
    command -v fish >/dev/null 2>&1 &&
      fish --no-config -c 'functions -q fish_add_path; and fish_add_path --path --prepend -- /; and contains -- / $PATH' >/dev/null 2>&1 ||
      fail "fish requires fish_add_path with --path and --prepend support"
  fi
fi

case "$REPO" in ''|*[!A-Za-z0-9_./-]*|/*|*/|*/*/*) fail "repository must be OWNER/REPO" ;; esac
case "$REPO" in */*) ;; *) fail "repository must be OWNER/REPO" ;; esac
[ "$VERSION_SET" -eq 0 ] || [ "$SOURCE_REF_SET" -eq 0 ] || fail "use either --version or --source-ref, not both"
case "$INSTALL_DIR" in /*) ;; *) fail "install directory must be an absolute path" ;; esac
case "$INSTALL_DIR" in
  *:*|*'
'*|*"$(printf '\r')"*) fail "install directory cannot contain PATH delimiters or line breaks" ;;
esac
mkdir -p "$INSTALL_DIR"
if [ -e "$INSTALL_DIR/tapid" ] || [ -L "$INSTALL_DIR/tapid" ]; then
  [ -f "$INSTALL_DIR/tapid" ] && [ ! -L "$INSTALL_DIR/tapid" ] || fail "existing Tapid destination must be a regular file"
fi

if [ "$SOURCE_REF_SET" -eq 1 ]; then
  [ -n "$SOURCE_REF" ] || fail "--source-ref requires a non-empty value"
  case "$SOURCE_REF" in -*) fail "source ref must not start with '-'" ;; esac
  command -v cargo >/dev/null 2>&1 || fail "cargo is required for --source-ref"
  command -v git >/dev/null 2>&1 || fail "git is required for --source-ref"
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tapid-install.XXXXXX")"
  cleanup() { rm -rf "$tmp_dir"; [ -z "$STAGED_BINARY" ] || rm -f "$STAGED_BINARY"; [ -z "$STAGED_MARKER" ] || rm -f "$STAGED_MARKER"; }
  trap cleanup 0 1 2 15
  git clone --filter=blob:none --no-checkout "https://github.com/$REPO.git" "$tmp_dir/tapid"
  if ! git -C "$tmp_dir/tapid" checkout --detach "$SOURCE_REF"; then
    git -C "$tmp_dir/tapid" fetch --filter=blob:none origin "$SOURCE_REF" || fail "could not find source ref $SOURCE_REF in $REPO"
    git -C "$tmp_dir/tapid" checkout --detach "$SOURCE_REF" || fail "could not check out source ref $SOURCE_REF in $REPO"
  fi
  cargo install --path "$tmp_dir/tapid/crates/tapid-cli" --locked --root "$tmp_dir/root"
  STAGED_BINARY="$(mktemp "$INSTALL_DIR/.tapid.tmp.XXXXXX")"
  STAGED_MARKER="$(mktemp "$INSTALL_DIR/.tapid-marker.tmp.XXXXXX")"
  install -m 0755 "$tmp_dir/root/bin/tapid" "$STAGED_BINARY"
  configure_path || fail 'could not safely update the selected shell startup file'
  printf 'tapid-managed-v1\n' > "$STAGED_MARKER"
  mv -f "$STAGED_MARKER" "$INSTALL_DIR/.tapid-managed"; STAGED_MARKER=""
  mv -f "$STAGED_BINARY" "$INSTALL_DIR/tapid"; STAGED_BINARY=""
  printf 'Installed Tapid from %s into %s/tapid\n' "$SOURCE_REF" "$INSTALL_DIR"
  print_path_guidance
  exit 0
fi

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"
if [ "$VERSION" != latest ]; then
  case "$VERSION" in ''|*[!v0-9.]*) fail "version must be a stable release such as v0.1.0" ;; esac
  valid_version "${VERSION#v}" || fail "version must be a stable release such as v0.1.0"
  VERSION="${VERSION#v}"
  # These published versions predate release records. Never infer fallback from a failed request.
  if [ -z "${TAPID_RELEASE_RECORD_URL-}" ]; then
    case "$VERSION" in 0.0.[0-9]|0.0.10) LEGACY_RELEASE=1 ;; esac
  fi
fi
if [ -n "${TAPID_RELEASE_RECORD_URL-}" ] && [ "$LEGACY_RELEASE" -eq 1 ]; then
  fail "release record URL cannot be combined with legacy release overrides"
fi

case "$(uname -s):$(uname -m)" in
  Darwin:arm64|Darwin:aarch64) target="aarch64-apple-darwin" ;;
  Darwin:x86_64) target="x86_64-apple-darwin" ;;
  Linux:x86_64|Linux:amd64) target="x86_64-unknown-linux-gnu" ;;
  Linux:arm64|Linux:aarch64) target="aarch64-unknown-linux-gnu" ;;
  *) fail "unsupported platform: $(uname -s) $(uname -m)" ;;
esac

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tapid-install.XXXXXX")"
cleanup() { rm -rf "$tmp_dir"; [ -z "$STAGED_BINARY" ] || rm -f "$STAGED_BINARY"; [ -z "$STAGED_MARKER" ] || rm -f "$STAGED_MARKER"; }
trap cleanup 0 1 2 15
expected_size=""
if [ "$LEGACY_RELEASE" -eq 1 ]; then
  RELEASE_BASE_URL="${TAPID_RELEASE_BASE_URL:-https://github.com/$REPO/releases/download}"
  RELEASE_DISCOVERY_URL="${TAPID_RELEASE_DISCOVERY_URL:-https://github.com/$REPO/releases/latest}"
  case "$RELEASE_BASE_URL" in https://*) ;; *) fail "stable release base URL must use HTTPS" ;; esac
  case "$RELEASE_DISCOVERY_URL" in https://*) ;; *) fail "stable release discovery URL must use HTTPS" ;; esac
  if [ "$VERSION" = latest ]; then
    resolved_url="$(curl -fsSIL --connect-timeout 10 --max-time 30 -o /dev/null -w '%{url_effective}' "$RELEASE_DISCOVERY_URL" 2>/dev/null)" || fail "could not contact the stable release discovery endpoint"
    case "$resolved_url" in */releases/tag/*) VERSION="${resolved_url##*/tag/}" ;; *) fail "stable release discovery endpoint did not resolve a release tag" ;; esac
  fi
  case "$VERSION" in ''|*[!v0-9.]*) fail "version must be a stable release such as v0.1.0" ;; esac
  valid_version "${VERSION#v}" || fail "version must be a stable release such as v0.1.0"
  VERSION="${VERSION#v}"
  archive="tapid-$VERSION-$target.tar.gz"
  base="$RELEASE_BASE_URL/v$VERSION"
  curl -fsSL --connect-timeout 10 --max-time 60 --proto '=https' --proto-redir '=https' --max-filesize "$MAX_CHECKSUM_BYTES" "$base/SHA256SUMS" -o "$tmp_dir/SHA256SUMS" 2>/dev/null || fail "could not download SHA256SUMS"
  [ "$(wc -c < "$tmp_dir/SHA256SUMS" | tr -d '[:space:]')" -le "$MAX_CHECKSUM_BYTES" ] || fail "SHA256SUMS exceeds the size limit"
  expected="$(awk -v name="$archive" '{ hash=substr($0,1,64); separator=substr($0,65,2); file=substr($0,67); if (length(hash)==64 && hash !~ /[^0-9a-f]/ && separator=="  " && file==name) { print hash; count++ } } END { if (count != 1) exit 1 }' "$tmp_dir/SHA256SUMS")" || fail "SHA256SUMS does not contain exactly one checksum for $archive"
  printf '%s' "$expected" | grep -Eq '^[0-9a-f]{64}$' || fail "invalid SHA-256 checksum for $archive"
  archive_url="$base/$archive"
else
  record_url="${TAPID_RELEASE_RECORD_URL:-https://tapid.dev/releases/v1/latest.tsv}"
  if [ -z "${TAPID_RELEASE_RECORD_URL-}" ] && [ "$VERSION" != latest ]; then
    record_url="https://tapid.dev/releases/v1/v$VERSION.tsv"
  fi
  # Validate the discovery address before downloading metadata.
  printf '%s\n' "$record_url" | LC_ALL=C awk '
    NR != 1 || $0 !~ /^https:\/\/[A-Za-z0-9][A-Za-z0-9.-]*(:[0-9]+)?(\/.*)?$/ || /[^!-~]/ || /[@?#\\]/ { bad=1 }
    END { exit bad }
  ' || fail "release record URL must be a safe HTTPS URL"
  curl -fsSL --connect-timeout 10 --max-time 60 --proto '=https' --proto-redir '=https' --max-filesize "$MAX_RECORD_BYTES" "$record_url" -o "$tmp_dir/release.tsv" 2>/dev/null || fail "could not download release record"
  curl -fsSL --connect-timeout 10 --max-time 60 --proto '=https' --proto-redir '=https' --max-filesize "$MAX_RECORD_BYTES" "$record_url.sig" -o "$tmp_dir/release.tsv.sig" 2>/dev/null || fail "could not download release record signature"
  [ "$(wc -c < "$tmp_dir/release.tsv" | tr -d '[:space:]')" -le "$MAX_RECORD_BYTES" ] || fail "release record exceeds the size limit"
  [ -s "$tmp_dir/release.tsv.sig" ] || fail "release record signature is empty"
  bootstrap_hash="$(bootstrap_sha256 "$target")" || fail "installer bootstrap pins are missing; use a published release installer"
  bootstrap_archive="tapid-$BOOTSTRAP_VERSION-$target.tar.gz"
  bootstrap_path="$tmp_dir/bootstrap.tar.gz"
  curl -fsSL --connect-timeout 10 --max-time 60 --proto '=https' --proto-redir '=https' --max-filesize "$MAX_ARCHIVE_BYTES" "$BOOTSTRAP_BASE_URL/$bootstrap_archive" -o "$bootstrap_path" 2>/dev/null || fail "could not download installer bootstrap"
  [ "$(wc -c < "$bootstrap_path" | tr -d '[:space:]')" -le "$MAX_ARCHIVE_BYTES" ] || fail "bootstrap archive exceeds the size limit"
  [ "$(sha256_file "$bootstrap_path")" = "$bootstrap_hash" ] || fail "bootstrap archive checksum mismatch"
  extract_binary "$bootstrap_path" "$tmp_dir/bootstrap"
  chmod 0755 "$tmp_dir/bootstrap/tapid"
  "$tmp_dir/bootstrap/tapid" __verify-release-record "$tmp_dir/release.tsv" "$tmp_dir/release.tsv.sig" || fail "release record signature verification failed"
  [ "$(tail -c 1 "$tmp_dir/release.tsv" | od -An -tu1 | tr -d '[:space:]')" = 10 ] || fail "release record must end with a newline"
  # Some awk implementations truncate strings at NUL. Check raw bytes first.
  [ "$(LC_ALL=C tr -d '\011\012\040-\176' < "$tmp_dir/release.tsv" | wc -c | tr -d '[:space:]')" = 0 ] || fail "release record must contain ASCII fields and LF lines"
  LC_ALL=C awk -F '\t' -v target="$target" -v requested="$VERSION" -v limit="$MAX_ARCHIVE_BYTES" '
    function invalid() { bad=1; exit 1 }
    NR == 1 {
      if (NF != 2 || $1 != "tapid-release-v1" || $2 !~ /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/) invalid()
      version=$2
      split(version, parts, ".")
      for (i=1; i<=3; i++) if (length(parts[i]) > 20 || (length(parts[i]) == 20 && "x" parts[i] > "x18446744073709551615")) invalid()
      if (requested != "latest" && requested != version) invalid()
      next
    }
    {
      if (NF != 5 || $1 !~ /^[A-Za-z0-9_-]+$/ || seen_target[$1]++ || seen_name[$2]++) invalid()
      if ($2 != "tapid-" version "-" $1 ".tar.gz") invalid()
      if ($3 !~ /^[1-9][0-9]*$/ || length($3) > 9 || $3+0 > limit) invalid()
      if (length($4) != 64 || $4 ~ /[^0-9a-f]/) invalid()
      if ($5 !~ /^https:\/\/[A-Za-z0-9][A-Za-z0-9.-]*(:[0-9]+)?(\/.*)?$/ || $5 ~ /[^!-~]/ || $5 ~ /[@?#\\]/) invalid()
      if ($1 == target) { selected=$0; found=1 }
    }
    END { if (bad || !found) exit 1; print version; print selected }
  ' "$tmp_dir/release.tsv" > "$tmp_dir/selected" || fail "invalid release record or missing platform"
  VERSION="$(sed -n '1p' "$tmp_dir/selected")"
  archive="$(awk -F '\t' 'NR == 2 { print $2 }' "$tmp_dir/selected")"
  expected_size="$(awk -F '\t' 'NR == 2 { print $3 }' "$tmp_dir/selected")"
  expected="$(awk -F '\t' 'NR == 2 { print $4 }' "$tmp_dir/selected")"
  archive_url="$(awk -F '\t' 'NR == 2 { print $5 }' "$tmp_dir/selected")"
fi
curl -fsSL --connect-timeout 10 --max-time 60 --proto '=https' --proto-redir '=https' --max-filesize "$MAX_ARCHIVE_BYTES" "$archive_url" -o "$tmp_dir/$archive" 2>/dev/null || fail "could not download $archive"
actual_size="$(wc -c < "$tmp_dir/$archive" | tr -d '[:space:]')"
[ "$actual_size" -le "$MAX_ARCHIVE_BYTES" ] || fail "release archive exceeds the size limit"
[ -z "$expected_size" ] || [ "$actual_size" = "$expected_size" ] || fail "release archive size does not match release record"
actual="$(sha256_file "$tmp_dir/$archive")"
[ "$actual" = "$expected" ] || fail "checksum verification failed for $archive"
extract_binary "$tmp_dir/$archive" "$tmp_dir/extracted"
if [ "$LEGACY_RELEASE" -eq 0 ]; then
  "$tmp_dir/bootstrap/tapid" __prepare-release-install "$VERSION" "$BOOTSTRAP_VERSION" "$tmp_dir/$archive" "$INSTALL_DIR/tapid" || fail "installer release policy rejected"
else
  # Historical binaries have no native policy helper. Never let this path
  # bypass durable rollback state or replace an existing installation.
  [ ! -e "$INSTALL_DIR/.tapid-release-state.json" ] && [ ! -L "$INSTALL_DIR/.tapid-release-state.json" ] || fail "legacy installer cannot replace release-managed state; use tapid upgrade"
  if [ -e "$INSTALL_DIR/tapid" ]; then
    cmp -s "$INSTALL_DIR/tapid" "$tmp_dir/extracted/tapid" || fail "legacy installer cannot replace an existing installation; use tapid upgrade"
  fi
fi

STAGED_BINARY="$(mktemp "$INSTALL_DIR/.tapid.tmp.XXXXXX")"
STAGED_MARKER="$(mktemp "$INSTALL_DIR/.tapid-marker.tmp.XXXXXX")"
install -m 0755 "$tmp_dir/extracted/tapid" "$STAGED_BINARY"
configure_path || fail 'could not safely update the selected shell startup file'
printf 'tapid-managed-v1\n' > "$STAGED_MARKER"
mv -f "$STAGED_MARKER" "$INSTALL_DIR/.tapid-managed"; STAGED_MARKER=""
mv -f "$STAGED_BINARY" "$INSTALL_DIR/tapid"; STAGED_BINARY=""
printf 'Installed Tapid v%s into %s/tapid\n' "$VERSION" "$INSTALL_DIR"
print_path_guidance
