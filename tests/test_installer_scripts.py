import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INSTALL = ROOT / "scripts" / "install.sh"
UNINSTALL = ROOT / "scripts" / "uninstall.sh"


class InstallerScriptTests(unittest.TestCase):
    def run_script(self, script: Path, *args, env=None):
        return subprocess.run(
            ["bash", str(script), *args],
            cwd=ROOT,
            text=True,
            capture_output=True,
            env=env,
        )

    def run_posix_script(self, script: Path, *args, env=None):
        return subprocess.run(
            ["sh", str(script), *args],
            cwd=ROOT,
            text=True,
            capture_output=True,
            env=env,
        )


    def test_install_help_documents_latest_and_explicit_version(self):
        result = self.run_script(INSTALL, "--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("latest stable", result.stdout)
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



if __name__ == "__main__":
    unittest.main()
