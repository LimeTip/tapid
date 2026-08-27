#!/usr/bin/env python3
"""Validate the schema fields needed by the bootstrap installer.

Cryptographic verification is deliberately delegated to the configured verifier;
this helper never treats a checksum or JSON shape as authenticity.
"""
import json
import re
import sys
from pathlib import Path
from urllib.parse import urlparse


def fail(message):
    print(f"tapid installer: {message}", file=sys.stderr)
    raise SystemExit(1)


def main():
    if len(sys.argv) != 4:
        fail("manifest helper requires MANIFEST TARGET VERSION")
    path, target, version = map(Path, sys.argv[1:2]) + (sys.argv[2], sys.argv[3]) if False else (Path(sys.argv[1]), sys.argv[2], sys.argv[3])
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        fail(f"cannot parse signed release manifest: {exc}")
    if not isinstance(value, dict) or value.get("schema") != "tapid-release-manifest-v1" or value.get("product") != "tapid":
        fail("invalid signed release manifest identity")
    if value.get("version") != version.removeprefix("v") or value.get("tag") != version:
        fail("signed release manifest version does not match requested version")
    signature = value.get("signature")
    if not isinstance(signature, dict) or signature.get("algorithm") != "ed25519":
        fail("signed release manifest has no Ed25519 signature")
    artifacts = value.get("artifacts")
    if not isinstance(artifacts, list):
        fail("signed release manifest artifacts must be an array")
    matches = [a for a in artifacts if isinstance(a, dict) and a.get("target") == target]
    if len(matches) != 1:
        fail(f"signed release manifest must contain exactly one artifact for {target}")
    artifact = matches[0]
    name = artifact.get("name")
    expected_name = f"tapid-{version.removeprefix('v')}-{target}.tar.gz"
    if name != expected_name or not isinstance(artifact.get("url"), str) or urlparse(artifact["url"]).scheme != "https":
        fail("signed release manifest artifact identity or URL is invalid")
    digest = artifact.get("sha256")
    size = artifact.get("size")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        fail("signed release manifest artifact hash is invalid")
    if not isinstance(size, int) or isinstance(size, bool) or size < 1:
        fail("signed release manifest artifact size is invalid")
    print(f"{name}\t{artifact['url']}\t{digest}\t{size}")


if __name__ == "__main__":
    main()
