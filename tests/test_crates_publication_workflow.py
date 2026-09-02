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
    def test_all_jobs_execute_only_freshly_fetched_current_main(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertEqual(workflow.count("ref: main"), 3)
        self.assertNotIn("ref: ${{ inputs.commit }}", workflow)
        self.assertEqual(
            workflow.count(
                "git fetch --no-tags origin +refs/heads/main:refs/remotes/origin/main"
            ),
            3,
        )
        self.assertEqual(
            workflow.count('test "$(git rev-parse origin/main)" = "$SOURCE_COMMIT"'),
            3,
        )
        self.assertNotIn("git merge-base --is-ancestor", workflow)

    def test_exact_archive_bundle_is_prepared_before_oidc_and_only_uploaded_afterward(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        prepare = workflow.index("Prepare exact reviewed archive bundle")
        auth = workflow.index("Obtain temporary crates.io credential")
        upload = workflow.index("Upload and read back exact reviewed archives")

        self.assertLess(prepare, auth)
        self.assertLess(auth, upload)
        self.assertIn("--prepare-bundle", workflow[prepare:auth])
        mutation = workflow[upload:workflow.index("Verify tapid through the registry", upload)]
        self.assertIn("--bundle", mutation)
        self.assertNotIn("cargo publish", mutation)
        self.assertNotIn("cargo package", mutation)
        self.assertNotIn("CARGO_REGISTRY_TOKEN", workflow[prepare:auth])

    def test_oidc_job_ends_with_a_bounded_http_only_upload(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        publish = workflow[workflow.index("  publish:"):workflow.index("  verify:")]
        auth = publish.index("Obtain temporary crates.io credential")
        upload = publish.index("Upload and read back exact reviewed archives")
        privileged_tail = publish[auth:]

        self.assertIn("Record credential phase start", publish[:auth])
        self.assertIn("--http-only", publish[upload:])
        self.assertIn("--credential-deadline", publish[upload:])
        self.assertIn(
            'timeout --foreground --signal=TERM --kill-after=5s "${remaining}s"',
            publish[upload:],
        )
        self.assertNotIn("cargo ", privileged_tail)
        self.assertNotIn("uses:", publish[upload:])

    def test_registry_cargo_verification_is_a_separate_credential_free_job(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        verification = workflow[workflow.index("  verify:"):]

        self.assertIn("needs: [preflight, publish]", verification)
        self.assertIn(
            "if: ${{ always() && inputs.dry_run == false && needs.preflight.result == 'success' }}",
            verification,
        )
        self.assertIn("--require-published", verification)
        self.assertIn("cargo install tapid", verification)
        self.assertIn('runner_rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"', verification)
        self.assertIn('mkdir -p "$verification_root/home"', verification)
        self.assertIn('export RUSTUP_HOME="$runner_rustup_home"', verification)
        self.assertLess(
            verification.index('runner_rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"'),
            verification.index('export HOME="$verification_root/home"'),
        )
        self.assertNotIn("id-token: write", verification)
        self.assertNotIn("CARGO_REGISTRY_TOKEN:", verification)
        self.assertNotIn("CARGO_REGISTRIES_CRATES_IO_TOKEN:", verification)
        self.assertNotIn("crates-auth.outputs.token", verification)


if __name__ == "__main__":
    unittest.main()
