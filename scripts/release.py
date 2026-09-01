#!/usr/bin/env python3
"""Plan and explicitly execute guarded Tapid release operations."""

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

RELEASE_MODULES = Path(__file__).resolve().parents[1] / ".github" / "release"
sys.path.insert(0, str(RELEASE_MODULES))

import release_identity
import release_plan
import release_repository


REPOSITORY = "LimeTip/tapid"


def _parser():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    plan = subcommands.add_parser("plan", help="build a read-only guarded release plan")
    plan.add_argument("--version", required=True)
    plan.add_argument("--commit", required=True)
    plan.add_argument("--output")
    for name in ("tag", "dispatch"):
        mutation = subcommands.add_parser(name, help="explicitly execute the reviewed {} step".format(name))
        mutation.add_argument("--plan", required=True)
        mutation.add_argument("--expect-digest", required=True)
    return parser


def _write_plan(plan, output):
    encoded = release_identity.canonical_json(plan).decode("utf-8") + "\n"
    if output:
        Path(output).write_text(encoded, encoding="utf-8")
    else:
        sys.stdout.write(encoded)


def _reject_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("release plan contains duplicate JSON keys")
        result[key] = value
    return result


def _read_reviewed_plan(path, expected_digest):
    try:
        plan = json.loads(Path(path).read_text(encoding="utf-8"), object_pairs_hook=_reject_duplicate_keys)
    except json.JSONDecodeError as error:
        raise ValueError("release plan is malformed JSON") from error
    release_plan.validate_plan(plan)
    if plan["plan_digest"] != expected_digest:
        raise ValueError("release plan does not match the expected digest")
    return plan


def _revalidate(plan, now, runner):
    if now.tzinfo is None or now.utcoffset() is None:
        raise ValueError("release clock must be timezone-aware")
    try:
        expires_at = datetime.fromisoformat(plan["expires_at"].replace("Z", "+00:00"))
    except (KeyError, ValueError) as error:
        raise ValueError("release plan expiry is malformed") from error
    if now.astimezone(timezone.utc) >= expires_at:
        raise ValueError("release plan has expired")
    repository, github = release_repository.gather_snapshots(
        plan["repository"], plan["version"], plan["commit"], now, runner
    )
    if repository["head"] != plan["commit"] or repository["origin_main"] != plan["commit"]:
        raise ValueError("release source changed after planning")
    if not repository["commit_is_ancestor_of_main"]:
        raise ValueError("release commit is no longer on origin/main")
    if repository["workspace_versions"].get("tapid") != plan["version"]:
        raise ValueError("tapid Cargo version changed after planning")
    if not repository["release_note_present"] or "Cargo.lock" not in repository["lockfiles_present"]:
        raise ValueError("release source checks changed after planning")
    if github["release"]["state"] == "public":
        raise ValueError("public releases are immutable")
    return repository, github


def _checked_run(runner, command, label, accepted=(0,)):
    result = runner(command)
    if result.returncode not in accepted:
        detail = (result.stderr or result.stdout).strip()
        raise ValueError("{} failed{}".format(label, ": " + detail if detail else ""))
    return result


def _tag(plan, repository, runner):
    tag = repository["tag"]
    if tag["state"] == "present":
        if tag.get("object_type") != "tag" or tag.get("peeled_commit") != plan["commit"]:
            raise ValueError("existing release tag is not exact and annotated")
        return
    if tag["state"] != "absent":
        raise ValueError("release tag state is malformed")
    ref = "refs/tags/{}".format(plan["tag"])
    local = runner(["git", "show-ref", "--verify", "--quiet", ref])
    if local.returncode == 0:
        raise ValueError("local release tag already exists while remote tag is absent")
    if local.returncode != 1:
        raise ValueError("local tag lookup failed")
    _checked_run(
        runner,
        ["git", "tag", "-a", plan["tag"], plan["commit"], "-m", "Tapid {}".format(plan["tag"])],
        "annotated tag creation",
    )
    _checked_run(
        runner,
        ["git", "push", "origin", "{}:{}".format(ref, ref)],
        "annotated tag push",
    )


def dispatch_command(plan):
    """Return the sole allowed workflow dispatch with all inputs explicit."""
    command = [
        "gh", "workflow", "run", ".github/workflows/release-publication.yml",
        "--repo", plan["repository"], "--ref", "main",
    ]
    for name in (
        "version", "tag", "commit", "created_at", "expires_at", "base_url", "manifest_endpoints"
    ):
        command.extend(["-f", "{}={}".format(name, plan["workflow_dispatch"][name])])
    return command


def _dispatch(plan, repository, runner):
    tag = repository["tag"]
    if (
        tag.get("state") != "present"
        or tag.get("object_type") != "tag"
        or tag.get("peeled_commit") != plan["commit"]
    ):
        raise ValueError("dispatch requires the exact annotated remote release tag")
    _checked_run(runner, dispatch_command(plan), "release workflow dispatch")


def main(argv=None, runner=release_repository.subprocess_runner, clock=None):
    args = _parser().parse_args(argv)
    now = (clock or (lambda: datetime.now(timezone.utc)))()
    if args.command == "plan":
        repository, github = release_repository.gather_snapshots(
            REPOSITORY, args.version, args.commit, now, runner
        )
        plan = release_plan.build_release_plan(
            {"repository": REPOSITORY, "version": args.version, "commit": args.commit},
            repository,
            github,
            now,
        )
        _write_plan(plan, args.output)
        return 0
    plan = _read_reviewed_plan(args.plan, args.expect_digest)
    repository, _github = _revalidate(plan, now, runner)
    if args.command == "tag":
        _tag(plan, repository, runner)
        return 0
    if args.command == "dispatch":
        _dispatch(plan, repository, runner)
        return 0
    raise AssertionError("unhandled release command")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError) as error:
        print("release: {}".format(error), file=sys.stderr)
        raise SystemExit(1)
