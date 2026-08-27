import base64
import hashlib
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).parent
MANIFEST = ROOT / "generate_manifest.py"
STABLE = ROOT / "stable_channel.py"
VERIFY = ROOT / "verify_manifest.py"
VALIDATE = ROOT / "validate_artifacts.py"
WORKFLOW = ROOT.parent / "workflows" / "release-publication.yml"


class ReleasePublicationTests(unittest.TestCase):
    def generated_key(self, directory):
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        key = Ed25519PrivateKey.generate()
        return key.private_bytes(serialization.Encoding.PEM,
                                 serialization.PrivateFormat.PKCS8,
                                 serialization.NoEncryption()).decode()

    def run_manifest(self, env, artifact, *extra):
        return subprocess.run(
            [
                "python3", str(MANIFEST), "--version", "0.0.7", "--tag", "v0.0.7",
                "--commit", "0123456789abcdef0123456789abcdef01234567",
                "--created-at", "2026-08-27T10:00:00Z", "--expires-at", "2026-09-27T10:00:00Z",
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

    def test_verifier_accepts_manifest_with_matching_public_key_and_rejects_tampering(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "tapid-0.0.7-x86_64-unknown-linux-gnu.tar.gz"
            artifact.write_bytes(b"release bytes\\n")
            from cryptography.hazmat.primitives import serialization
            from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
            private = Ed25519PrivateKey.generate()
            private_pem = private.private_bytes(serialization.Encoding.PEM,
                                                serialization.PrivateFormat.PKCS8,
                                                serialization.NoEncryption()).decode()
            public_pem = private.public_key().public_bytes(serialization.Encoding.PEM,
                                                           serialization.PublicFormat.SubjectPublicKeyInfo).decode()
            env = os.environ.copy()
            env["RELEASE_SIGNING_KEY"] = private_pem
            env["RELEASE_SIGNING_KEY_ID"] = "test-key"
            result = self.run_manifest(env, str(artifact))
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = Path(directory, "manifest.json")
            verify = subprocess.run(["python3", str(VERIFY), "--manifest", str(manifest),
                                     "--public-key", public_pem], text=True, capture_output=True)
            self.assertEqual(verify.returncode, 0, verify.stderr)
            data = json.loads(manifest.read_text())
            data["commit"] = "fedcba9876543210fedcba9876543210fedcba98"
            manifest.write_text(json.dumps(data))
            verify = subprocess.run(["python3", str(VERIFY), "--manifest", str(manifest),
                                     "--public-key", public_pem], text=True, capture_output=True)
            self.assertNotEqual(verify.returncode, 0)

    def test_workflow_checks_immutable_commit_tag_target_mapping_and_protected_verification(self):
        workflow = WORKFLOW.read_text()
        self.assertIn("ref: ${{ inputs.commit }}", workflow)
        self.assertIn("git rev-parse", workflow)
        self.assertIn("target mapping", workflow)
        self.assertIn("verify_manifest.py", workflow)
        self.assertIn("environment:", workflow)
        self.assertNotIn("source fallback", workflow.lower())

    def test_artifact_validation_rejects_unknown_names_and_emits_target_mapping(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "artifacts"
            root.mkdir()
            valid = root / "tapid-0.0.7-aarch64-pc-windows-msvc.zip"
            valid.write_bytes(b"zip bytes")
            args = Path(directory) / "args"
            result = subprocess.run(["python3", str(VALIDATE), "--version", "0.0.7",
                                     "--root", str(root), "--output", str(args)],
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(args.read_text().splitlines(), [
                "--artifact", f"aarch64-pc-windows-msvc={valid}"])
            (root / "not-a-release.txt").write_text("reject")
            result = subprocess.run(["python3", str(VALIDATE), "--version", "0.0.7",
                                     "--root", str(root), "--output", str(args)],
                                    text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0)

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
