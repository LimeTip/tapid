#!/usr/bin/env python3
"""Verify a Tapid manifest's canonical Ed25519 signature."""
import argparse
import base64
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()


def verify(payload, signature, pem):
    try:
        from cryptography.hazmat.primitives import serialization
        key = serialization.load_pem_public_key(pem.encode())
        key.verify(signature, payload)
        return
    except ImportError:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "key.pem").write_text(pem)
            (root / "payload").write_bytes(payload)
            (root / "signature").write_bytes(signature)
            result = subprocess.run(
                ["openssl", "pkeyutl", "-verify", "-rawin", "-pubin",
                 "-inkey", str(root / "key.pem"), "-in", str(root / "payload"),
                 "-sigfile", str(root / "signature")],
                capture_output=True, text=True,
            )
            if result.returncode:
                raise ValueError("manifest signature verification failed")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--public-key", required=True)
    args = parser.parse_args()
    manifest = json.loads(Path(args.manifest).read_text())
    signature = manifest.get("signature")
    if not isinstance(signature, dict) or signature.get("algorithm") != "ed25519":
        raise ValueError("manifest is not signed with Ed25519")
    unsigned = dict(manifest)
    unsigned.pop("signature", None)
    expected_digest = "sha256-" + hashlib.sha256(canonical(unsigned)).hexdigest()
    if signature.get("signed_digest") != expected_digest:
        raise ValueError("manifest signed digest does not match canonical content")
    context = dict(unsigned)
    context["signature_context"] = {
        "algorithm": signature.get("algorithm"),
        "key_id": signature.get("key_id"),
        "signed_digest": signature.get("signed_digest"),
    }
    try:
        value = base64.b64decode(signature.get("value", ""), validate=True)
        verify(canonical(context), value, args.public_key)
    except Exception as exc:
        raise ValueError(f"manifest signature verification failed: {exc}") from exc
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"error: {exc}")
        raise SystemExit(2)
