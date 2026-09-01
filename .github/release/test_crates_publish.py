import copy
import hashlib
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import crates_plan
import crates_publish
import release_identity


COMMIT = "a" * 40


def publication_plan(packages, order=None):
    entries = []
    for name, version, archive in packages:
        digest = hashlib.sha256(archive).hexdigest()
        entries.append({
            "name": name,
            "version": version,
            "classification": "unpublished",
            "action": "publish",
            "internal_dependencies": [],
            "archive_sha256": digest,
            "archive_size": len(archive),
            "expected_registry_checksum": digest,
            "observed_registry_checksum": None,
        })
    plan = {
        "schema": crates_plan.SCHEMA,
        "source_commit": COMMIT,
        "cargo_lock_sha256": "1" * 64,
        "integration_lock_sha256": "2" * 64,
        "packages": entries,
        "publication_order": order or [item[0] for item in packages],
        "required_dependent_updates": [],
        "package_verification": "archives-hashed-without-registry-verification",
        "preflight": {"ok": True, "errors": []},
    }
    plan["plan_digest"] = crates_plan.digest_publication_plan(plan)
    return plan


class FakeRegistry:
    def __init__(self, responses):
        self.responses = {name: list(values) for name, values in responses.items()}
        self.queries = []

    def versions(self, name):
        self.queries.append(name)
        values = self.responses[name]
        return values.pop(0) if len(values) > 1 else values[0]


class CratesPublishTests(unittest.TestCase):
    def test_publishes_one_reviewed_archive_then_verifies_registry_checksum(self):
        archive = b"reviewed archive"
        plan = publication_plan((("tapid-core", "0.0.2", archive),))
        checksum = plan["packages"][0]["archive_sha256"]
        registry = FakeRegistry({"tapid-core": [[], [{"version": "0.0.2", "checksum": checksum}]]})
        published = []

        with tempfile.TemporaryDirectory() as directory:
            progress = Path(directory) / "progress.json"
            result = crates_publish.execute_publication(
                plan,
                expected_digest=plan["plan_digest"],
                expected_commit=COMMIT,
                registry=registry,
                package_adapter=lambda name, version: {
                    "archive_path": str(Path(directory) / f"{name}-{version}.crate"),
                    "archive_sha256": checksum,
                    "archive_size": len(archive),
                },
                publish_adapter=lambda name, version: published.append((name, version)),
                verify_adapter=lambda name, version: None,
                progress_path=progress,
                sleep=lambda _: None,
                max_poll_attempts=2,
            )

            self.assertEqual(published, [("tapid-core", "0.0.2")])
            self.assertEqual(result["status"], "complete")
            self.assertEqual(result["verified"][0]["checksum"], checksum)
            self.assertEqual(json.loads(progress.read_text()), result)

    def test_rejects_digest_source_and_registry_drift_before_mutation(self):
        archive = b"reviewed archive"
        plan = publication_plan((("tapid-core", "0.0.2", archive),))
        adapters = {
            "registry": FakeRegistry({"tapid-core": [[{
                "version": "0.0.2", "checksum": "f" * 64,
            }]]}),
            "package_adapter": self.fail,
            "publish_adapter": self.fail,
            "progress_path": Path(tempfile.gettempdir()) / "unused-progress.json",
        }
        with self.assertRaisesRegex(crates_publish.PublicationError, "digest"):
            crates_publish.execute_publication(
                plan, expected_digest="f" * 64, expected_commit=COMMIT, **adapters
            )
        with self.assertRaisesRegex(crates_publish.PublicationError, "source commit"):
            crates_publish.execute_publication(
                plan, expected_digest=plan["plan_digest"], expected_commit="b" * 40,
                **adapters,
            )
        with self.assertRaisesRegex(crates_publish.PublicationError, "checksum drift"):
            crates_publish.execute_publication(
                plan, expected_digest=plan["plan_digest"], expected_commit=COMMIT,
                **adapters,
            )

    def test_packages_and_hashes_entire_remaining_order_before_first_mutation(self):
        packages = (("core", "0.0.2", b"core"), ("app", "0.0.2", b"app"))
        plan = publication_plan(packages)
        events = []
        observations = {}
        for name, version, archive in packages:
            checksum = hashlib.sha256(archive).hexdigest()
            observations[name] = [[], [{"version": version, "checksum": checksum}]]

        with tempfile.TemporaryDirectory() as directory:
            result = crates_publish.execute_publication(
                plan,
                expected_digest=plan["plan_digest"],
                expected_commit=COMMIT,
                registry=FakeRegistry(observations),
                package_adapter=lambda name, version: (
                    events.append(("package", name)) or {
                        "archive_path": str(Path(directory) / f"{name}-{version}.crate"),
                        "archive_sha256": next(
                            item["archive_sha256"] for item in plan["packages"]
                            if item["name"] == name
                        ),
                        "archive_size": next(
                            item["archive_size"] for item in plan["packages"]
                            if item["name"] == name
                        ),
                    }
                ),
                publish_adapter=lambda name, version: events.append(("publish", name)),
                verify_adapter=lambda name, version: events.append(("verify", name)),
                progress_path=Path(directory) / "progress.json",
                sleep=lambda _: None,
                max_poll_attempts=2,
            )

        self.assertEqual(events, [
            ("package", "core"), ("package", "app"),
            ("publish", "core"), ("verify", "core"),
            ("publish", "app"), ("verify", "app"),
        ])
        self.assertEqual(result["status"], "complete")

    def test_resumes_exact_verified_prefix_without_republishing_it(self):
        packages = (("core", "0.0.2", b"core"), ("app", "0.0.2", b"app"))
        plan = publication_plan(packages)
        core_checksum = plan["packages"][0]["archive_sha256"]
        app_checksum = plan["packages"][1]["archive_sha256"]
        registry = FakeRegistry({
            "core": [[{"version": "0.0.2", "checksum": core_checksum}]],
            "app": [[], [{"version": "0.0.2", "checksum": app_checksum}]],
        })
        published = []
        with tempfile.TemporaryDirectory() as directory:
            result = crates_publish.execute_publication(
                plan,
                expected_digest=plan["plan_digest"],
                expected_commit=COMMIT,
                registry=registry,
                package_adapter=lambda name, version: {
                    "archive_path": str(Path(directory) / f"{name}-{version}.crate"),
                    "archive_sha256": next(
                        item["archive_sha256"] for item in plan["packages"]
                        if item["name"] == name
                    ),
                    "archive_size": next(
                        item["archive_size"] for item in plan["packages"]
                        if item["name"] == name
                    ),
                },
                publish_adapter=lambda name, version: published.append(name),
                verify_adapter=lambda name, version: None,
                progress_path=Path(directory) / "progress.json",
                sleep=lambda _: None,
                max_poll_attempts=2,
            )
        self.assertEqual(published, ["app"])
        self.assertEqual(
            [item["state"] for item in result["verified"]],
            ["already-published", "published"],
        )

    def test_recovers_prefix_when_only_generated_archive_metadata_drifted(self):
        reviewed = publication_plan(
            (("core", "0.0.2", b"reviewed-core"), ("app", "0.0.2", b"app"))
        )
        core = reviewed["packages"][0]
        reviewed["packages"][1]["internal_dependencies"] = [
            {"name": "core", "requirement": "^0.0.2"}
        ]
        core["archive_content_sha256"] = "c" * 64
        core["published_content_sha256"] = None
        core["published_archive_size"] = None
        reviewed["plan_digest"] = crates_plan.digest_publication_plan(reviewed)

        current = copy.deepcopy(reviewed)
        current_core = current["packages"][0]
        current_core["classification"] = "unchanged"
        current_core["action"] = "skip"
        current_core["archive_sha256"] = "d" * 64
        current_core["archive_size"] = core["archive_size"] + 7
        current_core["observed_registry_checksum"] = core["archive_sha256"]
        current_core["published_content_sha256"] = core["archive_content_sha256"]
        current_core["published_archive_size"] = core["archive_size"]
        current["publication_order"] = ["app"]
        current["plan_digest"] = crates_plan.digest_publication_plan(current)

        recovered = crates_publish.recover_reviewed_plan(
            current, reviewed["plan_digest"]
        )
        self.assertEqual(recovered, reviewed)
        self.assertEqual(
            crates_publish.validate_recomputed_plan(
                recovered, current, reviewed["plan_digest"]
            ),
            ["core"],
        )

        missing_content = copy.deepcopy(current)
        missing_content["packages"][0]["archive_content_sha256"] = None
        missing_content["packages"][0]["published_content_sha256"] = None
        missing_content["plan_digest"] = crates_plan.digest_publication_plan(missing_content)
        with self.assertRaisesRegex(
            crates_publish.PublicationError, "not an exact published prefix"
        ):
            crates_publish.recover_reviewed_plan(
                missing_content, reviewed["plan_digest"]
            )

    def test_dry_run_packages_but_never_mutates_or_claims_registry_verification(self):
        plan = publication_plan((("core", "0.0.2", b"core"),))
        entry = plan["packages"][0]
        with tempfile.TemporaryDirectory() as directory:
            result = crates_publish.execute_publication(
                plan,
                expected_digest=plan["plan_digest"],
                expected_commit=COMMIT,
                registry=FakeRegistry({"core": [[]]}),
                package_adapter=lambda name, version: {
                    "archive_path": str(Path(directory) / "core.crate"),
                    "archive_sha256": entry["archive_sha256"],
                    "archive_size": entry["archive_size"],
                },
                publish_adapter=self.fail,
                progress_path=Path(directory) / "progress.json",
                dry_run=True,
            )
        self.assertEqual(result["status"], "dry-run")
        self.assertEqual(result["verified"][0]["state"], "would-publish")
    def test_timeout_and_publish_rejection_stop_before_any_dependent(self):
        packages = (("core", "0.0.2", b"core"), ("app", "0.0.2", b"app"))
        plan = publication_plan(packages)

        def package_adapter(name, version):
            entry = next(item for item in plan["packages"] if item["name"] == name)
            return {
                "archive_path": f"{name}-{version}.crate",
                "archive_sha256": entry["archive_sha256"],
                "archive_size": entry["archive_size"],
            }

        with tempfile.TemporaryDirectory() as directory:
            progress = Path(directory) / "progress.json"
            published = []
            with self.assertRaisesRegex(crates_publish.PublicationError, "visibility timeout"):
                crates_publish.execute_publication(
                    plan,
                    expected_digest=plan["plan_digest"],
                    expected_commit=COMMIT,
                    registry=FakeRegistry({"core": [[], [], []], "app": [[]]}),
                    package_adapter=package_adapter,
                    publish_adapter=lambda name, version: published.append(name),
                    verify_adapter=lambda name, version: None,
                    progress_path=progress,
                    sleep=lambda _: None,
                    max_poll_attempts=2,
                )
            self.assertEqual(published, ["core"])
            failed = json.loads(progress.read_text())
            self.assertEqual(failed["status"], "failed")
            self.assertEqual(failed["first_unverified"], "core")
            self.assertIn("same commit and reviewed digest", failed["resume"])

            def reject(name, version):
                raise crates_publish.PublicationError("cargo publish rejected core 0.0.2")

            with self.assertRaisesRegex(crates_publish.PublicationError, "rejected"):
                crates_publish.execute_publication(
                    plan,
                    expected_digest=plan["plan_digest"],
                    expected_commit=COMMIT,
                    registry=FakeRegistry({"core": [[]], "app": [[]]}),
                    package_adapter=package_adapter,
                    publish_adapter=reject,
                    verify_adapter=lambda name, version: None,
                    progress_path=progress,
                )
            self.assertEqual(json.loads(progress.read_text())["status"], "failed")

    def test_rate_limit_during_preflight_is_bounded_by_registry_and_never_mutates(self):
        plan = publication_plan((("core", "0.0.2", b"core"),))

        class RateLimitedRegistry:
            def versions(self, name):
                raise crates_publish.crates_repository.RegistryError(
                    "rate-limit", "crates.io request failed after bounded retries"
                )

        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(
                crates_publish.crates_repository.RegistryError, "bounded retries"
            ):
                crates_publish.execute_publication(
                    plan,
                    expected_digest=plan["plan_digest"],
                    expected_commit=COMMIT,
                    registry=RateLimitedRegistry(),
                    package_adapter=self.fail,
                    publish_adapter=self.fail,
                    progress_path=Path(directory) / "progress.json",
                )

    def test_cargo_publish_scopes_token_to_process_environment_not_argv(self):
        calls = []

        def run(command, **kwargs):
            calls.append((command, kwargs))
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        with mock.patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "ambient-secret", "UNSAFE": "drop"}):
            crates_publish.cargo_publish(
                "tapid-core", "0.0.2", Path("/repo"), "temporary-secret", run=run
            )

        command, kwargs = calls[0]
        self.assertEqual(command, [
            "cargo", "publish", "--manifest-path", "/repo/Cargo.toml",
            "-p", "tapid-core", "--locked",
        ])
        self.assertNotIn("temporary-secret", command)
        self.assertEqual(kwargs["env"]["CARGO_REGISTRY_TOKEN"], "temporary-secret")
        self.assertNotIn("UNSAFE", kwargs["env"])
        self.assertNotIn("ambient-secret", repr(command))
        self.assertEqual(kwargs["timeout"], 600)

        with self.assertRaisesRegex(crates_publish.PublicationError, "timed out"):
            crates_publish.cargo_publish(
                "tapid-core",
                "0.0.2",
                Path("/repo"),
                "temporary-secret",
                run=lambda command, **kwargs: (_ for _ in ()).throw(
                    subprocess.TimeoutExpired(command, kwargs["timeout"])
                ),
            )


    def test_requires_exact_archive_evidence_marker(self):
        plan = publication_plan((("core", "0.0.2", b"core"),))
        plan["package_verification"] = "normal-package-verification"
        plan["plan_digest"] = crates_plan.digest_publication_plan(plan)
        with self.assertRaisesRegex(crates_publish.PublicationError, "archive hash evidence"):
            crates_publish.execute_publication(
                plan,
                expected_digest=plan["plan_digest"],
                expected_commit=COMMIT,
                registry=self.fail,
                package_adapter=self.fail,
                publish_adapter=self.fail,
                progress_path=Path(tempfile.gettempdir()) / "unused-progress.json",
            )
    def test_cli_binds_clean_head_and_lockfiles_and_dry_run_needs_no_token(self):
        plan = publication_plan((("core", "0.0.2", b"core"),))
        entry = plan["packages"][0]
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            (workspace / "Cargo.lock").write_bytes(b"root lock")
            (workspace / "tests" / "integration").mkdir(parents=True)
            (workspace / "tests" / "integration" / "Cargo.lock").write_bytes(b"nested lock")
            plan["cargo_lock_sha256"] = hashlib.sha256(b"root lock").hexdigest()
            plan["integration_lock_sha256"] = hashlib.sha256(b"nested lock").hexdigest()
            plan["plan_digest"] = crates_plan.digest_publication_plan(plan)
            plan_path = workspace / "plan.json"
            plan_path.write_bytes(release_identity.canonical_json(plan) + b"\n")

            def run(command, **kwargs):
                if command == ["git", "rev-parse", "HEAD"]:
                    return subprocess.CompletedProcess(command, 0, stdout=COMMIT + "\n", stderr="")
                if command == ["git", "status", "--porcelain", "--untracked-files=no"]:
                    return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
                raise AssertionError(command)

            exit_code = crates_publish.main(
                [
                    "--plan", str(plan_path), "--expect-digest", plan["plan_digest"],
                    "--commit", COMMIT, "--progress", str(workspace / "progress.json"),
                    "--dry-run",
                ],
                workspace=workspace,
                run=run,
                registry=FakeRegistry({"core": [[]]}),
                package_adapter=lambda name, version: {
                    "archive_path": str(workspace / "core.crate"),
                    "archive_sha256": entry["archive_sha256"],
                    "archive_size": entry["archive_size"],
                },
                environ={},
            )
            self.assertEqual(exit_code, 0)

            (workspace / "Cargo.lock").write_bytes(b"drift")
            with self.assertRaisesRegex(crates_publish.PublicationError, "Cargo.lock drift"):
                crates_publish.main(
                    [
                        "--plan", str(plan_path), "--expect-digest", plan["plan_digest"],
                        "--commit", COMMIT, "--progress", str(workspace / "progress.json"),
                        "--dry-run",
                    ],
                    workspace=workspace,
                    run=run,
                    registry=FakeRegistry({"core": [[]]}),
                    package_adapter=self.fail,
                    environ={},
                )
    def test_recomputed_plan_accepts_only_exact_verified_prefix_resume(self):
        reviewed = publication_plan((
            ("core", "0.0.2", b"core"),
            ("middle", "0.0.2", b"middle"),
            ("app", "0.0.2", b"app"),
        ))
        current = copy.deepcopy(reviewed)
        core = current["packages"][0]
        core["classification"] = "unchanged"
        core["action"] = "skip"
        core["observed_registry_checksum"] = core["expected_registry_checksum"]
        current["publication_order"] = ["middle", "app"]
        current["plan_digest"] = crates_plan.digest_publication_plan(current)

        completed = crates_publish.validate_recomputed_plan(
            reviewed, current, reviewed["plan_digest"]
        )
        self.assertEqual(completed, ["core"])

        non_prefix = copy.deepcopy(current)
        non_prefix["packages"][0]["classification"] = "unpublished"
        non_prefix["packages"][0]["action"] = "publish"
        non_prefix["packages"][0]["observed_registry_checksum"] = None
        middle = non_prefix["packages"][1]
        middle["classification"] = "unchanged"
        middle["action"] = "skip"
        middle["observed_registry_checksum"] = middle["expected_registry_checksum"]
        non_prefix["publication_order"] = ["core", "app"]
        non_prefix["plan_digest"] = crates_plan.digest_publication_plan(non_prefix)
        with self.assertRaisesRegex(crates_publish.PublicationError, "prefix"):
            crates_publish.validate_recomputed_plan(
                reviewed, non_prefix, reviewed["plan_digest"]
            )

        drift = copy.deepcopy(current)
        drift["packages"][2]["archive_size"] += 1
        drift["plan_digest"] = crates_plan.digest_publication_plan(drift)
        with self.assertRaisesRegex(crates_publish.PublicationError, "package archive drift"):
            crates_publish.validate_recomputed_plan(
                reviewed, drift, reviewed["plan_digest"]
            )
    def test_recovers_reviewed_plan_from_exact_newly_published_prefix(self):
        reviewed = publication_plan((
            ("core", "0.0.2", b"core"),
            ("app", "0.0.2", b"app"),
        ))
        reviewed["packages"][1]["internal_dependencies"] = [
            {"name": "core", "requirement": "^0.0.2"}
        ]
        reviewed["plan_digest"] = crates_plan.digest_publication_plan(reviewed)
        current = copy.deepcopy(reviewed)
        core = current["packages"][0]
        core["classification"] = "unchanged"
        core["action"] = "skip"
        core["observed_registry_checksum"] = core["archive_sha256"]
        current["publication_order"] = ["app"]
        current["plan_digest"] = crates_plan.digest_publication_plan(current)

        recovered = crates_publish.recover_reviewed_plan(
            current, reviewed["plan_digest"]
        )

        self.assertEqual(recovered, reviewed)
        self.assertEqual(
            crates_publish.validate_recomputed_plan(
                recovered, current, reviewed["plan_digest"]
            ),
            ["core"],
        )

    def test_rejects_non_dependency_order_and_nonprefix_registry_state(self):
        plan = publication_plan((
            ("core", "0.0.2", b"core"),
            ("app", "0.0.2", b"app"),
        ), order=["app", "core"])
        plan["packages"][1]["internal_dependencies"] = [
            {"name": "core", "requirement": "^0.0.2"}
        ]
        plan["plan_digest"] = crates_plan.digest_publication_plan(plan)
        with self.assertRaisesRegex(crates_publish.PublicationError, "dependency order"):
            crates_publish.execute_publication(
                plan,
                expected_digest=plan["plan_digest"],
                expected_commit=COMMIT,
                registry=self.fail,
                package_adapter=self.fail,
                publish_adapter=self.fail,
                progress_path=Path(tempfile.gettempdir()) / "unused-progress.json",
            )

        valid = publication_plan((
            ("core", "0.0.2", b"core"),
            ("app", "0.0.2", b"app"),
        ))
        app_checksum = valid["packages"][1]["archive_sha256"]
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(crates_publish.PublicationError, "prefix"):
                crates_publish.execute_publication(
                    valid,
                    expected_digest=valid["plan_digest"],
                    expected_commit=COMMIT,
                    registry=FakeRegistry({
                        "core": [[]],
                        "app": [[{"version": "0.0.2", "checksum": app_checksum}]],
                    }),
                    package_adapter=lambda name, version: {
                        "archive_path": f"{name}.crate",
                        "archive_sha256": next(
                            item["archive_sha256"] for item in valid["packages"]
                            if item["name"] == name
                        ),
                        "archive_size": next(
                            item["archive_size"] for item in valid["packages"]
                            if item["name"] == name
                        ),
                    },
                    publish_adapter=self.fail,
                    progress_path=Path(directory) / "progress.json",
                )
    def test_workspace_package_overlay_hashes_all_archives_without_credentials(self):
        plan = publication_plan((
            ("core", "0.0.2", b"core"),
            ("app", "0.0.2", b"app"),
        ))
        calls = []
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory) / "repo"
            workspace.mkdir()
            output = Path(directory) / "output"

            def run(command, **kwargs):
                calls.append((command, kwargs))
                package_dir = Path(kwargs["env"]["CARGO_TARGET_DIR"]) / "package"
                package_dir.mkdir(parents=True)
                (package_dir / "core-0.0.2.crate").write_bytes(b"core")
                (package_dir / "app-0.0.2.crate").write_bytes(b"app")
                return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

            with mock.patch.dict(os.environ, {
                "CARGO_REGISTRY_TOKEN": "secret",
                "CARGO_REGISTRIES_CRATES_IO_TOKEN": "secret",
                "UNSAFE": "drop",
            }):
                archives = crates_publish.package_workspace_archives(
                    workspace, output, plan, run=run
                )

        command, kwargs = calls[0]
        self.assertEqual(command, [
            "cargo", "package", "--manifest-path", str(workspace.resolve() / "Cargo.toml"),
            "--workspace", "--locked", "--no-verify",
        ])
        self.assertNotEqual(kwargs["cwd"], workspace)
        self.assertNotIn("CARGO_REGISTRY_TOKEN", kwargs["env"])
        self.assertNotIn("CARGO_REGISTRIES_CRATES_IO_TOKEN", kwargs["env"])
        self.assertNotIn("UNSAFE", kwargs["env"])
        self.assertEqual(archives["core"]["archive_sha256"], hashlib.sha256(b"core").hexdigest())
        self.assertEqual(archives["app"]["archive_size"], len(b"app"))
    def test_registry_resolved_verification_precedes_each_dependent(self):
        plan = publication_plan((
            ("core", "0.0.2", b"core"),
            ("app", "0.0.2", b"app"),
        ))
        core_checksum = plan["packages"][0]["archive_sha256"]
        app_checksum = plan["packages"][1]["archive_sha256"]
        events = []
        with tempfile.TemporaryDirectory() as directory:
            crates_publish.execute_publication(
                plan,
                expected_digest=plan["plan_digest"],
                expected_commit=COMMIT,
                registry=FakeRegistry({
                    "core": [[{"version": "0.0.2", "checksum": core_checksum}]],
                    "app": [[], [{"version": "0.0.2", "checksum": app_checksum}]],
                }),
                package_adapter=lambda name, version: {
                    "archive_path": f"{name}.crate",
                    "archive_sha256": next(
                        item["archive_sha256"] for item in plan["packages"]
                        if item["name"] == name
                    ),
                    "archive_size": next(
                        item["archive_size"] for item in plan["packages"]
                        if item["name"] == name
                    ),
                },
                publish_adapter=lambda name, version: events.append(("publish", name)),
                verify_adapter=lambda name, version: events.append(("verify", name)),
                progress_path=Path(directory) / "progress.json",
                sleep=lambda _: None,
                max_poll_attempts=2,
            )
        self.assertEqual(events, [
            ("verify", "core"), ("publish", "app"), ("verify", "app")
        ])

    def test_registry_package_verifier_is_bounded_and_credential_free(self):
        calls = []

        def run(command, **kwargs):
            calls.append((command, kwargs))
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        with mock.patch.dict(os.environ, {
            "CARGO_REGISTRY_TOKEN": "secret",
            "CARGO_REGISTRIES_CRATES_IO_TOKEN": "secret",
            "UNSAFE": "drop",
        }):
            crates_publish.verify_registry_package(
                "tapid-core", "0.0.2", Path("/repo"), run=run
            )
        command, kwargs = calls[0]
        self.assertEqual(command, [
            "cargo", "package", "--manifest-path", "/repo/Cargo.toml",
            "-p", "tapid-core", "--locked",
        ])
        self.assertEqual(kwargs["timeout"], 1200)
        self.assertNotIn("CARGO_REGISTRY_TOKEN", kwargs["env"])
        self.assertNotIn("CARGO_REGISTRIES_CRATES_IO_TOKEN", kwargs["env"])
        self.assertNotIn("UNSAFE", kwargs["env"])


if __name__ == "__main__":
    unittest.main()
