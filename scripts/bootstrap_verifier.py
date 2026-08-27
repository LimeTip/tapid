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
