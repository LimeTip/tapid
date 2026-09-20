#!/bin/sh
set -eu

REPO="${TAPID_REPO:-LimeTip/tapid}"
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
MAX_CHECKSUM_BYTES=1048576
MAX_ARCHIVE_BYTES=536870912
MAX_BINARY_BYTES=536870912

usage() {
  cat <<'USAGE'
Usage: install.sh [options]

Install the latest stable Tapid release by default.

Options:
  --version VERSION     Install a specific stable release tag, e.g. v0.1.0
  --source-ref REF      Build from a source branch, tag, or commit (development)
  --install-dir DIR     Install the binary into DIR (default: ~/.local/bin)
  --repo OWNER/REPO     GitHub repository (default: LimeTip/tapid)
  -h, --help            Show this help

Environment:
  TAPID_REPO, TAPID_INSTALL_DIR
  TAPID_RELEASE_BASE_URL, TAPID_RELEASE_DISCOVERY_URL

Release assets are expected to include platform archives and SHA256SUMS.
Release tags use the v0.1.0 form; --version accepts either v0.1.0 or 0.1.0.
USAGE
}

fail() { printf 'tapid installer: %s\n' "$*" >&2; exit 1; }

configure_path() {
  case ":${PATH:-}:" in *:"$INSTALL_DIR":*) return ;; esac
  [ "$INSTALL_DIR" = "$HOME/.local/bin" ] || return 0
  shell_name="${SHELL-}"; shell_name="${shell_name##*/}"
  case "$shell_name" in
    zsh) PATH_RC="$HOME/.zprofile"; PATH_COMMAND=". \"$PATH_RC\""; path_line='export PATH="$HOME/.local/bin:$PATH"' ;;
    bash)
      if [ -f "$HOME/.bash_profile" ]; then PATH_RC="$HOME/.bash_profile"; else PATH_RC="$HOME/.bashrc"; fi
      PATH_COMMAND=". \"$PATH_RC\""; path_line='export PATH="$HOME/.local/bin:$PATH"'
      ;;
    fish)
      PATH_RC="$HOME/.config/fish/config.fish"; PATH_COMMAND="source \"$PATH_RC\""
      path_line='set -gx PATH $HOME/.local/bin $PATH'; mkdir -p "$(dirname "$PATH_RC")" || return 1
      ;;
    *) PATH_RC="$HOME/.profile"; PATH_COMMAND=". \"$PATH_RC\""; path_line='export PATH="$HOME/.local/bin:$PATH"' ;;
  esac
  if [ ! -f "$PATH_RC" ] || ! grep -Fqx "$path_line" "$PATH_RC"; then
    printf '\n# Tapid\n%s\n' "$path_line" >> "$PATH_RC" || return 1
  fi
  PATH="$INSTALL_DIR${PATH:+:$PATH}"; export PATH; PATH_UPDATED=1
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
    --version) [ "$#" -ge 2 ] || fail "--version requires a value"; VERSION="$2"; VERSION_SET=1; shift 2 ;;
    --source-ref) [ "$#" -ge 2 ] || fail "--source-ref requires a value"; SOURCE_REF="$2"; SOURCE_REF_SET=1; shift 2 ;;
    --install-dir) [ "$#" -ge 2 ] || fail "--install-dir requires a value"; INSTALL_DIR="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a value"; REPO="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown option: $1" ;;
  esac
done

case "$REPO" in ''|*[!A-Za-z0-9_./-]*|/*|*/|*/*/*) fail "repository must be OWNER/REPO" ;; esac
case "$REPO" in */*) ;; *) fail "repository must be OWNER/REPO" ;; esac
[ "$VERSION_SET" -eq 0 ] || [ "$SOURCE_REF_SET" -eq 0 ] || fail "use either --version or --source-ref, not both"
case "$INSTALL_DIR" in /*) ;; *) fail "install directory must be an absolute path" ;; esac
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
  printf 'tapid-managed-v1\n' > "$STAGED_MARKER"
  mv -f "$STAGED_MARKER" "$INSTALL_DIR/.tapid-managed"; STAGED_MARKER=""
  mv -f "$STAGED_BINARY" "$INSTALL_DIR/tapid"; STAGED_BINARY=""
  configure_path || printf 'Tapid was installed, but PATH could not be updated.\n' >&2
  printf 'Installed Tapid from %s into %s/tapid\n' "$SOURCE_REF" "$INSTALL_DIR"
  print_path_guidance
  exit 0
fi

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"
RELEASE_BASE_URL="${TAPID_RELEASE_BASE_URL:-https://github.com/$REPO/releases/download}"
RELEASE_DISCOVERY_URL="${TAPID_RELEASE_DISCOVERY_URL:-https://github.com/$REPO/releases/latest}"
case "$RELEASE_BASE_URL" in https://*) ;; *) fail "stable release base URL must use HTTPS" ;; esac
case "$RELEASE_DISCOVERY_URL" in https://*) ;; *) fail "stable release discovery URL must use HTTPS" ;; esac

if [ "$VERSION" = latest ]; then
  resolved_url="$(curl -fsSIL -o /dev/null -w '%{url_effective}' "$RELEASE_DISCOVERY_URL" 2>/dev/null)" || fail "could not contact the stable release discovery endpoint"
  case "$resolved_url" in */releases/tag/*) VERSION="${resolved_url##*/tag/}" ;; *) fail "stable release discovery endpoint did not resolve a release tag" ;; esac
fi
case "$VERSION" in ''|*[!v0-9.]*) fail "version must be a stable release such as v0.1.0" ;; esac
printf '%s' "$VERSION" | grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+$' || fail "version must be a stable release such as v0.1.0"
case "$VERSION" in v*) ;; *) VERSION="v$VERSION" ;; esac

case "$(uname -s):$(uname -m)" in
  Darwin:arm64|Darwin:aarch64) target="aarch64-apple-darwin" ;;
  Darwin:x86_64) target="x86_64-apple-darwin" ;;
  Linux:x86_64|Linux:amd64) target="x86_64-unknown-linux-gnu" ;;
  Linux:arm64|Linux:aarch64) target="aarch64-unknown-linux-gnu" ;;
  *) fail "unsupported platform: $(uname -s) $(uname -m)" ;;
esac

version_without_v="${VERSION#v}"
archive="tapid-$version_without_v-$target.tar.gz"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tapid-install.XXXXXX")"
cleanup() { rm -rf "$tmp_dir"; [ -z "$STAGED_BINARY" ] || rm -f "$STAGED_BINARY"; [ -z "$STAGED_MARKER" ] || rm -f "$STAGED_MARKER"; }
trap cleanup 0 1 2 15
base="$RELEASE_BASE_URL/$VERSION"
curl -fsSL --max-filesize "$MAX_CHECKSUM_BYTES" "$base/SHA256SUMS" -o "$tmp_dir/SHA256SUMS" 2>/dev/null || fail "could not download SHA256SUMS"
[ "$(wc -c < "$tmp_dir/SHA256SUMS" | tr -d '[:space:]')" -le "$MAX_CHECKSUM_BYTES" ] || fail "SHA256SUMS exceeds the size limit"
expected="$(awk -v name="$archive" '{ hash=substr($0,1,64); separator=substr($0,65,2); file=substr($0,67); if (length(hash)==64 && hash !~ /[^0-9a-f]/ && separator=="  " && file==name) { print hash; count++ } } END { if (count != 1) exit 1 }' "$tmp_dir/SHA256SUMS")" || fail "SHA256SUMS does not contain exactly one checksum for $archive"
printf '%s' "$expected" | grep -Eq '^[0-9a-f]{64}$' || fail "invalid SHA-256 checksum for $archive"
curl -fsSL --max-filesize "$MAX_ARCHIVE_BYTES" "$base/$archive" -o "$tmp_dir/$archive" 2>/dev/null || fail "could not download $archive"
[ "$(wc -c < "$tmp_dir/$archive" | tr -d '[:space:]')" -le "$MAX_ARCHIVE_BYTES" ] || fail "release archive exceeds the size limit"
if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp_dir/$archive" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp_dir/$archive" | awk '{print $1}')"
else
  fail "shasum or sha256sum is required"
fi
[ "$actual" = "$expected" ] || fail "checksum verification failed for $archive"

mkdir "$tmp_dir/extracted"
members="$(tar -tzf "$tmp_dir/$archive")" || fail "cannot inspect release archive"
printf '%s\n' "$members" | awk 'NF { count++; if ($0 != "tapid") invalid=1 } END { exit !(count == 1 && !invalid) }' || fail "release archive must contain exactly one member named tapid"
entry_info="$(tar -tvzf "$tmp_dir/$archive")" || fail "cannot inspect release archive entry type"
printf '%s\n' "$entry_info" | awk 'NF { count++; if (substr($0, 1, 1) != "-" || $NF != "tapid") invalid=1 } END { exit !(count == 1 && !invalid) }' || fail "release archive tapid member must be a regular file"
probe_bytes="$(tar -xOzf "$tmp_dir/$archive" tapid | dd bs=1048576 count=513 2>/dev/null | wc -c | tr -d '[:space:]')"
[ "$probe_bytes" -le "$MAX_BINARY_BYTES" ] || fail "release binary exceeds the size limit"
(ulimit -f 1048576; tar -xzf "$tmp_dir/$archive" -C "$tmp_dir/extracted" tapid) || fail "cannot extract tapid"
[ -f "$tmp_dir/extracted/tapid" ] && [ ! -L "$tmp_dir/extracted/tapid" ] || fail "release archive tapid member must be a regular file"
[ "$(wc -c < "$tmp_dir/extracted/tapid" | tr -d '[:space:]')" -le "$MAX_BINARY_BYTES" ] || fail "release binary exceeds the size limit"

STAGED_BINARY="$(mktemp "$INSTALL_DIR/.tapid.tmp.XXXXXX")"
STAGED_MARKER="$(mktemp "$INSTALL_DIR/.tapid-marker.tmp.XXXXXX")"
install -m 0755 "$tmp_dir/extracted/tapid" "$STAGED_BINARY"
printf 'tapid-managed-v1\n' > "$STAGED_MARKER"
mv -f "$STAGED_MARKER" "$INSTALL_DIR/.tapid-managed"; STAGED_MARKER=""
mv -f "$STAGED_BINARY" "$INSTALL_DIR/tapid"; STAGED_BINARY=""
configure_path || printf 'Tapid was installed, but PATH could not be updated.\n' >&2
printf 'Installed Tapid %s into %s/tapid\n' "$VERSION" "$INSTALL_DIR"
print_path_guidance
