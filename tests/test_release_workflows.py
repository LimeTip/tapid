import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CI_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
CRATES_WORKFLOW = ROOT / ".github" / "workflows" / "crates-publication.yml"


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

    def test_binary_publication_verifies_downloaded_draft_bytes_immediately_before_promotion(self):
        workflow = (
            ROOT / ".github" / "workflows" / "release-publication.yml"
        ).read_text()
        publication = workflow.split("      - name: Publish signed release assets", 1)[1]

        required_contracts = (
            'gh release download "$RELEASE_TAG" --dir downloaded-draft-assets',
            "verify_downloaded_release_assets(",
            "downloaded-draft-assets",
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
        self.assertNotIn("tag:", public_smoke_call)

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
            "rust-lang/crates-io-auth-action@v1",
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


if __name__ == "__main__":
    unittest.main()
