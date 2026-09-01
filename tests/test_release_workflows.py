import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CI_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"


class ReleaseWorkflowTests(unittest.TestCase):
    def test_ci_runs_release_and_installer_tests_and_actionlint(self):
        workflow = CI_WORKFLOW.read_text()
        required_commands = (
            "python3 -m unittest discover -s .github/release -p 'test_*.py' -v",
            "python3 tests/test_installer_scripts.py",
            "actionlint .github/workflows/*.yml",
        )

        for command in required_commands:
            with self.subTest(command=command):
                self.assertIn(command, workflow)


if __name__ == "__main__":
    unittest.main()
