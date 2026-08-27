#!/bin/sh
set -eu

REPO="${TAPID_REPO:-LimeTip/tapid}"
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
  TAPID_RELEASE_BASE_URL, TAPID_RELEASE_DISCOVERY_URL, TAPID_RELEASE_MANIFEST_URL

Stable releases are described by a provider-neutral, signed manifest. The
manifest contains schema_version, version, target, artifact_url,
artifact_size, and artifact_sha256. Its detached Ed25519 signature is fetched
from manifest.json.sig (the detached manifest.sig). A checksum file alone is
never trusted.
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
command -v openssl >/dev/null 2>&1 || fail "fail closed: an OpenSSL Ed25519 verifier is required"
command -v python3 >/dev/null 2>&1 || fail "fail closed: python3 is required to validate the signed manifest"

manifest_url="${TAPID_RELEASE_MANIFEST_URL:-}"
manifest_url_fallback=""
if [ -z "$manifest_url" ] && [ "$VERSION" = latest ]; then
  manifest_url="$RELEASE_DISCOVERY_URL/download/tapid-manifest.json"
  manifest_url_fallback="$RELEASE_BASE_URL/latest/tapid-manifest.json"
fi

if [ "$VERSION" != latest ]; then
  case "$VERSION" in
    *[![:print:]]*) fail "version must be a stable release such as v0.1.0" ;;
  esac
  printf '%s' "$VERSION" | grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+$' || \
    fail "version must be a stable release such as v0.1.0"
fi
if [ "$VERSION" != latest ]; then
  case "$VERSION" in
    v*) ;;
    *) VERSION="v$VERSION" ;;
  esac
fi

case "$(uname -s):$(uname -m)" in
  Darwin:arm64|Darwin:aarch64) target="aarch64-apple-darwin" ;;
  Darwin:x86_64) target="x86_64-apple-darwin" ;;
  Linux:x86_64|Linux:amd64) target="x86_64-unknown-linux-gnu" ;;
  Linux:arm64|Linux:aarch64) target="aarch64-unknown-linux-gnu" ;;
  *) fail "unsupported platform: $(uname -s) $(uname -m)" ;;
esac

version_without_v="${VERSION#v}"
if [ -z "$manifest_url" ]; then
  manifest_url="$RELEASE_BASE_URL/$VERSION/tapid-manifest.json"
  manifest_url_fallback="$RELEASE_BASE_URL/$VERSION/manifest.json"
fi
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tapid-install.XXXXXX")"
cleanup() { rm -rf "$tmp_dir"; [ -z "$STAGED_BINARY" ] || rm -f "$STAGED_BINARY"; }
trap cleanup 0 1 2 15

download_manifest() {
  curl -fsSL "$1" -o "$tmp_dir/manifest.json" 2>/dev/null && \
    curl -fsSL "$1.sig" -o "$tmp_dir/manifest.json.sig" 2>/dev/null
}
if ! download_manifest "$manifest_url"; then
  [ -n "$manifest_url_fallback" ] || fail "could not fetch the stable signed manifest"
  manifest_url="$manifest_url_fallback"
  download_manifest "$manifest_url" || fail "could not fetch the stable signed manifest"
fi

# CONFIGURATION: this is the release-signing public key, never a private key.
public_key_file="$tmp_dir/release-signing-key.pem"
if [ -n "${TAPID_RELEASE_PUBLIC_KEY_FILE:-}" ]; then
  [ -f "$TAPID_RELEASE_PUBLIC_KEY_FILE" ] || fail "release public key file does not exist"
  cp "$TAPID_RELEASE_PUBLIC_KEY_FILE" "$public_key_file"
else
  printf '%s\n' '-----BEGIN PUBLIC KEY-----' 'MCowBQYDK2VwAyEAKH2wLpL1ZawchfeUH3TH4xxWHHwdHel/GtPSTCNy8SY=' '-----END PUBLIC KEY-----' > "$public_key_file"
fi
openssl pkeyutl -verify -pubin -inkey "$public_key_file" -rawin \
  -in "$tmp_dir/manifest.json" -sigfile "$tmp_dir/manifest.json.sig" >/dev/null 2>&1 || \
  fail "Ed25519 manifest signature verification failed"

manifest_values="$(python3 - "$tmp_dir/manifest.json" "$target" "$VERSION" <<'PY'
import json, sys
m=json.load(open(sys.argv[1], encoding='utf-8'))
if m.get('schema_version') != 1 or m.get('target') != sys.argv[2] or (sys.argv[3] != 'latest' and m.get('version') != sys.argv[3]): raise SystemExit(1)
u=m.get('artifact_url'); n=m.get('artifact_size'); h=m.get('artifact_sha256')
if not isinstance(u,str) or not u.startswith(('https://','file://')) or not isinstance(n,int) or n < 0 or not isinstance(h,str) or len(h)!=64 or any(c not in '0123456789abcdefABCDEF' for c in h): raise SystemExit(1)
if not isinstance(m.get('version'), str) or not __import__('re').fullmatch(r'v?[0-9]+\.[0-9]+\.[0-9]+', m['version']): raise SystemExit(1)
print(m['version']); print(u); print(n); print(h.lower())
PY
)" || fail "signed manifest has invalid target, version, or artifact metadata"
VERSION="$(printf '%s\n' "$manifest_values" | sed -n '1p')"
case "$VERSION" in v*) ;; *) VERSION="v$VERSION" ;; esac
artifact_url="$(printf '%s\n' "$manifest_values" | sed -n '2p')"
artifact_size="$(printf '%s\n' "$manifest_values" | sed -n '3p')"
expected="$(printf '%s\n' "$manifest_values" | sed -n '4p')"
archive="$tmp_dir/artifact.tar.gz"
curl -fsSL "$artifact_url" -o "$archive" 2>/dev/null || fail "could not fetch the signed release artifact"
actual_size="$(wc -c < "$archive" | tr -d '[:space:]')"
[ "$actual_size" = "$artifact_size" ] || fail "signed artifact size verification failed"
if command -v shasum >/dev/null 2>&1; then actual="$(shasum -a 256 "$archive" | awk '{print $1}')"; else command -v sha256sum >/dev/null 2>&1 || fail "SHA-256 verifier is required"; actual="$(sha256sum "$archive" | awk '{print $1}')"; fi
[ "$actual" = "$expected" ] || fail "signed artifact SHA-256 verification failed"

mkdir -p "$tmp_dir/extracted"
members="$(tar -tzf "$archive")" || fail "cannot inspect release archive"
printf '%s\n' "$members" | awk 'NF { count++; if ($0 != "tapid") invalid=1 } END { exit !(count == 1 && !invalid) }' || \
  fail "release archive must contain exactly one member named tapid"
entry_info="$(tar -tvzf "$archive")" || fail "cannot inspect release archive entry type"
printf '%s\n' "$entry_info" | awk 'NF { count++; if (substr($0, 1, 1) != "-" || $NF != "tapid") invalid=1 } END { exit !(count == 1 && !invalid) }' || \
  fail "release archive tapid member must be a regular file"
tar -xzf "$archive" -C "$tmp_dir/extracted" tapid || fail "cannot extract tapid from release archive"
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
