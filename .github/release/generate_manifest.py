#!/usr/bin/env python3
"""Generate a deterministic, signed Tapid release manifest."""
import argparse
import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urljoin

VERSION_RE = re.compile(r"^0\.\d+\.\d+$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40,64}$")
TARGET_RE = re.compile(r"^[A-Za-z0-9._-]+$")
UTC_RFC3339_RE = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$"
)


def canonical(value):
    """Serialize the release schema using the RFC 8785 JCS subset it permits."""
    if isinstance(value, dict):
        ordered = sorted(value.items(), key=lambda item: item[0].encode("utf-16-be"))
        return ("{" + ",".join(
            json.dumps(key, ensure_ascii=False, separators=(",", ":")) + ":" + canonical(item).decode("utf-8")
            for key, item in ordered
        ) + "}").encode("utf-8")
    if isinstance(value, list):
        return ("[" + ",".join(canonical(item).decode("utf-8") for item in value) + "]").encode("utf-8")
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if isinstance(value, bool) or value is None:
        return json.dumps(value, separators=(",", ":")).encode("ascii")
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return str(value).encode("ascii")
    raise ValueError("release manifest contains a number outside the supported JCS subset")


def sign(payload, pem):
    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        key = serialization.load_pem_private_key(pem.encode(), password=None)
        if not isinstance(key, Ed25519PrivateKey):
            raise ValueError("RELEASE_SIGNING_KEY is not an Ed25519 private key")
        return key.sign(payload)
    except ImportError:
        with tempfile.TemporaryDirectory() as directory:
            key_path = Path(directory) / "key.pem"
            data_path = Path(directory) / "payload"
            signature_path = Path(directory) / "signature"
            key_path.write_text(pem)
            key_path.chmod(0o600)
            data_path.write_bytes(payload)
            result = subprocess.run(
                ["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", str(key_path),
                 "-in", str(data_path), "-out", str(signature_path)],
                capture_output=True, text=True,
            )
            if result.returncode:
                raise RuntimeError(f"Ed25519 signing failed: {result.stderr.strip()}")
            return signature_path.read_bytes()


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(2)


def parse_utc_rfc3339(value, field):
    if not UTC_RFC3339_RE.fullmatch(value):
        fail(f"{field} must be a canonical UTC RFC3339 timestamp ending in Z")
    try:
        return datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError:
        fail(f"{field} must be a canonical UTC RFC3339 timestamp ending in Z")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--created-at", required=True)
    parser.add_argument("--expires-at", required=True)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--artifact", action="append", required=True, metavar="TARGET=PATH")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    if not VERSION_RE.fullmatch(args.version) or args.tag != f"v{args.version}":
        fail("version and tag must match supported 0.x.y format")
    if not COMMIT_RE.fullmatch(args.commit):
        fail("commit must be an immutable lowercase hexadecimal SHA")
    created_at = parse_utc_rfc3339(args.created_at, "created_at")
    expires_at = parse_utc_rfc3339(args.expires_at, "expires_at")
    if expires_at <= created_at:
        fail("expires_at must be later than created_at")
    now = datetime.now(timezone.utc)
    if created_at > now:
        fail("created_at must not be in the future")
    if expires_at <= now:
        fail("expires_at must be in the future")
    if not args.base_url.startswith("https://"):
        fail("base URL must use HTTPS")
    artifacts = []
    for item in args.artifact:
        if "=" not in item:
            fail(f"artifact must be TARGET=PATH: {item}")
        target, raw_path = item.split("=", 1)
        path = Path(raw_path)
        if not TARGET_RE.fullmatch(target):
            fail(f"invalid artifact target: {target}")
        if not path.is_file():
            fail(f"artifact does not exist: {path}")
        name = path.name
        if not name.startswith(f"tapid-{args.version}-") or not (name.endswith(".tar.gz") or name.endswith(".zip")):
            fail(f"artifact filename does not match version and archive format: {name}")
        contents = path.read_bytes()
        artifacts.append({"name": name, "target": target, "url": urljoin(args.base_url.rstrip("/") + "/", name),
                          "sha256": hashlib.sha256(contents).hexdigest(), "size": len(contents)})
    key = os.environ.get("RELEASE_SIGNING_KEY")
    key_id = os.environ.get("RELEASE_SIGNING_KEY_ID")
    if not key:
        fail("RELEASE_SIGNING_KEY is required; refusing to publish unsigned metadata")
    if not key_id or not re.fullmatch(r"[A-Za-z0-9._:-]+", key_id):
        fail("RELEASE_SIGNING_KEY_ID is required and malformed")
    manifest = {"schema": "tapid-release-manifest-v1", "product": "tapid", "version": args.version,
                "tag": args.tag, "commit": args.commit, "created_at": args.created_at,
                "expires_at": args.expires_at, "artifacts": artifacts}
    unsigned_digest = "sha256-" + hashlib.sha256(canonical(manifest)).hexdigest()
    context = dict(manifest)
    context["signature_context"] = {"algorithm": "ed25519", "key_id": key_id, "signed_digest": unsigned_digest}
    signature = base64.b64encode(sign(canonical(context), key)).decode("ascii")
    manifest["signature"] = {"algorithm": "ed25519", "key_id": key_id, "signed_digest": unsigned_digest, "value": signature}
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(canonical(manifest) + b"\n")


if __name__ == "__main__":
    main()
