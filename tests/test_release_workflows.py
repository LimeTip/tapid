import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CI_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
CRATES_WORKFLOW = ROOT / ".github" / "workflows" / "crates-publication.yml"
RELEASE_WORKFLOW = ROOT / ".github" / "workflows" / "release-publication.yml"


class ReleaseWorkflowTests(unittest.TestCase):
    def release_job(self, name, next_name):
        workflow = RELEASE_WORKFLOW.read_text()
        return workflow.split(f"  {name}:\n", 1)[1].split(f"  {next_name}:\n", 1)[0]

    def test_ci_runs_release_and_installer_tests_and_actionlint(self):
        workflow = CI_WORKFLOW.read_text()
        required_commands = (
            "python3 -m unittest discover -s .github/release -p 'test_*.py' -v",
            "python3 -m unittest tests.test_release_workflows tests.test_crates_publication_workflow -v",
            "python3 tests/test_installer_scripts.py",
            "actionlint .github/workflows/*.yml",
        )

        for command in required_commands:
            with self.subTest(command=command):
                self.assertIn(command, workflow)

    def test_binary_publication_requires_annotated_tag_and_canonical_inputs(self):
        workflow = (
            ROOT / ".github" / "workflows" / "release-publication.yml"
        ).read_text()

        required_contracts = (
            'git cat-file -t "$RELEASE_TAG"',
            "release_identity.validate_version_tag",
            "release_identity.validate_commit",
            "release_identity.github_release_urls",
            "release_identity.freshness_timestamps",
        )
        for contract in required_contracts:
            with self.subTest(contract=contract):
                self.assertIn(contract, workflow)

    def test_privileged_release_jobs_execute_only_fresh_current_main_tooling(self):
        for name, next_name in (
            ("sign-manifest", "advance-stable"),
            ("advance-stable", "public-smoke"),
        ):
            job = self.release_job(name, next_name)
            with self.subTest(job=name):
                self.assertIn("ref: main", job)
                self.assertIn("fetch-depth: 0", job)
                self.assertIn("persist-credentials: false", job)
                self.assertIn(
                    "git fetch --no-tags origin +refs/heads/main:refs/remotes/origin/main",
                    job,
                )
                self.assertIn(
                    'test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"',
                    job,
                )
                self.assertNotIn("ref: ${{ inputs.commit }}", job)

    def test_privileged_jobs_keep_tagged_source_separate_from_trusted_tools(self):
        for name, next_name in (
            ("sign-manifest", "advance-stable"),
            ("advance-stable", "public-smoke"),
        ):
            job = self.release_job(name, next_name)
            with self.subTest(job=name):
                self.assertIn("name: validated-release-source", job)
                self.assertIn("tagged-release-source", job)
                self.assertIn(
                    "--manifest-path .github/release/verifier/Cargo.toml", job
                )
                self.assertIn(
                    "tagged-release-source/crates/tapid-signatures/data/release-keyring.json",
                    job,
                )
                self.assertNotIn("tagged-release-source/.github/release", job)
                self.assertNotIn("tagged-release-source/scripts", job)

    def test_signing_key_is_not_available_to_cargo_execution(self):
        job = self.release_job("sign-manifest", "advance-stable")
        signing_step = job.split("      - name: Generate signed manifest", 1)[1].split(
            "      - name:", 1
        )[0]
        self.assertIn("RELEASE_SIGNING_KEY:", signing_step)
        self.assertIn("generate_manifest.py", signing_step)
        self.assertNotIn("cargo ", signing_step)

        verifier_step = job.split("      - name: Verify signed manifest", 1)[1].split(
            "      - name:", 1
        )[0]
        self.assertIn("cargo run", verifier_step)
        self.assertNotIn("RELEASE_SIGNING_KEY", verifier_step)

    def test_write_token_steps_never_execute_python_or_cargo(self):
        job = self.release_job("advance-stable", "public-smoke")
        token_steps = job.split("GH_TOKEN: ${{ github.token }}")
        self.assertGreaterEqual(len(token_steps), 4)
        for suffix in token_steps[1:]:
            step = suffix.split("      - name:", 1)[0]
            with self.subTest(step=step[:80]):
                self.assertNotIn("python", step)
                self.assertNotIn("cargo ", step)
                self.assertNotIn("bootstrap_verifier", step)

    def test_binary_publication_verifies_downloaded_draft_bytes_immediately_before_promotion(self):
        workflow = (
            ROOT / ".github" / "workflows" / "release-publication.yml"
        ).read_text()
        publication = workflow.split("      - name: Publish signed release assets", 1)[1]

        required_contracts = (
            'gh release download "$RELEASE_TAG" --dir downloaded-draft-assets',
            "verify_downloaded_release_assets(",
            "downloaded-draft-assets",
            "production_rust_verifier(",
            "tagged-release-source/crates/tapid-signatures/data/release-keyring.json",
            "verifier=tagged_release_verifier",
            'git cat-file -t "$RELEASE_TAG"',
            'git rev-list -n 1 "$RELEASE_TAG^{commit}"',
            'gh release edit "$RELEASE_TAG" --draft=false --latest',
        )
        for contract in required_contracts:
            with self.subTest(contract=contract):
                self.assertIn(contract, publication)

        self.assertLess(
            publication.index("verify_downloaded_release_assets("),
            publication.index('gh release edit "$RELEASE_TAG" --draft=false --latest'),
        )
        between_verification_and_promotion = publication.split(
            "verify_downloaded_release_assets(", 1
        )[1].split('gh release edit "$RELEASE_TAG" --draft=false --latest', 1)[0]
        self.assertNotIn("gh release upload", between_verification_and_promotion)

    def test_public_smoke_is_reusable_and_runs_after_publication(self):
        smoke = (
            ROOT / ".github" / "workflows" / "release-public-smoke.yml"
        ).read_text()
        publication = (
            ROOT / ".github" / "workflows" / "release-publication.yml"
        ).read_text()

        self.assertIn("workflow_call:", smoke)
        self.assertIn("uses: ./.github/workflows/release-public-smoke.yml", publication)
        self.assertIn("needs: advance-stable", publication)
        self.assertIn("mode: stable", publication)
        public_smoke_call = publication.split("  public-smoke:", 1)[1]
        self.assertNotIn("tag:", public_smoke_call.split("expected_tag:", 1)[0])
        self.assertIn("expected_version: ${{ inputs.version }}", public_smoke_call)
        self.assertIn("expected_tag: ${{ inputs.tag }}", public_smoke_call)
        self.assertIn("expected_commit: ${{ inputs.commit }}", public_smoke_call)
        for expected_input in ("expected_version:", "expected_tag:", "expected_commit:"):
            self.assertIn(expected_input, smoke)
        self.assertIn('test "$version" = "$EXPECTED_VERSION"', smoke)
        self.assertIn('test "$resolved_tag" = "$EXPECTED_TAG"', smoke)
        self.assertIn('test "$resolved_commit" = "$EXPECTED_COMMIT"', smoke)

    def test_public_smoke_proves_latest_default_installers_website_and_run_evidence(self):
        smoke = (
            ROOT / ".github" / "workflows" / "release-public-smoke.yml"
        ).read_text()
        verifier = (ROOT / ".github" / "release" / "public_release.py").read_text()

        self.assertIn('args=(--mode "$MODE" --output public-release-evidence.json)', smoke)
        self.assertNotIn("--skip-website", smoke)
        self.assertIn('--workflow-run-id "${{ github.run_id }}"', smoke)
        self.assertIn("${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }}", smoke)
        self.assertIn('if [[ "$MODE" == stable ]]; then', smoke)
        self.assertIn('sh "$root/install.sh" --install-dir "$install_root"', smoke)
        self.assertIn("& $installer -InstallDir $installDir", smoke)
        self.assertIn('"/releases/latest" if mode == "stable"', verifier)
        self.assertIn('"https://tapid.dev/install.sh"', verifier)
        self.assertIn('"https://tapid.dev/install.ps1"', verifier)
        self.assertIn('"https://tapid.dev/docs/getting-started/"', verifier)
        self.assertIn('"https://tapid.dev/docs/releases/" + version + "/"', verifier)

    def test_windows_public_smoke_initializes_installer_exit_status(self):
        smoke = (
            ROOT / ".github" / "workflows" / "release-public-smoke.yml"
        ).read_text()
        windows_install = smoke.split(
            "      - name: Install and verify the public binary", 1
        )[1]
        before_exit_check = windows_install.split(
            "if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }", 1
        )[0]

        self.assertIn("$LASTEXITCODE = 0", before_exit_check)
        self.assertLess(
            before_exit_check.index("$LASTEXITCODE = 0"),
            before_exit_check.index("& $installer"),
        )

    def test_crates_publication_is_digest_bound_protected_and_resumable(self):
        workflow = CRATES_WORKFLOW.read_text()
        required_contracts = (
            "commit:",
            "plan_digest:",
            "dry_run:",
            "group: crates-publication",
            "cancel-in-progress: false",
            "contents: read",
            "environment: crates-io-release",
            "id-token: write",
            "rust-lang/crates-io-auth-action@c6f97d42243bad5fab37ca0427f495c86d5b1a18",
            "actions/upload-artifact@v4",
            "actions/download-artifact@v4",
            "scripts/crates_release.py plan",
            ".github/release/crates_publish.py",
            "--expect-digest",
            "CARGO_REGISTRY_TOKEN: ${{ steps.crates-auth.outputs.token }}",
            "cargo package -p tapid --locked",
            "cargo install tapid --version",
        )
        for contract in required_contracts:
            with self.subTest(contract=contract):
                self.assertIn(contract, workflow)

        preflight = workflow.split("  publish:", 1)[0]
        self.assertNotIn("id-token: write", preflight)
        self.assertNotIn("CARGO_REGISTRY_TOKEN", preflight)
        self.assertNotIn("environment: crates-io-release", preflight)
        self.assertNotIn("contents: write", workflow)
        self.assertIn("${{ runner.temp }}/crates-publication-preflight", workflow)
        self.assertIn("${{ runner.temp }}/crates-publication-mutation", workflow)
        self.assertNotIn("mkdir -p publication", workflow)
        self.assertNotIn("mkdir -p mutation", workflow)

    def test_crates_io_auth_action_is_pinned_to_reviewed_v1_0_5_commit(self):
        workflow = CRATES_WORKFLOW.read_text()
        auth_action_refs = re.findall(
            r"uses:\s+rust-lang/crates-io-auth-action@([^\s#]+)", workflow
        )

        self.assertEqual(
            auth_action_refs,
            ["c6f97d42243bad5fab37ca0427f495c86d5b1a18"],
        )
        self.assertIn(
            "uses: rust-lang/crates-io-auth-action@"
            "c6f97d42243bad5fab37ca0427f495c86d5b1a18 # v1.0.5",
            workflow,
        )
        self.assertTrue(
            all(re.fullmatch(r"[0-9a-f]{40}", ref) for ref in auth_action_refs)
        )


if __name__ == "__main__":
    unittest.main()
