import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "crates-publication.yml"


class CratesPublicationWorkflowTests(unittest.TestCase):
    def test_reviewed_digest_validation_accepts_only_prefixed_sha256(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn(
            '[[ "$REVIEWED_DIGEST" =~ ^sha256-[0-9a-f]{64}$ ]]',
            workflow,
        )
        self.assertNotIn(
            '[[ "$REVIEWED_DIGEST" =~ ^[0-9a-f]{64}$ ]]',
            workflow,
        )
    def test_preflight_recovers_a_digest_bound_exact_publication_prefix(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn('recomputed-plan.json', workflow)
        self.assertIn('import crates_publish', workflow)
        self.assertIn('crates_publish.recover_reviewed_plan(', workflow)
        self.assertIn('"crates-plan.json"', workflow)
    def test_both_jobs_execute_only_freshly_fetched_current_main(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertEqual(workflow.count("ref: main"), 2)
        self.assertNotIn("ref: ${{ inputs.commit }}", workflow)
        self.assertEqual(
            workflow.count(
                "git fetch --no-tags origin +refs/heads/main:refs/remotes/origin/main"
            ),
            2,
        )
        self.assertEqual(
            workflow.count('test "$(git rev-parse origin/main)" = "$SOURCE_COMMIT"'),
            2,
        )
        self.assertNotIn("git merge-base --is-ancestor", workflow)


if __name__ == "__main__":
    unittest.main()
