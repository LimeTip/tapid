import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
CHECKER = REPOSITORY_ROOT / "scripts" / "check_architecture.py"


class ArchitectureCheckerTests(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        subprocess.run(
            ["git", "init", "--quiet", str(self.root)],
            check=True,
            text=True,
        )

    def tearDown(self):
        self.temporary_directory.cleanup()

    def write(self, relative_path, content):
        path = self.root / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return path

    def track(self, *relative_paths):
        subprocess.run(
            ["git", "add", "--", *relative_paths],
            cwd=self.root,
            check=True,
            text=True,
        )

    def run_checker(self):
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            cwd=self.root,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_reports_review_recommendation_above_advisory_threshold(self):
        self.write("crates/example/src/lib.rs", "line\n" * 801)
        self.track("crates/example/src/lib.rs")

        result = self.run_checker()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(
            "crates/example/src/lib.rs: 801 physical lines exceeds the 800-line review recommendation",
            result.stdout,
        )
        self.assertNotIn("docs/architecture-exceptions.txt", result.stdout)

    def test_accepts_file_at_review_threshold(self):
        self.write("crates/example/src/lib.rs", "line\n" * 800)
        self.track("crates/example/src/lib.rs")

        result = self.run_checker()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_scans_production_module_named_build(self):
        self.write("crates/example/src/build/mod.rs", "line\n" * 801)
        self.track("crates/example/src/build/mod.rs")

        result = self.run_checker()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(
            "crates/example/src/build/mod.rs: 801 physical lines exceeds the 800-line review recommendation",
            result.stdout,
        )

    def test_rejects_exception_for_advisory_only_file(self):
        self.write("crates/example/src/lib.rs", "line\n" * 801)
        self.write(
            "docs/architecture-exceptions.txt",
            "crates/example/src/lib.rs | Cohesive parser retained after architecture review.\n",
        )
        self.track("crates/example/src/lib.rs", "docs/architecture-exceptions.txt")

        result = self.run_checker()

        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "exception path has no hard architecture threshold: crates/example/src/lib.rs",
            result.stdout,
        )

    def test_ignores_tests_generated_build_trees_and_untracked_files(self):
        excluded = (
            "crates/example/tests/large.rs",
            "crates/example/src/tests.rs",
            "tests/integration/large.rs",
            "target/debug/build/large.rs",
            "generated/large.rs",
        )
        for relative_path in excluded:
            self.write(relative_path, "line\n" * 801)
        self.write("crates/example/src/untracked.rs", "line\n" * 801)
        self.write("crates/example/src/lib.rs", "pub fn small() {}\n")
        self.track(*excluded, "crates/example/src/lib.rs")

        result = self.run_checker()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Architecture check passed: 1 production Rust file scanned", result.stdout)

    def test_rejects_exception_without_a_rationale(self):
        self.write("crates/example/src/lib.rs", "line\n" * 801)
        self.write("docs/architecture-exceptions.txt", "crates/example/src/lib.rs |\n")
        self.track("crates/example/src/lib.rs", "docs/architecture-exceptions.txt")

        result = self.run_checker()

        self.assertEqual(result.returncode, 1)
        self.assertIn("line 1 must contain a path and rationale", result.stdout)

    def test_rejects_exception_without_a_concrete_rationale(self):
        self.write("crates/example/src/lib.rs", "line\n" * 801)
        self.write("docs/architecture-exceptions.txt", "crates/example/src/lib.rs | x\n")
        self.track("crates/example/src/lib.rs", "docs/architecture-exceptions.txt")

        result = self.run_checker()

        self.assertEqual(result.returncode, 1)
        self.assertIn("line 1 rationale must be at least", result.stdout)

    def test_rejects_stale_exception_below_threshold(self):
        self.write("crates/tapid-cli/src/main.rs", "fn main() {}\n")
        self.write(
            "docs/architecture-exceptions.txt",
            "crates/tapid-cli/src/main.rs | This documented exception is no longer needed.\n",
        )
        self.track(
            "crates/tapid-cli/src/main.rs", "docs/architecture-exceptions.txt"
        )

        result = self.run_checker()

        self.assertEqual(result.returncode, 1)
        self.assertIn("exception is stale at 1 physical lines", result.stdout)

    def test_enforces_entrypoint_only_threshold_for_tapid_cli_main(self):
        self.write("crates/tapid-cli/src/main.rs", "line\n" * 101)
        self.track("crates/tapid-cli/src/main.rs")

        result = self.run_checker()

        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "crates/tapid-cli/src/main.rs: 101 physical lines exceeds entrypoint threshold 100",
            result.stdout,
        )

    def test_reports_advisories_in_path_order(self):
        self.write("crates/zeta/src/lib.rs", "line\n" * 801)
        self.write("crates/alpha/src/lib.rs", "line\n" * 802)
        self.track("crates/zeta/src/lib.rs", "crates/alpha/src/lib.rs")

        result = self.run_checker()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertLess(
            result.stdout.index("crates/alpha/src/lib.rs"),
            result.stdout.index("crates/zeta/src/lib.rs"),
        )


if __name__ == "__main__":
    unittest.main()
