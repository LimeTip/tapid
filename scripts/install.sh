#!/bin/sh
set -eu

REPO="${TAPID_REPO:-LimeTip/tapid}"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
RELEASE_BASE_URL="${TAPID_RELEASE_BASE_URL:-https://github.com/$REPO/releases/download}"
RELEASE_DISCOVERY_URL="${TAPID_RELEASE_DISCOVERY_URL:-https://github.com/$REPO/releases/latest}"
INSTALL_DIR="${TAPID_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="latest"
VERSION_SET=0
SOURCE_REF=""
SOURCE_REF_SET=0
STAGED_BINARY=""
STAGED_MARKER=""
PATH_UPDATED=0
PATH_RC=""
PATH_COMMAND=""

usage() {
  cat <<'USAGE'
Usage: install.sh [options]

Install the latest stable Tapid release by default.

Options:
  --version VERSION     Install a specific stable release tag, e.g. v0.1.0
  --source-ref REF      Build from a source branch, tag, or commit (development)
  --install-dir DIR     Install the binary into DIR (default: ~/.local/bin)
  --repo OWNER/REPO     Default GitHub repository (default: LimeTip/tapid)
  -h, --help            Show this help

Environment:
  TAPID_REPO, TAPID_INSTALL_DIR
  TAPID_RELEASE_BASE_URL, TAPID_RELEASE_DISCOVERY_URL
  TAPID_RELEASE_VERIFIER, TAPID_RELEASE_TRUSTED_KEYS

Release assets are expected to use this contract:
  tapid-0.1.0-TARGET.tar.gz
  manifest.json (signed tapid-release-manifest-v1)
Release tags use the v0.1.0 form; --version accepts either v0.1.0 or 0.1.0.
USAGE
}

fail() { printf 'tapid installer: %s\n' "$*" >&2; exit 1; }

configure_path() {
  case ":${PATH:-}:" in
    *:"$INSTALL_DIR":*) return ;;
  esac
  # Only modify shell configuration for the default user-local path.
  if [ "$INSTALL_DIR" != "$HOME/.local/bin" ]; then return; fi

  shell_name="${SHELL-}"
  shell_name="${shell_name##*/}"
  case "$shell_name" in
    zsh) PATH_RC="$HOME/.zprofile"; PATH_COMMAND=". \"$PATH_RC\""; path_line='export PATH="$HOME/.local/bin:$PATH"' ;;
    bash)
      if [ -f "$HOME/.bash_profile" ]; then PATH_RC="$HOME/.bash_profile"; else PATH_RC="$HOME/.bashrc"; fi
      PATH_COMMAND=". \"$PATH_RC\""
      path_line='export PATH="$HOME/.local/bin:$PATH"'
      ;;
    fish)
      PATH_RC="$HOME/.config/fish/config.fish"
      PATH_COMMAND="source \"$PATH_RC\""
      path_line='set -gx PATH $HOME/.local/bin $PATH'
      mkdir -p "$(dirname "$PATH_RC")"
      ;;
    *)
      PATH_RC="$HOME/.profile"
      PATH_COMMAND=". \"$PATH_RC\""
      path_line='export PATH="$HOME/.local/bin:$PATH"'
      ;;
  esac

  if [ ! -f "$PATH_RC" ] || ! grep -Fqx "$path_line" "$PATH_RC"; then
    printf '\n# Tapid\n%s\n' "$path_line" >> "$PATH_RC"
  fi
  if [ -n "${PATH:-}" ]; then
    PATH="$INSTALL_DIR:$PATH"
  else
    PATH="$INSTALL_DIR"
  fi
  export PATH
  PATH_UPDATED=1
}

print_path_guidance() {
  if [ "$PATH_UPDATED" -eq 1 ]; then
    printf 'Tapid was installed and PATH was configured in %s.\n' "$PATH_RC"
    printf 'To enable it in the current shell, run: %s\n' "$PATH_COMMAND"
  elif [ "$INSTALL_DIR" != "$HOME/.local/bin" ]; then
    printf 'Add this directory to PATH before running Tapid: %s\n' "$INSTALL_DIR"
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || fail "--version requires a value"
      VERSION="$2"
      VERSION_SET=1
      shift 2
      ;;
    --source-ref)
      [ "$#" -ge 2 ] || fail "--source-ref requires a value"
      SOURCE_REF="$2"
      SOURCE_REF_SET=1
      shift 2
      ;;
    --install-dir)
      [ "$#" -ge 2 ] || fail "--install-dir requires a value"
      INSTALL_DIR="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires a value"
      REPO="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown option: $1"
      ;;
  esac
done

case "$REPO" in
  ''|*[!A-Za-z0-9_./-]*|*/*/*|/*|*/|.*|*/.*) fail "repository must be OWNER/REPO" ;;
esac
printf '%s' "$REPO" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' || \
  fail "repository must be OWNER/REPO"

if [ "$VERSION_SET" -eq 1 ] && [ "$SOURCE_REF_SET" -eq 1 ]; then
  fail "use either --version or --source-ref, not both"
fi

case "$INSTALL_DIR" in
  /*) ;;
  *) fail "install directory must be an absolute path" ;;
esac

mkdir -p "$INSTALL_DIR"
if [ -e "$INSTALL_DIR/tapid" ] || [ -L "$INSTALL_DIR/tapid" ]; then
  [ -f "$INSTALL_DIR/tapid" ] && [ ! -L "$INSTALL_DIR/tapid" ] || \
    fail "existing Tapid destination must be a regular file"
fi

if [ "$SOURCE_REF_SET" -eq 1 ]; then
  [ -n "$SOURCE_REF" ] || fail "--source-ref requires a non-empty value"
  case "$SOURCE_REF" in
    -*) fail "source ref must not start with '-'" ;;
  esac
  command -v cargo >/dev/null 2>&1 || fail "cargo is required for --source-ref"
  command -v git >/dev/null 2>&1 || fail "git is required for --source-ref"
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tapid-install.XXXXXX")"
  cleanup() { rm -rf "$tmp_dir"; [ -z "$STAGED_BINARY" ] || rm -f "$STAGED_BINARY"; [ -z "$STAGED_MARKER" ] || rm -f "$STAGED_MARKER"; }
  trap cleanup 0 1 2 15
  git clone --filter=blob:none --no-checkout "https://github.com/$REPO.git" "$tmp_dir/tapid"
  if ! git -C "$tmp_dir/tapid" checkout --detach "$SOURCE_REF"; then
    git -C "$tmp_dir/tapid" fetch --filter=blob:none origin "$SOURCE_REF" || \
      fail "could not find source ref $SOURCE_REF in $REPO"
    git -C "$tmp_dir/tapid" checkout --detach "$SOURCE_REF" || \
      fail "could not check out source ref $SOURCE_REF in $REPO"
  fi
  cargo install --path "$tmp_dir/tapid/crates/tapid-cli" --locked --root "$tmp_dir/root"
  mkdir -p "$INSTALL_DIR"
  STAGED_BINARY="$(mktemp "$INSTALL_DIR/.tapid.tmp.XXXXXX")"
  STAGED_MARKER="$(mktemp "$INSTALL_DIR/.tapid-marker.tmp.XXXXXX")"
  install -m 0755 "$tmp_dir/root/bin/tapid" "$STAGED_BINARY"
  printf 'tapid-managed-v1\n' > "$STAGED_MARKER"
  mv -f "$STAGED_BINARY" "$INSTALL_DIR/tapid"
  STAGED_BINARY=""
  mv -f "$STAGED_MARKER" "$INSTALL_DIR/.tapid-managed"
  STAGED_MARKER=""
  configure_path
  printf 'Installed Tapid from %s into %s/tapid\n' "$SOURCE_REF" "$INSTALL_DIR"
  print_path_guidance
  exit 0
fi

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"


case "$VERSION" in
  latest)
    resolved_url="$(curl -fsSIL -o /dev/null -w '%{url_effective}' "$RELEASE_DISCOVERY_URL" 2>/dev/null)" || \
      fail "could not contact the stable release discovery endpoint"
    case "$resolved_url" in
      */releases/tag/*) VERSION="${resolved_url##*/tag/}" ;;
      *) fail "stable release discovery endpoint did not resolve a release tag; use --source-ref REF for development installation" ;;
    esac
    ;;
esac

case "$VERSION" in
  ''|*[!0-9.v]*) fail "version must be a stable release such as v0.1.0" ;;
esac
if ! printf '%s' "$VERSION" | grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+$'; then
  fail "version must be a stable release such as v0.1.0"
fi
case "$VERSION" in
  v*) ;;
  *) VERSION="v$VERSION" ;;
esac

case "$(uname -s):$(uname -m)" in
  Darwin:arm64|Darwin:aarch64) target="aarch64-apple-darwin" ;;
  Darwin:x86_64) target="x86_64-apple-darwin" ;;
  Linux:x86_64|Linux:amd64) target="x86_64-unknown-linux-gnu" ;;
  Linux:arm64|Linux:aarch64) target="aarch64-unknown-linux-gnu" ;;
  *) fail "unsupported platform: $(uname -s) $(uname -m)" ;;
esac

version_without_v="${VERSION#v}"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tapid-install.XXXXXX")"
cleanup() { rm -rf "$tmp_dir"; [ -z "$STAGED_BINARY" ] || rm -f "$STAGED_BINARY"; }
trap cleanup 0 1 2 15

base="$RELEASE_BASE_URL/$VERSION"
manifest="$tmp_dir/manifest.json"
case "$RELEASE_BASE_URL" in https://*) ;; *) fail "stable release base URL must use HTTPS" ;; esac
command -v python3 >/dev/null 2>&1 || fail "python3 is required for signed manifest validation"
[ -n "${TAPID_RELEASE_VERIFIER:-}" ] || fail "stable installation requires TAPID_RELEASE_VERIFIER; use --source-ref REF for development installation"
[ -x "$TAPID_RELEASE_VERIFIER" ] || fail "TAPID_RELEASE_VERIFIER must be an executable path"
[ -n "${TAPID_RELEASE_TRUSTED_KEYS:-}" ] || fail "stable installation requires TAPID_RELEASE_TRUSTED_KEYS; use --source-ref REF for development installation"
[ -f "$TAPID_RELEASE_TRUSTED_KEYS" ] || fail "TAPID_RELEASE_TRUSTED_KEYS must name a trusted key file"
curl -fsSL "$base/manifest.json" -o "$manifest" 2>/dev/null || fail "stable release $VERSION has no signed release manifest"
"$TAPID_RELEASE_VERIFIER" "$manifest" "$TAPID_RELEASE_TRUSTED_KEYS" "$target" "$VERSION" || fail "signed release manifest verification failed"
# The verifier validates the Ed25519 signature; the helper validates signature,
# target, version, size, and sha256 fields before artifact selection.
artifact_info="$(python3 "$SCRIPT_DIR/release_manifest.py" "$manifest" "$target" "$VERSION")" || exit 1
IFS='\t' read -r archive artifact_url expected expected_size <<EOF
$artifact_info
EOF
curl -fsSL "$artifact_url" -o "$tmp_dir/$archive" 2>/dev/null || fail "could not download verified artifact $archive"
if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp_dir/$archive" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp_dir/$archive" | awk '{print $1}')"
else
  fail "shasum or sha256sum is required"
fi
[ "$actual" = "$expected" ] || fail "checksum verification failed for $archive"
[ "$(wc -c < "$tmp_dir/$archive" | tr -d ' ')" = "$expected_size" ] || fail "artifact size verification failed for $archive"

mkdir -p "$tmp_dir/extracted"
members="$(tar -tzf "$tmp_dir/$archive")" || fail "cannot inspect release archive"
printf '%s\n' "$members" | awk 'NF { count++; if ($0 != "tapid") invalid=1 } END { exit !(count == 1 && !invalid) }' || \
  fail "release archive must contain exactly one member named tapid"
entry_info="$(tar -tvzf "$tmp_dir/$archive")" || fail "cannot inspect release archive entry type"
printf '%s\n' "$entry_info" | awk 'NF { count++; if (substr($0, 1, 1) != "-" || $NF != "tapid") invalid=1 } END { exit !(count == 1 && !invalid) }' || \
  fail "release archive tapid member must be a regular file"
tar -xzf "$tmp_dir/$archive" -C "$tmp_dir/extracted" tapid || fail "cannot extract tapid from release archive"
[ -f "$tmp_dir/extracted/tapid" ] || fail "release archive does not contain tapid"
[ ! -L "$tmp_dir/extracted/tapid" ] || fail "release archive tapid member must not be a symlink"
  STAGED_BINARY="$(mktemp "$INSTALL_DIR/.tapid.tmp.XXXXXX")"
  STAGED_MARKER="$(mktemp "$INSTALL_DIR/.tapid-marker.tmp.XXXXXX")"
  install -m 0755 "$tmp_dir/extracted/tapid" "$STAGED_BINARY"
  printf 'tapid-managed-v1\n' > "$STAGED_MARKER"
  mv -f "$STAGED_BINARY" "$INSTALL_DIR/tapid"
  STAGED_BINARY=""
  mv -f "$STAGED_MARKER" "$INSTALL_DIR/.tapid-managed"
  STAGED_MARKER=""
configure_path
printf 'Installed Tapid %s into %s/tapid\n' "$VERSION" "$INSTALL_DIR"
print_path_guidance
