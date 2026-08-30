import os
import subprocess
import shutil
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INSTALL = ROOT / "scripts" / "install.sh"
INSTALL_PS1 = ROOT / "scripts" / "install.ps1"
BOOTSTRAP_VERIFIER = ROOT / "scripts" / "bootstrap_verifier.py"
UNINSTALL = ROOT / "scripts" / "uninstall.sh"


class InstallerScriptTests(unittest.TestCase):
    def isolated_env(self, env=None):
        isolated = dict(os.environ if env is None else env)
        home = tempfile.mkdtemp(prefix="tapid-test-home-")
        isolated.setdefault("HOME", home)
        isolated.setdefault("TAPID_INSTALL_DIR", str(Path(home) / ".local" / "bin"))
        return isolated, home

    def run_script(self, script: Path, *args, env=None):
        isolated, home = self.isolated_env(env)
        try:
            return subprocess.run(
                ["bash", str(script), *args],
                cwd=ROOT,
                text=True,
                capture_output=True,
                env=isolated,
            )
        finally:
            shutil.rmtree(home, ignore_errors=True)

    def run_posix_script(self, script: Path, *args, env=None):
        isolated, home = self.isolated_env(env)
        try:
            return subprocess.run(
                ["sh", str(script), *args],
                cwd=ROOT,
                text=True,
                capture_output=True,
                env=isolated,
            )
        finally:
            shutil.rmtree(home, ignore_errors=True)


    def test_install_help_documents_latest_and_explicit_version(self):
        result = self.run_script(INSTALL, "--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("stable release", result.stdout)
        self.assertIn("--version VERSION", result.stdout)
        self.assertIn("--source-ref REF", result.stdout)


    def test_install_help_runs_under_posix_sh(self):
        result = self.run_posix_script(INSTALL, "--help")
        self.assertEqual(result.returncode, 0, result.stderr)


    def test_install_rejects_invalid_version_before_network_access(self):
        result = self.run_script(INSTALL, "--version", "not-a-version")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("version must be a stable release", result.stderr)

    def test_install_rejects_malformed_repository(self):
        result = self.run_script(INSTALL, "--repo", "not-a-repository")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("repository must be OWNER/REPO", result.stderr)

    def test_install_rejects_multiline_repository(self):
        result = self.run_script(INSTALL, "--repo", "LimeTip/tapid\nother")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("repository must be OWNER/REPO", result.stderr)

    def test_install_rejects_multiline_version(self):
        result = self.run_script(INSTALL, "--version", "v0.1.0\nother")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("version must be a stable release", result.stderr)

    def test_uninstall_rejects_relative_install_directory(self):
        result = self.run_script(UNINSTALL, "--install-dir", "relative/bin")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("absolute path", result.stderr)


    def test_install_rejects_combining_release_and_source_selection(self):
        result = self.run_script(
            INSTALL, "--version", "v0.1.0", "--source-ref", "main"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("either --version or --source-ref", result.stderr)

    def test_install_rejects_latest_version_with_source_selection(self):
        result = self.run_script(
            INSTALL, "--version", "latest", "--source-ref", "main"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("either --version or --source-ref", result.stderr)

    def test_install_rejects_empty_source_ref(self):
        result = self.run_script(INSTALL, "--source-ref", "")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("non-empty value", result.stderr)

    def test_stable_discovery_endpoint_is_provider_configurable(self):
        install_text = INSTALL.read_text()
        self.assertIn('TAPID_RELEASE_BASE_URL', install_text)
        self.assertIn('TAPID_RELEASE_DISCOVERY_URL', install_text)
        self.assertIn('TAPID_RELEASE_BASE_URL', install_text.split('Environment:', 1)[1])

    def test_stable_install_embeds_production_verifier_without_external_configuration(self):
        install_text = INSTALL.read_text()
        self.assertNotIn("TAPID_RELEASE_VERIFIER", install_text)
        self.assertNotIn("TAPID_RELEASE_TRUSTED_KEYS", install_text)
        self.assertIn("eYPvN15Ah8ytHoBd2jY+36Wh/5g1kbqhDA9TL6wPRWc=", install_text)
        self.assertIn("signature_context", install_text)
        self.assertIn("sha256-", install_text)
        self.assertNotIn("checksums.txt", install_text)

    def test_signed_manifest_is_verified_before_artifact_download(self):
        install_text = INSTALL.read_text()
        verify = install_text.index("signature_context")
        artifact = install_text.index('curl -fsSL "$artifact_url"')
        self.assertLess(verify, artifact)
        for field in ("target", "version", "size", "sha256"):
            self.assertIn(field, install_text[verify:artifact])

    def test_bootstrap_verifier_is_self_contained_and_fails_closed(self):
        install_text = INSTALL.read_text()
        self.assertNotIn("pkeyutl", install_text)
        self.assertNotIn("openssl", install_text)
        self.assertIn("RFC 8032", install_text)
        self.assertIn("unsupported Ed25519 verifier", install_text)
        self.assertNotIn("release_manifest.py", install_text)

    def test_embedded_verifiers_match_standalone_verifier(self):
        standalone = BOOTSTRAP_VERIFIER.read_text()
        expected = standalone[standalone.index("# RFC 8032"):standalone.index("def verify(")]

        for script in (INSTALL, INSTALL_PS1):
            embedded = script.read_text()
            self.assertIn(expected, embedded, script.name)

    def test_bootstrap_verifier_accepts_and_rejects_rfc8032_vector(self):
        import sys
        sys.path.insert(0, str(ROOT / "scripts"))
        import bootstrap_verifier

        public_key = bytes.fromhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
        signature = bytes.fromhex(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155"
            "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        )
        self.assertTrue(bootstrap_verifier.verify_ed25519(public_key, signature, b""))
        self.assertFalse(bootstrap_verifier.verify_ed25519(public_key, signature, b"tampered"))
        self.assertFalse(bootstrap_verifier.verify_ed25519(public_key, signature[:-1], b""))
        self.assertFalse(bootstrap_verifier.verify_ed25519(public_key, signature + b"\0", b""))
        self.assertFalse(bootstrap_verifier.verify_ed25519(public_key, signature[:32] + bootstrap_verifier.L.to_bytes(32, "little"), b""))
        # RFC 8032 uses the cofactored equation and accepts valid torsion
        # points; rejecting them would not be RFC-compatible.
        torsion_public_key = bytes.fromhex("ec" + "ff" * 30 + "7f")
        identity_r = bytes.fromhex("01" + "00" * 31)
        zero_signature = identity_r + (0).to_bytes(32, "little")
        self.assertTrue(bootstrap_verifier.verify_ed25519(torsion_public_key, zero_signature, b""))

    def test_posix_installer_splits_verified_artifact_fields(self):
        read_line = next(
            line.strip()
            for line in INSTALL.read_text().splitlines()
            if line.strip().startswith("IFS=")
            and "archive artifact_url expected expected_size" in line
        )
        shell = f"""artifact_info=$(printf 'tapid.tar.gz\thttps://example.test/a\tdeadbeef\t42')
{read_line}
$artifact_info
EOF
printf '%s|%s|%s|%s\n' "$archive" "$artifact_url" "$expected" "$expected_size"
"""
        result = subprocess.run(
            ["sh", "-c", shell],
            text=True,
            capture_output=True,
            env={"PATH": os.environ["PATH"]},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            "tapid.tar.gz|https://example.test/a|deadbeef|42\n",
        )


    def test_uninstall_removes_only_tapid_binary(self):
        with tempfile.TemporaryDirectory() as tmp:
            install_dir = Path(tmp) / "bin"
            install_dir.mkdir()
            binary = install_dir / "tapid"
            marker = install_dir / "keep-me"
            binary.write_text("fake binary")
            marker.write_text("unrelated")
            result = self.run_script(
                UNINSTALL,
                "--install-dir",
                str(install_dir),
                env={**os.environ, "TAPID_INSTALL_DIR": str(install_dir)},
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(binary.exists())
            self.assertTrue(marker.exists())

    def test_uninstall_refuses_symlink_destination(self):
        with tempfile.TemporaryDirectory() as tmp:
            install_dir = Path(tmp) / "bin"
            install_dir.mkdir()
            target = Path(tmp) / "outside-file"
            target.write_text("keep")
            binary = install_dir / "tapid"
            binary.symlink_to(target)
            result = self.run_script(UNINSTALL, "--install-dir", str(install_dir))
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(binary.is_symlink())
            self.assertTrue(target.exists())

    def test_uninstall_refuses_dangling_symlink_destination(self):
        with tempfile.TemporaryDirectory() as tmp:
            install_dir = Path(tmp) / "bin"
            install_dir.mkdir()
            binary = install_dir / "tapid"
            binary.symlink_to(Path(tmp) / "missing-target")
            result = self.run_script(UNINSTALL, "--install-dir", str(install_dir))
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(binary.is_symlink())



if __name__ == "__main__":
    unittest.main()
