import hashlib
import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import crates_plan
import crates_repository


COMMIT = "a" * 40


def package(name, version, dependencies=(), publish=None):
    return {
        "id": f"path+file:///{name}#{version}",
        "name": name,
        "version": version,
        "publish": publish,
        "dependencies": [
            {
                "name": dependency,
                "req": requirement,
                "path": f"/{dependency}",
                "kind": None,
                "target": None,
                "optional": False,
            }
            for dependency, requirement in dependencies
        ],
    }


def metadata(*packages):
    return {
        "packages": list(packages),
        "workspace_members": [item["id"] for item in packages],
    }


def evidence(name, version, local_bytes, published_checksum=None):
    digest = hashlib.sha256(local_bytes).hexdigest()
    versions = [] if published_checksum is None else [
        {"version": version, "checksum": published_checksum}
    ]
    return {
        "archive_sha256": digest,
        "archive_size": len(local_bytes),
        "registry_versions": versions,
    }


class CratesPlanTests(unittest.TestCase):
    def test_unpublished_dependency_is_ordered_before_unpublished_dependent(self):
        workspace = metadata(
            package("app", "0.0.2", (("core", "^0.0.2"),)),
            package("core", "0.0.2"),
        )
        observations = {
            "app": evidence("app", "0.0.2", b"app"),
            "core": evidence("core", "0.0.2", b"core"),
        }

        plan = crates_plan.build_publication_plan(
            workspace,
            observations,
            source_commit=COMMIT,
            cargo_lock_sha256="1" * 64,
            integration_lock_sha256="2" * 64,
        )

        self.assertEqual(plan["schema"], "tapid-crates-publication-plan-v1")
        self.assertEqual(
            plan["package_verification"],
            "archives-hashed-without-registry-verification",
        )
        self.assertEqual(plan["publication_order"], ["core", "app"])
        self.assertEqual(
            [entry["classification"] for entry in plan["packages"]],
            ["unpublished", "unpublished"],
        )
        self.assertTrue(plan["preflight"]["ok"])
        self.assertEqual(
            plan["plan_digest"],
            crates_plan.digest_publication_plan(plan),
        )

    def test_classifies_identical_published_bytes_as_unchanged(self):
        archive = b"same immutable crate"
        checksum = hashlib.sha256(archive).hexdigest()
        plan = crates_plan.build_publication_plan(
            metadata(package("core", "0.0.1")),
            {"core": evidence("core", "0.0.1", archive, checksum)},
            source_commit=COMMIT,
            cargo_lock_sha256="1" * 64,
            integration_lock_sha256="2" * 64,
        )
        self.assertEqual(plan["packages"][0]["classification"], "unchanged")
        self.assertEqual(plan["packages"][0]["action"], "skip")
        self.assertEqual(plan["publication_order"], [])

    def test_rejects_local_bytes_that_drift_from_an_immutable_published_version(self):
        with self.assertRaisesRegex(ValueError, "immutable published bytes"):
            crates_plan.build_publication_plan(
                metadata(package("core", "0.0.1")),
                {"core": evidence("core", "0.0.1", b"local", "f" * 64)},
                source_commit=COMMIT,
                cargo_lock_sha256="1" * 64,
                integration_lock_sha256="2" * 64,
            )

    def test_rejects_ambiguous_registry_checksums_for_one_version(self):
        observation = evidence("core", "0.0.1", b"local")
        observation["registry_versions"] = [
            {"version": "0.0.1", "checksum": "1" * 64},
            {"version": "0.0.1", "checksum": "2" * 64},
        ]
        with self.assertRaisesRegex(ValueError, "ambiguous registry bytes"):
            crates_plan.build_publication_plan(
                metadata(package("core", "0.0.1")),
                {"core": observation},
                source_commit=COMMIT,
                cargo_lock_sha256="1" * 64,
                integration_lock_sha256="2" * 64,
            )

    def test_changed_foundation_requires_pre_one_dependent_version_update(self):
        workspace = metadata(
            package("app", "0.0.1", (("core", "^0.0.1"),)),
            package("core", "0.0.2"),
        )
        app_bytes = b"published app"
        app_checksum = hashlib.sha256(app_bytes).hexdigest()
        core_observation = evidence("core", "0.0.2", b"changed core")
        core_observation["registry_versions"] = [
            {"version": "0.0.1", "checksum": "3" * 64}
        ]

        plan = crates_plan.build_publication_plan(
            workspace,
            {
                "app": evidence("app", "0.0.1", app_bytes, app_checksum),
                "core": core_observation,
            },
            source_commit=COMMIT,
            cargo_lock_sha256="1" * 64,
            integration_lock_sha256="2" * 64,
        )

        self.assertEqual(plan["packages"][1]["classification"], "changed")
        self.assertEqual(plan["publication_order"], ["core"])
        self.assertEqual(plan["required_dependent_updates"], [{
            "dependent": "app",
            "dependency": "core",
            "current_requirement": "^0.0.1",
            "required_version": "0.0.2",
        }])
        self.assertFalse(plan["preflight"]["ok"])

    def test_build_dependency_is_published_before_its_dependent(self):
        app = package("app", "0.0.2", (("build-core", "^0.0.2"),))
        app["dependencies"][0]["kind"] = "build"
        workspace = metadata(app, package("build-core", "0.0.2"))
        observations = {
            "app": evidence("app", "0.0.2", b"app"),
            "build-core": evidence("build-core", "0.0.2", b"build-core"),
        }

        plan = crates_plan.build_publication_plan(
            workspace,
            observations,
            source_commit=COMMIT,
            cargo_lock_sha256="1" * 64,
            integration_lock_sha256="2" * 64,
        )

        self.assertEqual(plan["publication_order"], ["build-core", "app"])
        self.assertEqual(
            plan["packages"][0]["internal_dependencies"],
            [{"name": "build-core", "requirement": "^0.0.2"}],
        )

    def test_accepting_requirement_propagates_changed_dependent_into_release_set(self):
        workspace = metadata(
            package("app", "0.1.2", (("core", "^0.1.1"),)),
            package("core", "0.1.2"),
        )
        observations = {
            "app": evidence("app", "0.1.2", b"new app"),
            "core": evidence("core", "0.1.2", b"new core"),
        }
        for observation in observations.values():
            observation["registry_versions"] = [
                {"version": "0.1.1", "checksum": "4" * 64}
            ]
        plan = crates_plan.build_publication_plan(
            workspace,
            observations,
            source_commit=COMMIT,
            cargo_lock_sha256="1" * 64,
            integration_lock_sha256="2" * 64,
        )
        self.assertEqual(plan["publication_order"], ["core", "app"])
        self.assertEqual(plan["required_dependent_updates"], [])

    def test_publish_false_is_excluded_and_name_ties_are_stable(self):
        plan = crates_plan.build_publication_plan(
            metadata(
                package("zeta", "0.0.1"),
                package("private", "0.0.1", publish=[]),
                package("alternate", "0.0.1", publish=["private-registry"]),
                package("public", "0.0.1", publish=["crates-io"]),
                package("alpha", "0.0.1"),
            ),
            {
                "alpha": evidence("alpha", "0.0.1", b"a"),
                "public": evidence("public", "0.0.1", b"p"),
                "zeta": evidence("zeta", "0.0.1", b"z"),
            },
            source_commit=COMMIT,
            cargo_lock_sha256="1" * 64,
            integration_lock_sha256="2" * 64,
        )
        self.assertEqual(
            [item["name"] for item in plan["packages"]],
            ["alpha", "public", "zeta"],
        )
        self.assertEqual(plan["publication_order"], ["alpha", "public", "zeta"])

    def test_rejects_cycles_and_internal_dependencies_without_explicit_versions(self):
        with self.assertRaisesRegex(ValueError, "cycle"):
            crates_plan.build_publication_plan(
                metadata(
                    package("a", "0.0.1", (("b", "^0.0.1"),)),
                    package("b", "0.0.1", (("a", "^0.0.1"),)),
                ),
                {
                    "a": evidence("a", "0.0.1", b"a"),
                    "b": evidence("b", "0.0.1", b"b"),
                },
                source_commit=COMMIT,
                cargo_lock_sha256="1" * 64,
                integration_lock_sha256="2" * 64,
            )
        with self.assertRaisesRegex(ValueError, "explicit version requirement"):
            crates_plan.build_publication_plan(
                metadata(
                    package("a", "0.0.1", (("b", "*"),)),
                    package("b", "0.0.1"),
                ),
                {
                    "a": evidence("a", "0.0.1", b"a"),
                    "b": evidence("b", "0.0.1", b"b"),
                },
                source_commit=COMMIT,
                cargo_lock_sha256="1" * 64,
                integration_lock_sha256="2" * 64,
            )

    def test_rejects_a_local_version_behind_the_registry(self):
        observation = evidence("core", "0.0.1", b"old local")
        observation["registry_versions"] = [
            {"version": "0.0.2", "checksum": "5" * 64}
        ]
        with self.assertRaisesRegex(ValueError, "version drift"):
            crates_plan.build_publication_plan(
                metadata(package("core", "0.0.1")),
                {"core": observation},
                source_commit=COMMIT,
                cargo_lock_sha256="1" * 64,
                integration_lock_sha256="2" * 64,
            )

    def test_rejects_matching_local_version_when_registry_also_has_a_newer_version(self):
        archive = b"old local"
        observation = evidence(
            "core", "0.0.1", archive, hashlib.sha256(archive).hexdigest()
        )
        observation["registry_versions"].append(
            {"version": "0.0.2", "checksum": "5" * 64}
        )
        with self.assertRaisesRegex(ValueError, "version drift"):
            crates_plan.build_publication_plan(
                metadata(package("core", "0.0.1")),
                {"core": observation},
                source_commit=COMMIT,
                cargo_lock_sha256="1" * 64,
                integration_lock_sha256="2" * 64,
            )

    def test_accepts_cargo_tilde_wildcard_and_comparator_requirements(self):
        for requirement in ("^0", "~0.1.1", "0.1.*", ">=0.1.0, <0.2.0"):
            with self.subTest(requirement=requirement):
                plan = crates_plan.build_publication_plan(
                    metadata(
                        package("app", "0.1.2", (("core", requirement),)),
                        package("core", "0.1.2"),
                    ),
                    {
                        "app": evidence("app", "0.1.2", b"app"),
                        "core": evidence("core", "0.1.2", b"core"),
                    },
                    source_commit=COMMIT,
                    cargo_lock_sha256="1" * 64,
                    integration_lock_sha256="2" * 64,
                )
                self.assertTrue(plan["preflight"]["ok"])

    def test_rejects_higher_prerelease_version_drift_and_private_internal_edges(self):
        observation = evidence("core", "0.0.1", b"old local")
        observation["registry_versions"] = [
            {"version": "0.0.2-alpha.1", "checksum": "5" * 64}
        ]
        with self.assertRaisesRegex(ValueError, "version drift"):
            crates_plan.build_publication_plan(
                metadata(package("core", "0.0.1")),
                {"core": observation},
                source_commit=COMMIT,
                cargo_lock_sha256="1" * 64,
                integration_lock_sha256="2" * 64,
            )

        with self.assertRaisesRegex(ValueError, "non-publishable workspace package"):
            crates_plan.build_publication_plan(
                metadata(
                    package("app", "0.0.1", (("private", "^0.0.1"),)),
                    package("private", "0.0.1", publish=[]),
                ),
                {"app": evidence("app", "0.0.1", b"app")},
                source_commit=COMMIT,
                cargo_lock_sha256="1" * 64,
                integration_lock_sha256="2" * 64,
            )

    def test_rejects_malformed_registry_semver(self):
        for malformed in ("0.0.1-foo..bar", "01.0.0", "0.0.1-01"):
            observation = evidence("core", "0.0.2", b"core")
            observation["registry_versions"] = [
                {"version": malformed, "checksum": "5" * 64}
            ]
            with self.subTest(version=malformed):
                with self.assertRaisesRegex(ValueError, "malformed package version"):
                    crates_plan.build_publication_plan(
                        metadata(package("core", "0.0.2")),
                        {"core": observation},
                        source_commit=COMMIT,
                        cargo_lock_sha256="1" * 64,
                        integration_lock_sha256="2" * 64,
                    )

    def test_current_eighteen_crate_fixture_has_dependency_first_stable_order(self):
        definitions = (
            package("tapid", "0.0.7", (("tapid-core", "^0.0.4"), ("tapid-store", "^0.0.4"))),
            package("tapid-archive", "0.0.3"),
            package("tapid-attestations", "0.0.2", (("tapid-signatures", "^0.0.2"),)),
            package("tapid-core", "0.0.4"),
            package("tapid-linker", "0.0.4", (("tapid-core", "^0.0.4"), ("tapid-manifest", "^0.0.6"))),
            package("tapid-lockfile", "0.0.8", (("tapid-core", "^0.0.4"),)),
            package("tapid-manifest", "0.0.6", (("tapid-core", "^0.0.4"),)),
            package("tapid-policy", "0.0.2"),
            package("tapid-protocol", "0.0.3", (("tapid-core", "^0.0.4"),)),
            package("tapid-publish", "0.0.3", (("tapid-core", "^0.0.4"),)),
            package("tapid-registry-client", "0.0.4", (("tapid-core", "^0.0.4"),)),
            package("tapid-release-client", "0.0.1", (("tapid-signatures", "^0.0.2"),)),
            package("tapid-resolver", "0.0.4", (("tapid-core", "^0.0.4"), ("tapid-registry-client", "^0.0.4"))),
            package("tapid-runner", "0.0.2", (("tapid-policy", "^0.0.2"),)),
            package("tapid-signatures", "0.0.2"),
            package("tapid-store", "0.0.4", (("tapid-archive", "^0.0.3"), ("tapid-core", "^0.0.4"))),
            package("tapid-test-support", "0.0.1"),
            package("tapid-transparency", "0.0.2", (("tapid-signatures", "^0.0.2"),)),
        )
        observations = {
            item["name"]: evidence(item["name"], item["version"], item["name"].encode())
            for item in definitions
        }
        plan = crates_plan.build_publication_plan(
            metadata(*reversed(definitions)),
            observations,
            source_commit=COMMIT,
            cargo_lock_sha256="1" * 64,
            integration_lock_sha256="2" * 64,
        )
        order = plan["publication_order"]
        self.assertEqual(len(order), 18)
        self.assertLess(order.index("tapid-core"), order.index("tapid-store"))
        self.assertLess(order.index("tapid-store"), order.index("tapid"))
        self.assertLess(order.index("tapid-signatures"), order.index("tapid-attestations"))
        self.assertEqual(
            plan["plan_digest"], crates_plan.digest_publication_plan(plan)
        )


class CratesRepositoryTests(unittest.TestCase):
    def test_reads_locked_metadata_and_rejects_malformed_output(self):
        calls = []

        def run(command, **kwargs):
            calls.append((command, kwargs))
            return subprocess.CompletedProcess(command, 0, stdout='{"packages": [], "workspace_members": []}', stderr="")

        result = crates_repository.read_locked_metadata(Path("/repo"), run=run)
        self.assertEqual(result["packages"], [])
        self.assertEqual(calls[0][0], [
            "cargo", "metadata", "--locked", "--format-version", "1",
        ])
        self.assertEqual(calls[0][1]["cwd"], Path("/repo"))

        with self.assertRaisesRegex(crates_repository.RepositoryError, "malformed Cargo metadata"):
            crates_repository.read_locked_metadata(
                Path("/repo"),
                run=lambda *args, **kwargs: subprocess.CompletedProcess(args, 0, stdout="[]", stderr=""),
            )

    def test_registry_adapter_retries_transient_status_without_credentials(self):
        responses = [
            (503, b"busy", {}),
            (200, json.dumps({"versions": [{"num": "0.0.1", "checksum": "a" * 64}]}).encode(), {}),
        ]
        requests = []
        sleeps = []

        def http_get(url, headers, timeout):
            requests.append((url, headers, timeout))
            return responses.pop(0)

        client = crates_repository.CratesIoClient(
            http_get=http_get, sleep=sleeps.append, max_attempts=3
        )
        self.assertEqual(client.versions("tapid-core"), [
            {"version": "0.0.1", "checksum": "a" * 64}
        ])
        self.assertEqual(len(requests), 2)
        self.assertEqual(sleeps, [1.0])
        self.assertNotIn("Authorization", requests[0][1])
        self.assertIn("tapid-core", requests[0][0])

    def test_registry_adapter_distinguishes_not_found_rate_limit_auth_and_malformed(self):
        self.assertEqual(
            crates_repository.CratesIoClient(
                http_get=lambda *args: (404, b"not found", {}), max_attempts=1
            ).versions("missing"),
            [],
        )
        cases = (
            (429, b"limited", {"Retry-After": "2"}, "rate-limit"),
            (403, b"forbidden", {}, "authorization"),
            (200, b"not-json", {}, "malformed-response"),
            (
                200,
                json.dumps({"versions": [{"num": "01.0.0", "checksum": "a" * 64}]}).encode(),
                {},
                "malformed-response",
            ),
        )
        for status, body, headers, kind in cases:
            with self.subTest(kind=kind):
                with self.assertRaises(crates_repository.RegistryError) as raised:
                    crates_repository.CratesIoClient(
                        http_get=lambda *args, response=(status, body, headers): response,
                        sleep=lambda _: None,
                        max_attempts=1,
                    ).versions("tapid-core")
                self.assertEqual(raised.exception.kind, kind)

    def test_packages_in_isolated_target_and_hashes_archive_without_cargo_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            calls = []

            def run(command, **kwargs):
                calls.append((command, kwargs))
                target = Path(kwargs["env"]["CARGO_TARGET_DIR"])
                archive = target / "package" / "tapid-core-0.0.2.crate"
                archive.parent.mkdir(parents=True)
                archive.write_bytes(b"crate bytes")
                return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

            with mock.patch.dict(
                "os.environ",
                {
                    "CARGO_HOME": "/credential-bearing-home",
                    "CARGO_REGISTRY_TOKEN": "secret",
                    "CARGO_REGISTRIES_CRATES_IO_TOKEN": "secret",
                    "CARGO_REGISTRIES_PRIVATE_TOKEN": "secret",
                },
            ):
                packaged = crates_repository.package_crate(
                    "tapid-core", "0.0.2", root, root / "outputs", run=run
                )

            self.assertEqual(calls[0][0], [
                "cargo", "package", "--manifest-path", str(root.resolve() / "Cargo.toml"),
                "-p", "tapid-core", "--locked",
            ])
            self.assertNotEqual(calls[0][1]["cwd"], root)
            self.assertNotIn("CARGO_REGISTRY_TOKEN", calls[0][1]["env"])
            self.assertNotIn("CARGO_REGISTRIES_CRATES_IO_TOKEN", calls[0][1]["env"])
            self.assertNotIn("CARGO_REGISTRIES_PRIVATE_TOKEN", calls[0][1]["env"])
            self.assertNotEqual(calls[0][1]["env"]["CARGO_HOME"], "/credential-bearing-home")
            self.assertEqual(packaged["archive_size"], len(b"crate bytes"))
            self.assertEqual(
                packaged["archive_sha256"], hashlib.sha256(b"crate bytes").hexdigest()
            )
            self.assertEqual(Path(packaged["archive_path"]).parent, root / "outputs")

    def test_packages_workspace_archives_without_premature_registry_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            calls = []
            workspace_metadata = metadata(
                package("app", "0.0.2", (("core", "^0.0.2"),)),
                package("core", "0.0.2"),
            )

            def run(command, **kwargs):
                calls.append((command, kwargs))
                if command == ["cargo", "--version"]:
                    return subprocess.CompletedProcess(
                        command, 0, stdout="cargo 1.97.1 (c980f4866 2026-06-30)\n", stderr=""
                    )
                target = Path(kwargs["env"]["CARGO_TARGET_DIR"])
                package_dir = target / "package"
                package_dir.mkdir(parents=True)
                (package_dir / "app-0.0.2.crate").write_bytes(b"app archive")
                (package_dir / "core-0.0.2.crate").write_bytes(b"core archive")
                return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

            packaged = crates_repository.package_workspace(
                workspace_metadata,
                root,
                root / "outputs",
                run=run,
            )

        self.assertEqual(calls[0][0], ["cargo", "--version"])
        self.assertEqual(
            calls[1][0],
            [
                "cargo",
                "package",
                "--manifest-path",
                str(root.resolve() / "Cargo.toml"),
                "--workspace",
                "--locked",
                "--no-verify",
            ],
        )
        self.assertEqual(sorted(packaged), ["app", "core"])
        self.assertEqual(packaged["app"]["archive_size"], len(b"app archive"))
        self.assertEqual(
            packaged["core"]["archive_sha256"],
            hashlib.sha256(b"core archive").hexdigest(),
        )

    def test_workspace_packaging_requires_cargo_with_package_workspace_support(self):
        workspace_metadata = metadata(package("core", "0.0.2"))
        calls = []

        def run(command, **kwargs):
            calls.append(command)
            return subprocess.CompletedProcess(
                command, 0, stdout="cargo 1.88.0 (6b00bc388 2025-06-23)\n", stderr=""
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(
                crates_repository.RepositoryError, "Cargo 1.89 or newer"
            ):
                crates_repository.package_workspace(
                    workspace_metadata,
                    root,
                    root / "outputs",
                    run=run,
                )

        self.assertEqual(calls, [["cargo", "--version"]])

    def test_collects_registry_and_package_evidence_for_every_publishable_crate(self):
        workspace = metadata(
            package("zeta", "0.0.1"),
            package("private", "0.0.1", publish=[]),
            package("alternate", "0.0.1", publish=["private-registry"]),
            package("alpha", "0.0.2"),
        )
        queried = []
        packaged = []

        class Registry:
            def versions(self, name):
                queried.append(name)
                return [{"version": "0.0.1", "checksum": "a" * 64}]

        def package_adapter(name, version):
            packaged.append((name, version))
            return {"archive_sha256": "b" * 64, "archive_size": 12}

        observations = crates_repository.collect_package_evidence(
            workspace, Registry(), package_adapter
        )
        self.assertEqual(queried, ["alpha", "zeta"])
        self.assertEqual(packaged, [("alpha", "0.0.2"), ("zeta", "0.0.1")])
        self.assertEqual(sorted(observations), ["alpha", "zeta"])


class CratesReleaseCliTests(unittest.TestCase):
    def _load_script(self, name="crates_release"):
        script = Path(__file__).parents[2] / "scripts" / "crates_release.py"
        spec = importlib.util.spec_from_file_location(name, script)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_plan_command_writes_digest_bound_plan_for_exact_head_without_mutation(self):
        module = self._load_script()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.lock").write_bytes(b"root lock")
            (root / "tests" / "integration").mkdir(parents=True)
            (root / "tests" / "integration" / "Cargo.lock").write_bytes(b"nested lock")
            output = root / "crates-plan.json"
            commands = []

            def run(command, **kwargs):
                commands.append(command)
                if command == ["git", "rev-parse", "HEAD"]:
                    return subprocess.CompletedProcess(command, 0, stdout=COMMIT + "\n", stderr="")
                if command == ["git", "status", "--porcelain"]:
                    return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
                raise AssertionError(command)

            fixture = metadata(package("core", "0.0.2"))
            registry = mock.Mock()
            registry.versions.return_value = []
            exit_code = module.main(
                ["plan", "--commit", COMMIT, "--output", str(output)],
                workspace=root,
                run=run,
                metadata_reader=lambda workspace, run: fixture,
                registry=registry,
                package_adapter=lambda name, version: {
                    "archive_sha256": "b" * 64,
                    "archive_size": 12,
                },
            )

            self.assertEqual(exit_code, 0)
            written = json.loads(output.read_text())
            self.assertEqual(written["source_commit"], COMMIT)
            self.assertEqual(written["publication_order"], ["core"])
            self.assertEqual(written["plan_digest"], crates_plan.digest_publication_plan(written))
            self.assertEqual(commands, [
                ["git", "rev-parse", "HEAD"],
                ["git", "status", "--porcelain"],
            ])

    def test_plan_command_rejects_dirty_source_before_reading_metadata(self):
        module = self._load_script("crates_release_dirty")
        with tempfile.TemporaryDirectory() as directory:
            def run(command, **kwargs):
                output = COMMIT + "\n" if command[1] == "rev-parse" else " M Cargo.lock\n"
                return subprocess.CompletedProcess(command, 0, stdout=output, stderr="")

            with self.assertRaisesRegex(ValueError, "clean exact source"):
                module.main(
                    ["plan", "--commit", COMMIT, "--output", str(Path(directory) / "plan.json")],
                    workspace=Path(directory),
                    run=run,
                    metadata_reader=lambda workspace, run: self.fail("metadata must not be read"),
                    registry=mock.Mock(),
                    package_adapter=self.fail,
                )

    def test_plan_command_rejects_a_commit_other_than_exact_head(self):
        module = self._load_script("crates_release_mismatch")
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "exact HEAD"):
                module.main(
                    ["plan", "--commit", COMMIT, "--output", str(Path(directory) / "plan.json")],
                    workspace=Path(directory),
                    run=lambda command, **kwargs: subprocess.CompletedProcess(
                        command, 0, stdout="b" * 40 + "\n", stderr=""
                    ),
                    metadata_reader=lambda workspace, run: self.fail("metadata must not be read"),
                    registry=mock.Mock(),
                    package_adapter=self.fail,
                )


if __name__ == "__main__":
    unittest.main()
