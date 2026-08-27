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
  TAPID_RELEASE_BASE_URL, TAPID_RELEASE_DISCOVERY_URL

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
cleanup() { rm -rf "$tmp_dir"; [ -z "$STAGED_BINARY" ] || rm -f "$STAGED_BINARY"; [ -z "$STAGED_MARKER" ] || rm -f "$STAGED_MARKER"; }
trap cleanup 0 1 2 15

base="$RELEASE_BASE_URL/$VERSION"
manifest="$tmp_dir/manifest.json"
case "$RELEASE_BASE_URL" in https://*) ;; *) fail "stable release base URL must use HTTPS" ;; esac
command -v python3 >/dev/null 2>&1 || fail "unsupported Ed25519 verifier: python3 is required"
curl -fsSL "$base/manifest.json" -o "$manifest" 2>/dev/null || fail "could not download signed release manifest"
# This verifier is embedded so curl-piped execution has no adjacent-file dependency.
artifact_info="$(python3 - "$manifest" "$target" "$VERSION" <<'TAPID_BOOTSTRAP_VERIFIER'
#!/usr/bin/env python3
"""Self-contained Tapid release-manifest verifier for first-install bootstrap."""
import base64, hashlib, json, os, re, shutil, subprocess, sys, tempfile

PUBLIC_KEY = "eYPvN15Ah8ytHoBd2jY+36Wh/5g1kbqhDA9TL6wPRWc="
PUBLIC_KEY_FINGERPRINT = "sha256-238d16177b1c9ae21b53476d1a9097b5011414a26e6625986ecf1799dacf47f4"
KEY_ID = "release-key-2026-01"
TARGET_RE = re.compile(r"^[A-Za-z0-9._-]+$")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")


def fail(message):
    print("tapid installer: " + message, file=sys.stderr)
    raise SystemExit(1)


def jcs(value):
    # RFC 8785 for the release schema: all numeric values are non-negative
    # integers (artifact sizes); reject other numbers rather than guessing.
    if isinstance(value, dict):
        return "{" + ",".join(json.dumps(k, ensure_ascii=False, separators=(",", ":")) + ":" + jcs(value[k]) for k in sorted(value)) + "}"
    if isinstance(value, list):
        return "[" + ",".join(jcs(item) for item in value) + "]"
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if isinstance(value, bool) or value is None:
        return json.dumps(value, separators=(",", ":"))
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return str(value)
    fail("manifest contains an unsupported number for RFC 8785 canonicalization")


def canonical(value):
    return jcs(value).encode("utf-8")


def verify(manifest_path, target, version):
    try:
        value = json.loads(open(manifest_path, encoding="utf-8").read())
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        fail("cannot parse signed release manifest: " + str(exc))
    if not isinstance(value, dict):
        fail("signed release manifest must be an object")
    allowed = {"schema", "product", "version", "tag", "commit", "created_at", "expires_at", "artifacts", "sbom", "provenance", "signature"}
    if set(value) - allowed or value.get("schema") != "tapid-release-manifest-v1" or value.get("product") != "tapid":
        fail("invalid signed release manifest identity")
    bare_version = version[1:] if version.startswith("v") else version
    if value.get("version") != bare_version or value.get("tag") != version:
        fail("signed release manifest version does not match requested version")
    signature = value.get("signature")
    if not isinstance(signature, dict) or set(signature) != {"algorithm", "key_id", "signed_digest", "value"}:
        fail("signed release manifest signature fields are invalid")
    if signature["algorithm"] != "ed25519" or signature["key_id"] != KEY_ID:
        fail("signed release manifest is not signed by the production release key")
    unsigned = dict(value); unsigned.pop("signature", None)
    unsigned_bytes = canonical(unsigned)
    digest = "sha256-" + hashlib.sha256(unsigned_bytes).hexdigest()
    if signature["signed_digest"] != digest:
        fail("signed release manifest digest mismatch")
    context = dict(unsigned)
    context["signature_context"] = {"algorithm": "ed25519", "key_id": KEY_ID, "signed_digest": digest}
    try:
        sig = base64.b64decode(signature["value"], validate=True)
        pub = base64.b64decode(PUBLIC_KEY, validate=True)
    except Exception:
        fail("signed release manifest signature encoding is invalid")
    if len(sig) != 64 or len(pub) != 32:
        fail("signed release manifest signature key material is invalid")
    if "sha256-" + hashlib.sha256(pub).hexdigest() != PUBLIC_KEY_FINGERPRINT:
        fail("embedded production release key fingerprint is invalid")
    if not shutil.which("openssl"):
        fail("unsupported Ed25519 verifier: openssl is required")
    with tempfile.TemporaryDirectory() as directory:
        payload = os.path.join(directory, "payload")
        signature_file = os.path.join(directory, "signature")
        public_file = os.path.join(directory, "public.pem")
        open(payload, "wb").write(canonical(context))
        open(signature_file, "wb").write(sig)
        der = bytes.fromhex("302a300506032b6570032100") + pub
        open(public_file, "wb").write(b"-----BEGIN PUBLIC KEY-----\n" + base64.encodebytes(der) + b"-----END PUBLIC KEY-----\n")
        result = subprocess.run(["openssl", "pkeyutl", "-verify", "-pubin", "-inkey", public_file, "-rawin", "-in", payload, "-sigfile", signature_file], capture_output=True)
        if result.returncode != 0:
            fail("signed release manifest Ed25519 verification failed")
    artifacts = value.get("artifacts")
    if not isinstance(artifacts, list):
        fail("signed release manifest artifacts must be an array")
    matches = [a for a in artifacts if isinstance(a, dict) and a.get("target") == target]
    if len(matches) != 1:
        fail("signed release manifest must contain exactly one artifact for " + target)
    artifact = matches[0]
    expected_name = "tapid-%s-%s.tar.gz" % (bare_version, target)
    if artifact.get("name") != expected_name or not isinstance(artifact.get("url"), str) or not artifact["url"].startswith("https://"):
        fail("signed release manifest artifact identity or URL is invalid")
    if not isinstance(artifact.get("sha256"), str) or not HEX64_RE.fullmatch(artifact["sha256"]):
        fail("signed release manifest artifact hash is invalid")
    if not isinstance(artifact.get("size"), int) or isinstance(artifact["size"], bool) or artifact["size"] < 1:
        fail("signed release manifest artifact size is invalid")
    print("\t".join((artifact["name"], artifact["url"], artifact["sha256"], str(artifact["size"]))) )

if __name__ == "__main__":
    if len(sys.argv) != 4: fail("verifier requires MANIFEST TARGET VERSION")
    verify(sys.argv[1], sys.argv[2], sys.argv[3])
TAPID_BOOTSTRAP_VERIFIER
)" || fail "signed release manifest verification failed"
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
