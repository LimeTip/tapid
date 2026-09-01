import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "crates-publication.yml"


class CratesPublicationWorkflowTests(unittest.TestCase):
    def test_reviewed_inputs_require_canonical_validation(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn('[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]]', workflow)
        self.assertIn(
            '[[ "$REVIEWED_DIGEST" =~ ^sha256-[0-9a-f]{64}$ ]]',
            workflow,
        )
        self.assertNotIn(
            '[[ "$REVIEWED_DIGEST" =~ ^[0-9a-f]{64}$ ]]',
            workflow,
        )

    def test_dispatch_input_expressions_never_appear_in_run_scripts(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        lines = workflow.splitlines()
        run_scripts = []
        for index, line in enumerate(lines):
            if line.lstrip() != "run: |":
                continue
            indentation = len(line) - len(line.lstrip())
            script_lines = []
            for candidate in lines[index + 1 :]:
                candidate_indentation = len(candidate) - len(candidate.lstrip())
                if candidate.strip() and candidate_indentation <= indentation:
                    break
                script_lines.append(candidate)
            run_scripts.append("\n".join(script_lines))

        self.assertTrue(run_scripts)
        for script in run_scripts:
            with self.subTest(script=script[:80]):
                self.assertNotRegex(
                    script,
                    r"\$\{\{\s*inputs\.(?:commit|plan_digest)\s*\}\}",
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
