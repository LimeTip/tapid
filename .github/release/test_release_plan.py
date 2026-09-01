import json
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))

import release as release_cli
import release_identity
import release_plan
import release_repository


COMMIT = "a" * 40
NOW = datetime(2026, 9, 1, 19, 15, 30, tzinfo=timezone.utc)


def valid_snapshots(tag=None, release=None):
    repository = {
        "observed_at": "2026-09-01T19:15:30Z",
        "head": COMMIT,
        "origin_main": COMMIT,
        "commit_exists": True,
        "commit_is_ancestor_of_main": True,
        "workspace_versions": {"tapid": "0.12.3"},
        "release_note_path": "docs/releases/0.12.3.md",
        "release_note_present": True,
        "lockfiles_present": ["Cargo.lock", "tests/integration/Cargo.lock"],
        "tag": tag or {"state": "absent"},
    }
    github = {
        "observed_at": "2026-09-01T19:15:30Z",
        "release": release or {"state": "absent"},
    }
    return repository, github


class FakeRunner:
    def __init__(self, responses):
        self.responses = responses
        self.calls = []

    def __call__(self, command):
        self.calls.append(command)
        key = tuple(command)
        if key not in self.responses:
            raise AssertionError("unexpected command: {}".format(command))
        response = self.responses[key]
        return SimpleNamespace(returncode=response[0], stdout=response[1], stderr=response[2])


def adapter_responses(release_code=1, release_stderr="release not found"):
    metadata = {"packages": [{"name": "tapid", "version": "0.12.3"}]}
    return {
        ("git", "status", "--porcelain"): (0, "", ""),
        ("git", "rev-parse", "HEAD^{commit}"): (0, COMMIT + "\n", ""),
        ("git", "rev-parse", "refs/remotes/origin/main^{commit}"): (0, COMMIT + "\n", ""),
        ("git", "ls-remote", "origin", "refs/heads/main"): (
            0,
            COMMIT + "\trefs/heads/main\n",
            "",
        ),
        ("git", "cat-file", "-e", COMMIT + "^{commit}"): (0, "", ""),
        ("git", "merge-base", "--is-ancestor", COMMIT, "refs/remotes/origin/main"): (0, "", ""),
        ("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"): (
            0,
            json.dumps(metadata),
            "",
        ),
        ("git", "show", COMMIT + ":docs/releases/0.12.3.md"): (0, "# notes\n", ""),
        ("git", "show", COMMIT + ":Cargo.lock"): (0, "lock\n", ""),
        ("git", "show", COMMIT + ":tests/integration/Cargo.lock"): (0, "lock\n", ""),
        ("git", "ls-remote", "--tags", "origin", "refs/tags/v0.12.3", "refs/tags/v0.12.3^{}"): (0, "", ""),
        ("gh", "release", "view", "v0.12.3", "--repo", "LimeTip/tapid", "--json", "isDraft,tagName"): (
            release_code,
            "",
            release_stderr,
        ),
    }


class ReleaseRepositoryTests(unittest.TestCase):
    def test_subprocess_runner_applies_a_bounded_timeout(self):
        with mock.patch.object(release_repository.subprocess, "run") as run:
            release_repository.subprocess_runner(
                ["git", "ls-remote", "origin", "refs/heads/main"]
            )

        timeout = run.call_args.kwargs.get("timeout")
        self.assertIsNotNone(timeout)
        self.assertGreater(timeout, 0)
        self.assertLessEqual(timeout, 300)

    def test_gathers_exact_commit_snapshot_without_mutation(self):
        runner = FakeRunner(adapter_responses())
        repository, github = release_repository.gather_snapshots(
            "LimeTip/tapid", "0.12.3", COMMIT, NOW, runner
        )

        self.assertEqual(repository["tag"], {"state": "absent"})
        self.assertEqual(github["release"], {"state": "absent"})
        self.assertEqual(repository["workspace_versions"], {"tapid": "0.12.3"})
        self.assertEqual(repository["head"], COMMIT)
        self.assertTrue(all(call[0:2] not in (("git", "tag"), ("git", "push")) for call in runner.calls))
        self.assertTrue(all(call[0:3] != ("gh", "workflow", "run") for call in runner.calls))

    def test_plan_command_writes_reviewable_plan_and_never_mutates(self):
        runner = FakeRunner(adapter_responses())
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "plan.json"
            result = release_cli.main(
                ["plan", "--version", "0.12.3", "--commit", COMMIT, "--output", str(output)],
                runner=runner,
                clock=lambda: NOW,
            )
            written = json.loads(output.read_text())

        self.assertEqual(result, 0)
        self.assertEqual(written["schema"], "tapid-release-plan-v1")
        self.assertTrue(all(call[0:2] not in (("git", "tag"), ("git", "push")) for call in runner.calls))
        self.assertTrue(all(call[0:3] != ("gh", "workflow", "run") for call in runner.calls))

    def test_adapters_fail_closed_on_malformed_or_unauthorized_output(self):
        cases = []
        responses = adapter_responses()
        responses[("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1")] = (0, "not-json", "")
        cases.append((responses, "Cargo metadata"))
        responses = adapter_responses(release_code=1, release_stderr="HTTP 403 forbidden")
        cases.append((responses, "GitHub release lookup failed"))
        responses = adapter_responses()
        responses[("git", "rev-parse", "refs/remotes/origin/main^{commit}")] = (128, "", "unknown revision")
        cases.append((responses, "origin/main"))
        responses = adapter_responses()
        responses[("git", "ls-remote", "origin", "refs/heads/main")] = (
            0,
            "b" * 40 + "\trefs/heads/main\n",
            "",
        )
        cases.append((responses, "remote main"))

        for responses, message in cases:
            with self.subTest(message=message):
                with self.assertRaisesRegex(ValueError, message):
                    release_repository.gather_snapshots(
                        "LimeTip/tapid", "0.12.3", COMMIT, NOW, FakeRunner(responses)
                    )


    def _write_plan_file(self, directory, tag=None, release=None):
        repository, github = valid_snapshots(tag=tag, release=release)
        plan = release_plan.build_release_plan(
            {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
            repository,
            github,
            NOW,
        )
        path = Path(directory) / "plan.json"
        path.write_bytes(release_identity.canonical_json(plan) + b"\n")
        return path, plan

    def test_tag_command_creates_and_pushes_only_an_absent_annotated_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            path, plan = self._write_plan_file(directory)
            responses = adapter_responses()
            responses[("git", "show-ref", "--verify", "--quiet", "refs/tags/v0.12.3")] = (1, "", "")
            responses[("git", "tag", "-a", "v0.12.3", COMMIT, "-m", "Tapid v0.12.3")] = (0, "", "")
            responses[("git", "push", "origin", "refs/tags/v0.12.3:refs/tags/v0.12.3")] = (0, "", "")
            runner = FakeRunner(responses)

            result = release_cli.main(
                ["tag", "--plan", str(path), "--expect-digest", plan["plan_digest"]],
                runner=runner,
                clock=lambda: NOW,
            )

        self.assertEqual(result, 0)
        self.assertIn(("git", "tag", "-a", "v0.12.3", COMMIT, "-m", "Tapid v0.12.3"), map(tuple, runner.calls))
        self.assertNotIn(("gh", "workflow", "run"), [tuple(call[:3]) for call in runner.calls])

    def test_tag_command_is_a_noop_for_the_exact_existing_annotated_tag(self):
        remote_tag = {"state": "present", "object_type": "tag", "peeled_commit": COMMIT}
        with tempfile.TemporaryDirectory() as directory:
            path, plan = self._write_plan_file(directory, tag=remote_tag, release={"state": "draft"})
            responses = adapter_responses(release_code=0, release_stderr="")
            responses[("git", "ls-remote", "--tags", "origin", "refs/tags/v0.12.3", "refs/tags/v0.12.3^{}")] = (
                0,
                "b" * 40 + "\trefs/tags/v0.12.3\n" + COMMIT + "\trefs/tags/v0.12.3^{}\n",
                "",
            )
            responses[("gh", "release", "view", "v0.12.3", "--repo", "LimeTip/tapid", "--json", "isDraft,tagName")] = (
                0,
                json.dumps({"isDraft": True, "tagName": "v0.12.3"}),
                "",
            )
            runner = FakeRunner(responses)
            result = release_cli.main(
                ["tag", "--plan", str(path), "--expect-digest", plan["plan_digest"]],
                runner=runner,
                clock=lambda: NOW,
            )
        self.assertEqual(result, 0)
        self.assertTrue(all(call[0:2] not in (("git", "tag"), ("git", "push")) for call in runner.calls))

    def test_tag_command_resumes_an_exact_local_tag_after_push_interruption(self):
        with tempfile.TemporaryDirectory() as directory:
            path, plan = self._write_plan_file(directory)
            responses = adapter_responses()
            ref = "refs/tags/v0.12.3"
            responses[("git", "show-ref", "--verify", "--quiet", ref)] = (0, "", "")
            responses[("git", "cat-file", "-t", ref)] = (0, "tag\n", "")
            responses[("git", "rev-parse", ref + "^{commit}")] = (0, COMMIT + "\n", "")
            responses[("git", "push", "origin", ref + ":" + ref)] = (0, "", "")
            runner = FakeRunner(responses)

            result = release_cli.main(
                ["tag", "--plan", str(path), "--expect-digest", plan["plan_digest"]],
                runner=runner,
                clock=lambda: NOW,
            )

        self.assertEqual(result, 0)
        self.assertNotIn(("git", "tag"), [tuple(call[:2]) for call in runner.calls])
        self.assertIn(("git", "push", "origin", ref + ":" + ref), map(tuple, runner.calls))

    def test_tag_command_never_recreates_a_tag_observed_during_planning(self):
        planned_tag = {"state": "present", "object_type": "tag", "peeled_commit": COMMIT}
        with tempfile.TemporaryDirectory() as directory:
            path, plan = self._write_plan_file(directory, tag=planned_tag)
            responses = adapter_responses()
            responses[("git", "show-ref", "--verify", "--quiet", "refs/tags/v0.12.3")] = (1, "", "")

            with self.assertRaisesRegex(ValueError, "disappeared"):
                release_cli.main(
                    ["tag", "--plan", str(path), "--expect-digest", plan["plan_digest"]],
                    runner=FakeRunner(responses),
                    clock=lambda: NOW,
                )

    def test_dispatch_passes_exactly_seven_reviewed_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            path, plan = self._write_plan_file(directory)
            responses = adapter_responses()
            responses[("git", "ls-remote", "--tags", "origin", "refs/tags/v0.12.3", "refs/tags/v0.12.3^{}")] = (
                0,
                "b" * 40 + "\trefs/tags/v0.12.3\n" + COMMIT + "\trefs/tags/v0.12.3^{}\n",
                "",
            )
            command = release_cli.dispatch_command(plan)
            responses[tuple(command)] = (0, "", "")
            runner = FakeRunner(responses)
            result = release_cli.main(
                ["dispatch", "--plan", str(path), "--expect-digest", plan["plan_digest"]],
                runner=runner,
                clock=lambda: NOW,
            )

        self.assertEqual(result, 0)
        self.assertEqual(runner.calls[-1], command)
        self.assertEqual(sum(1 for value in command if value == "-f"), 7)
        self.assertTrue(all(call[0:2] not in (("git", "tag"), ("git", "push")) for call in runner.calls))

    def test_mutations_reject_digest_mismatch_public_release_and_lightweight_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            path, plan = self._write_plan_file(directory)
            runner = FakeRunner({})
            with self.assertRaisesRegex(ValueError, "expected digest"):
                release_cli.main(
                    ["tag", "--plan", str(path), "--expect-digest", "sha256-" + "0" * 64],
                    runner=runner,
                    clock=lambda: NOW,
                )
            self.assertEqual(runner.calls, [])

            with self.assertRaisesRegex(ValueError, "expired"):
                release_cli.main(
                    ["dispatch", "--plan", str(path), "--expect-digest", plan["plan_digest"]],
                    runner=runner,
                    clock=lambda: NOW + timedelta(days=31),
                )
            self.assertEqual(runner.calls, [])

            responses = adapter_responses(release_code=0, release_stderr="")
            responses[("git", "ls-remote", "--tags", "origin", "refs/tags/v0.12.3", "refs/tags/v0.12.3^{}")] = (
                0,
                COMMIT + "\trefs/tags/v0.12.3\n",
                "",
            )
            responses[("gh", "release", "view", "v0.12.3", "--repo", "LimeTip/tapid", "--json", "isDraft,tagName")] = (
                0,
                json.dumps({"isDraft": False, "tagName": "v0.12.3"}),
                "",
            )
            with self.assertRaisesRegex(ValueError, "annotated|public"):
                release_cli.main(
                    ["dispatch", "--plan", str(path), "--expect-digest", plan["plan_digest"]],
                    runner=FakeRunner(responses),
                    clock=lambda: NOW,
                )


class ReleasePlanTests(unittest.TestCase):
    def test_builds_deterministic_absent_tag_plan_with_all_dispatch_inputs(self):
        repository, github = valid_snapshots()

        plan = release_plan.build_release_plan(
            {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
            repository,
            github,
            NOW,
        )

        self.assertEqual(plan["schema"], "tapid-release-plan-v1")
        self.assertEqual(plan["tag"], "v0.12.3")
        self.assertEqual(plan["tag_state"], "create")
        self.assertEqual(plan["release_state"], "absent")
        self.assertEqual(
            plan["workflow_dispatch"],
            {
                "version": "0.12.3",
                "tag": "v0.12.3",
                "commit": COMMIT,
                "created_at": "2026-09-01T19:15:30Z",
                "expires_at": "2026-10-01T19:15:30Z",
                "base_url": "https://github.com/LimeTip/tapid/releases/download/v0.12.3",
                "manifest_endpoints": "https://github.com/LimeTip/tapid/releases/download/v0.12.3/release-manifest.json",
            },
        )
        self.assertTrue(plan["plan_digest"].startswith("sha256-"))
        self.assertEqual(release_plan.validate_plan(plan), plan)

    def test_reuses_only_an_exact_annotated_tag_for_a_draft_rerun(self):
        repository, github = valid_snapshots(
            tag={
                "state": "present",
                "object_type": "tag",
                "peeled_commit": COMMIT,
            },
            release={"state": "draft"},
        )
        plan = release_plan.build_release_plan(
            {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
            repository,
            github,
            NOW,
        )
        self.assertEqual(plan["tag_state"], "reuse-exact-annotated")
        self.assertEqual(plan["release_state"], "draft")

    def test_classifies_repository_and_public_release_disagreement_as_ineligible(self):
        cases = []
        repository, github = valid_snapshots()
        repository["origin_main"] = "b" * 40
        cases.append((repository, github, "exact_main"))
        repository, github = valid_snapshots()
        repository["workspace_versions"]["tapid"] = "0.12.2"
        cases.append((repository, github, "cargo_version"))
        repository, github = valid_snapshots()
        repository["release_note_present"] = False
        cases.append((repository, github, "release_notes"))
        repository, github = valid_snapshots(release={"state": "public"})
        cases.append((repository, github, "release_state"))

        for repository, github, failed_check in cases:
            with self.subTest(failed_check=failed_check):
                plan = release_plan.build_release_plan(
                    {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
                    repository,
                    github,
                    NOW,
                )
                self.assertFalse(plan["eligible"])
                if failed_check == "release_state":
                    self.assertEqual(plan["release_state"], "public")
                else:
                    self.assertFalse(plan["checks"][failed_check])
                with self.assertRaises(ValueError):
                    release_plan.validate_plan(plan)

    def test_rejects_unsafe_tag_states_and_stale_snapshots(self):
        cases = []
        repository, github = valid_snapshots(
            tag={"state": "present", "object_type": "commit", "peeled_commit": COMMIT}
        )
        cases.append((repository, github, "annotated"))
        repository, github = valid_snapshots(
            tag={"state": "present", "object_type": "tag", "peeled_commit": "b" * 40}
        )
        cases.append((repository, github, "exact commit"))
        repository, github = valid_snapshots()
        repository["observed_at"] = "2026-09-01T19:00:00Z"
        cases.append((repository, github, "stale"))

        for repository, github, message in cases:
            with self.subTest(message=message):
                with self.assertRaisesRegex(ValueError, message):
                    release_plan.build_release_plan(
                        {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
                        repository,
                        github,
                        NOW,
                    )

    def test_plan_validation_rejects_a_rehashed_noncanonical_dispatch_input(self):
        repository, github = valid_snapshots()
        plan = release_plan.build_release_plan(
            {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
            repository,
            github,
            NOW,
        )
        plan["workflow_dispatch"]["base_url"] = "https://attacker.invalid"
        unsigned = dict(plan)
        unsigned.pop("plan_digest")
        plan["plan_digest"] = release_plan.release_identity.plan_digest(unsigned)

        with self.assertRaisesRegex(ValueError, "workflow dispatch inputs"):
            release_plan.validate_plan(plan)

    def test_plan_validation_rejects_a_rehashed_noncanonical_repository(self):
        repository, github = valid_snapshots()
        plan = release_plan.build_release_plan(
            {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
            repository,
            github,
            NOW,
        )
        plan["repository"] = "attacker/tapid"
        urls = release_identity.github_release_urls(
            plan["repository"], plan["version"], plan["tag"]
        )
        plan["urls"] = urls
        plan["workflow_dispatch"]["base_url"] = urls["base_url"]
        plan["workflow_dispatch"]["manifest_endpoints"] = urls["manifest_endpoint"]
        unsigned = dict(plan)
        unsigned.pop("plan_digest")
        plan["plan_digest"] = release_identity.plan_digest(unsigned)

        with self.assertRaisesRegex(ValueError, "repository must be LimeTip/tapid"):
            release_plan.validate_plan(plan)

    def test_plan_validation_rejects_rehashed_noncanonical_freshness(self):
        repository, github = valid_snapshots()
        plan = release_plan.build_release_plan(
            {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
            repository,
            github,
            NOW,
        )
        plan["expires_at"] = "2027-09-01T19:15:30Z"
        plan["workflow_dispatch"]["expires_at"] = plan["expires_at"]
        unsigned = dict(plan)
        unsigned.pop("plan_digest")
        plan["plan_digest"] = release_identity.plan_digest(unsigned)

        with self.assertRaisesRegex(ValueError, "freshness"):
            release_plan.validate_plan(plan)

    def test_plan_validation_rejects_rehashed_unknown_fields(self):
        repository, github = valid_snapshots()
        plan = release_plan.build_release_plan(
            {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
            repository,
            github,
            NOW,
        )
        plan["unexpected"] = "ignored"
        unsigned = dict(plan)
        unsigned.pop("plan_digest")
        plan["plan_digest"] = release_identity.plan_digest(unsigned)

        with self.assertRaisesRegex(ValueError, "fields"):
            release_plan.validate_plan(plan)

    def test_plan_validation_rejects_rehashed_fractional_timestamps(self):
        repository, github = valid_snapshots()
        plan = release_plan.build_release_plan(
            {"repository": "LimeTip/tapid", "version": "0.12.3", "commit": COMMIT},
            repository,
            github,
            NOW,
        )
        plan["created_at"] = "2026-09-01T19:15:30.000Z"
        plan["expires_at"] = "2026-10-01T19:15:30.000Z"
        plan["workflow_dispatch"]["created_at"] = plan["created_at"]
        plan["workflow_dispatch"]["expires_at"] = plan["expires_at"]
        unsigned = dict(plan)
        unsigned.pop("plan_digest")
        plan["plan_digest"] = release_identity.plan_digest(unsigned)

        with self.assertRaisesRegex(ValueError, "canonical"):
            release_plan.validate_plan(plan)


if __name__ == "__main__":
    unittest.main()
