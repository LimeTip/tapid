import base64
import hashlib
import json
import os
import subprocess
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).parent
MANIFEST = ROOT / "generate_manifest.py"
STABLE = ROOT / "stable_channel.py"
VERIFIER = ROOT / "verifier" / "Cargo.toml"


class ReleasePublicationTests(unittest.TestCase):
    def generated_key(self, directory):
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        key = Ed25519PrivateKey.generate()
        return key.private_bytes(serialization.Encoding.PEM,
                                 serialization.PrivateFormat.PKCS8,
                                 serialization.NoEncryption()).decode()

    def run_manifest(self, env, artifact, *extra):
        now = datetime.now(timezone.utc).replace(microsecond=0)
        created_at = (now - timedelta(hours=1)).isoformat().replace("+00:00", "Z")
        expires_at = (now + timedelta(days=30)).isoformat().replace("+00:00", "Z")
        return subprocess.run(
            [
                "python3", str(MANIFEST), "--version", "0.0.7", "--tag", "v0.0.7",
                "--commit", "0123456789abcdef0123456789abcdef01234567",
                "--created-at", created_at, "--expires-at", expires_at,
                "--base-url", "https://downloads.example.test/v0.0.7",
                "--artifact", f"x86_64-unknown-linux-gnu={artifact}",
                "--output", str(Path(artifact).parent / "manifest.json"), *extra,
            ], env=env, text=True, capture_output=True,
        )

    def test_manifest_hashes_exact_artifact_and_has_verifiable_ed25519_signature(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "tapid-0.0.7-x86_64-unknown-linux-gnu.tar.gz"
            artifact.write_bytes(b"release bytes\n")
            env = os.environ.copy()
            env["RELEASE_SIGNING_KEY"] = self.generated_key(directory)
            env["RELEASE_SIGNING_KEY_ID"] = "test-key"
            result = self.run_manifest(env, str(artifact))
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads(Path(directory, "manifest.json").read_text())
            self.assertEqual(manifest["artifacts"][0]["size"], artifact.stat().st_size)
            self.assertEqual(manifest["artifacts"][0]["sha256"], hashlib.sha256(artifact.read_bytes()).hexdigest())
            self.assertEqual(manifest["signature"]["algorithm"], "ed25519")
            self.assertEqual(manifest["signature"]["key_id"], "test-key")
            self.assertEqual(len(base64.b64decode(manifest["signature"]["value"])), 64)
            self.assertTrue(manifest["signature"]["signed_digest"].startswith("sha256-"))

    def test_manifest_canonicalization_uses_rfc8785_utf16_key_order(self):
        import importlib.util

        spec = importlib.util.spec_from_file_location("generate_manifest", MANIFEST)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual(
            module.canonical({"😀": 1, "\ue000": 2}),
            '{"😀":1,"\ue000":2}'.encode(),
        )

    def test_manifest_fails_closed_without_key_or_required_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "tapid-0.0.7-x86_64-unknown-linux-gnu.tar.gz"
            env = os.environ.copy()
            env.pop("RELEASE_SIGNING_KEY", None)
            env["RELEASE_SIGNING_KEY_ID"] = "test-key"
            result = self.run_manifest(env, str(artifact))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("artifact does not exist", result.stderr)
            artifact.write_bytes(b"bytes")
            result = self.run_manifest(env, str(artifact))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("RELEASE_SIGNING_KEY", result.stderr)

    def test_manifest_rejects_malformed_or_noncanonical_freshness_timestamps(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "tapid-0.0.7-x86_64-unknown-linux-gnu.tar.gz"
            artifact.write_bytes(b"bytes")
            env = os.environ.copy()
            env["RELEASE_SIGNING_KEY"] = self.generated_key(directory)
            env["RELEASE_SIGNING_KEY_ID"] = "test-key"
            cases = (
                ("not-a-time", "2026-09-27T10:00:00Z"),
                ("2026-08-27T10:00:00", "2026-09-27T10:00:00Z"),
                ("2026-08-27T10:00:00+00:00", "2026-09-27T10:00:00Z"),
            )
            for created_at, expires_at in cases:
                with self.subTest(created_at=created_at, expires_at=expires_at):
                    result = self.run_manifest(
                        env,
                        str(artifact),
                        "--created-at",
                        created_at,
                        "--expires-at",
                        expires_at,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("canonical UTC RFC3339", result.stderr)

    def test_manifest_requires_expiry_after_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "tapid-0.0.7-x86_64-unknown-linux-gnu.tar.gz"
            artifact.write_bytes(b"bytes")
            env = os.environ.copy()
            env["RELEASE_SIGNING_KEY"] = self.generated_key(directory)
            env["RELEASE_SIGNING_KEY_ID"] = "test-key"
            for expires_at in ("2026-08-27T10:00:00Z", "2026-08-27T09:59:59Z"):
                with self.subTest(expires_at=expires_at):
                    result = self.run_manifest(
                        env,
                        str(artifact),
                        "--expires-at",
                        expires_at,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("expires_at must be later than created_at", result.stderr)

    def test_manifest_requires_a_current_freshness_window(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "tapid-0.0.7-x86_64-unknown-linux-gnu.tar.gz"
            artifact.write_bytes(b"bytes")
            env = os.environ.copy()
            env["RELEASE_SIGNING_KEY"] = self.generated_key(directory)
            env["RELEASE_SIGNING_KEY_ID"] = "test-key"
            now = datetime.now(timezone.utc).replace(microsecond=0)
            future = self.run_manifest(
                env,
                str(artifact),
                "--created-at",
                (now + timedelta(days=1)).isoformat().replace("+00:00", "Z"),
                "--expires-at",
                (now + timedelta(days=2)).isoformat().replace("+00:00", "Z"),
            )
            self.assertNotEqual(future.returncode, 0)
            self.assertIn("created_at must not be in the future", future.stderr)

            expired = self.run_manifest(
                env,
                str(artifact),
                "--created-at",
                (now - timedelta(days=2)).isoformat().replace("+00:00", "Z"),
                "--expires-at",
                (now - timedelta(days=1)).isoformat().replace("+00:00", "Z"),
            )
            self.assertNotEqual(expired.returncode, 0)
            self.assertIn("expires_at must be in the future", expired.stderr)

    def test_rust_verifier_accepts_manifest_signed_by_production_protocol(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "tapid-0.0.7-x86_64-unknown-linux-gnu.tar.gz"
            artifact.write_bytes(b"release bytes\\n")
            from cryptography.hazmat.primitives import serialization
            from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
            key = Ed25519PrivateKey.generate()
            env = os.environ.copy()
            env["RELEASE_SIGNING_KEY"] = key.private_bytes(
                serialization.Encoding.PEM,
                serialization.PrivateFormat.PKCS8,
                serialization.NoEncryption(),
            ).decode()
            env["RELEASE_SIGNING_KEY_ID"] = "test-key"
            result = self.run_manifest(env, str(artifact))
            self.assertEqual(result.returncode, 0, result.stderr)
            keyring = Path(directory) / "keyring.json"
            public_key = key.public_key().public_bytes(
                serialization.Encoding.Raw,
                serialization.PublicFormat.Raw,
            )
            keyring.write_text(json.dumps({
                "version": "tapid-release-keyring-v1",
                "keys": [{
                    "key_id": "test-key",
                    "algorithm": "ed25519",
                    "public_key": base64.b64encode(public_key).decode(),
                    "fingerprint": "sha256-" + hashlib.sha256(public_key).hexdigest(),
                }],
            }))
            verify = subprocess.run(
                ["cargo", "run", "--quiet", "--manifest-path", str(VERIFIER), "--",
                 str(Path(directory) / "manifest.json"), str(keyring), "0.0.7", "v0.0.7",
                 "0123456789abcdef0123456789abcdef01234567"],
                cwd=ROOT.parent.parent.parent,
                text=True, capture_output=True,
            )
            self.assertEqual(verify.returncode, 0, verify.stderr)
            self.assertIn("verified", verify.stdout)
            manifest = json.loads(Path(directory, "manifest.json").read_text())
            manifest["version"] = "0.0.8"
            Path(directory, "manifest.json").write_text(json.dumps(manifest))
            rejected = subprocess.run(
                ["cargo", "run", "--quiet", "--manifest-path", str(VERIFIER), "--",
                 str(Path(directory) / "manifest.json"), str(keyring), "0.0.7", "v0.0.7",
                 "0123456789abcdef0123456789abcdef01234567"],
                cwd=ROOT.parent.parent.parent,
                text=True, capture_output=True,
            )
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("does not match workflow input", rejected.stderr)

    def test_stable_channel_preserves_endpoint_order_and_requires_https(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "stable.json"
            result = subprocess.run(
                ["python3", str(STABLE), "--output", str(output),
                 "--endpoint", "https://one.example/manifest.json",
                 "--endpoint", "https://two.example/manifest.json"],
                text=True, capture_output=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(output.read_text()), {
                "channel": "stable",
                "manifests": [
                    "https://one.example/manifest.json",
                    "https://two.example/manifest.json",
                ],
            })
            result = subprocess.run(
                ["python3", str(STABLE), "--output", str(output), "--endpoint", "http://insecure/manifest"],
                text=True, capture_output=True,
            )
            self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
