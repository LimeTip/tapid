#!/usr/bin/env python3
"""Fail-closed release-manifest/artifact/signature preflight.

The default fixture is intentionally ephemeral. Its signature field is
shape-checked only; cryptographic verification uses the existing Rust signing
crate tests, so no private key or production trust root is introduced here.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import subprocess
import tempfile
from pathlib import Path
from urllib.parse import urlparse

VERSION_RE = re.compile(r"^0\.[0-9]+\.[0-9]+$")
TAG_RE = re.compile(r"^v0\.[0-9]+\.[0-9]+$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40,64}$")
KEY_RE = re.compile(r"^[A-Za-z0-9._:-]+$")
ARTIFACT_RE = re.compile(r"^tapid-0\.[0-9]+\.[0-9]+-[A-Za-z0-9._-]+\.(tar\.gz|zip)$")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")


def fail(message: str) -> None:
    raise SystemExit(f"preflight failed: {message}")


def canonical(value: object) -> bytes:
    # The generated fixture contains only JSON types whose RFC 8785 encoding is
    # identical to this deterministic UTF-8 representation (no floats).
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def validate_shape(manifest: dict) -> None:
    required = {"schema", "product", "version", "tag", "commit", "created_at", "expires_at", "artifacts", "signature"}
    allowed = required | {"sbom", "provenance"}
    if set(manifest) - allowed or not required <= set(manifest):
        fail("manifest fields do not match the v1 contract")
    if manifest["schema"] != "tapid-release-manifest-v1" or manifest["product"] != "tapid":
        fail("schema/product discriminator mismatch")
    version = manifest["version"]
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version) or manifest["tag"] != f"v{version}":
        fail("version/tag mismatch")
    if not isinstance(manifest["commit"], str) or not COMMIT_RE.fullmatch(manifest["commit"]):
        fail("invalid commit")
    for field in ("created_at", "expires_at"):
        if not isinstance(manifest[field], str) or not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[^ ]+Z", manifest[field]):
            fail(f"invalid {field}")
    artifacts = manifest["artifacts"]
    if not isinstance(artifacts, list) or not artifacts:
        fail("artifacts must be a non-empty array")
    for artifact in artifacts:
        if set(artifact) != {"name", "target", "url", "sha256", "size"}:
            fail("artifact fields do not match the v1 contract")
        if not ARTIFACT_RE.fullmatch(artifact["name"]):
            fail("invalid artifact name")
        if not re.fullmatch(r"[A-Za-z0-9._-]+", artifact["target"]):
            fail("invalid artifact target")
        if not isinstance(artifact["url"], str) or urlparse(artifact["url"]).scheme != "https":
            fail("artifact URL must use HTTPS")
        if not HEX64_RE.fullmatch(artifact["sha256"]):
            fail("artifact sha256 must be lowercase hexadecimal")
        if not isinstance(artifact["size"], int) or isinstance(artifact["size"], bool) or artifact["size"] < 1:
            fail("artifact size must be a positive integer")
    sig = manifest["signature"]
    if set(sig) != {"algorithm", "key_id", "signed_digest", "value"}:
        fail("signature fields do not match the v1 contract")
    if sig["algorithm"] != "ed25519" or not KEY_RE.fullmatch(sig["key_id"]):
        fail("invalid signature algorithm or key ID")
    if not re.fullmatch(r"^sha256-[0-9a-f]{64}$", sig["signed_digest"]):
        fail("invalid signed_digest")
    try:
        raw = base64.b64decode(sig["value"], validate=True)
    except Exception as exc:
        fail(f"invalid signature base64: {exc}")
    if len(raw) != 64:
        fail("Ed25519 signature must decode to 64 bytes")


def run(*args: str, input: bytes | None = None) -> None:
    result = subprocess.run(args, input=input, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        fail(f"command {' '.join(args)} failed: {result.stderr.decode().strip()}")


def fixture(root: Path) -> tuple[Path, Path, Path]:
    artifact = root / "tapid-0.0.6-x86_64-unknown-linux-gnu.tar.gz"
    artifact.write_bytes(b"Tapid release preflight fixture\n")
    artifact_sha = hashlib.sha256(artifact.read_bytes()).hexdigest()
    manifest = {
        "schema": "tapid-release-manifest-v1", "product": "tapid", "version": "0.0.6", "tag": "v0.0.6",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "created_at": "2026-08-25T10:00:00Z", "expires_at": "2099-09-25T10:00:00Z",
        "artifacts": [{"name": artifact.name, "target": "x86_64-unknown-linux-gnu",
                       "url": "https://github.com/LimeTip/tapid/releases/download/v0.0.6/" + artifact.name,
                       "sha256": artifact_sha, "size": artifact.stat().st_size}],
    }
    unsigned_digest = "sha256-" + hashlib.sha256(canonical(manifest)).hexdigest()
    # Shape-only fixture signature: cryptographic verification is exercised by
    # the existing tapid-signatures crate tests in the workflow. Keeping the
    # fixture unsigned-by-cryptography avoids introducing a static private key.
    manifest["signature"] = {"algorithm": "ed25519", "key_id": "fixture-release-key",
                              "signed_digest": unsigned_digest,
                              "value": base64.b64encode(bytes(64)).decode()}
    manifest_path = root / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest_path, None, None


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest")
    parser.add_argument("--artifact-root", default=".")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tapid-release-preflight-") as temp:
        root = Path(temp)
        manifest_path, public_key, payload = fixture(root) if not args.manifest else (Path(args.manifest), root / "fixture-key.pub.pem", root / "signature-payload.json")
        manifest = json.loads(manifest_path.read_text())
        validate_shape(manifest)
        for artifact in manifest["artifacts"]:
            path = Path(args.artifact_root) / artifact["name"] if args.manifest else root / artifact["name"]
            if not path.is_file():
                fail(f"missing artifact: {path}")
            data = path.read_bytes()
            if len(data) != artifact["size"]:
                fail(f"size mismatch for {artifact['name']}")
            if hashlib.sha256(data).hexdigest() != artifact["sha256"]:
                fail(f"sha256 mismatch for {artifact['name']}")
        unsigned = dict(manifest)
        sig = unsigned.pop("signature")
        unsigned_digest = "sha256-" + hashlib.sha256(canonical(unsigned)).hexdigest()
        if sig["signed_digest"] != unsigned_digest:
            fail("manifest signed_digest does not match canonical unsigned manifest")
        print(f"manifest shape: PASS ({manifest_path.name})")
        print(f"artifact hashes/sizes: PASS ({len(manifest['artifacts'])} artifact)")
        print("signature envelope shape: PASS (cryptographic verification is delegated to tapid-signatures tests)")


if __name__ == "__main__":
    main()
