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
