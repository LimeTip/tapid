import os
import shutil
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INSTALL = ROOT / "scripts" / "install.ps1"
UNINSTALL = ROOT / "scripts" / "uninstall.ps1"


class WindowsInstallerScriptTests(unittest.TestCase):
    def test_windows_scripts_exist_and_cover_source_lifecycle(self):
        self.assertTrue(INSTALL.is_file())
        self.assertTrue(UNINSTALL.is_file())
        install_text = INSTALL.read_text()
        uninstall_text = UNINSTALL.read_text()
        for expected in ("SourceRef", "cargo install", "Move-Item", "Repo"):
            self.assertIn(expected, install_text)
        for expected in ("tapid.exe", "ReparsePoint", "Remove-Item"):
            self.assertIn(expected, uninstall_text)

    def test_windows_stable_and_source_paths_are_explicitly_separate(self):
        install_text = INSTALL.read_text()
        self.assertIn("TAPID_RELEASE_BASE_URL", install_text)
        self.assertIn("TAPID_RELEASE_DISCOVERY_URL", install_text)
        self.assertNotIn('$SourceRef = "main"', install_text)
        self.assertIn("stable release discovery endpoint", install_text)

    def test_windows_stable_install_embeds_production_verifier_without_external_configuration(self):
        install_text = INSTALL.read_text()
        self.assertNotIn("TAPID_RELEASE_VERIFIER", install_text)
        self.assertNotIn("TAPID_RELEASE_TRUSTED_KEYS", install_text)
        self.assertIn("eYPvN15Ah8ytHoBd2jY+36Wh/5g1kbqhDA9TL6wPRWc=", install_text)
        self.assertIn("signature_context", install_text)
        self.assertNotIn("checksums.txt", install_text)

    def test_windows_signed_manifest_is_verified_before_artifact_download(self):
        install_text = INSTALL.read_text()
        verify = install_text.index("signature_context")
        artifact = install_text.index('Invoke-WebRequest -UseBasicParsing $artifactUrl')
        self.assertLess(verify, artifact)
        for field in ("target", "version", "size", "sha256"):
            self.assertIn(field, install_text[verify:artifact])

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell Core is not installed")
    def test_windows_scripts_parse_with_powershell(self):
        for script in (INSTALL, UNINSTALL):
            environment = dict(os.environ)
            environment["TAPID_SCRIPT_PATH"] = str(script)
            result = subprocess.run(
                [
                    "pwsh",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "[scriptblock]::Create((Get-Content -Raw -LiteralPath $env:TAPID_SCRIPT_PATH)) | Out-Null",
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                env=environment,
            )
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
