"""Thin Git, Cargo, and GitHub adapters for guarded releases."""

import json
import subprocess
from datetime import timezone

import release_identity


def subprocess_runner(command):
    """Run one adapter command without a shell or inherited stdin."""
    return subprocess.run(command, capture_output=True, text=True, stdin=subprocess.DEVNULL)


def _run(runner, command, label, accepted=(0,)):
    result = runner(command)
    if result.returncode not in accepted:
        detail = (result.stderr or result.stdout).strip()
        raise ValueError("{} failed{}".format(label, ": " + detail if detail else ""))
    return result


def _canonical_observed_at(now):
    if now.tzinfo is None or now.utcoffset() is None:
        raise ValueError("snapshot time must be timezone-aware")
    return now.astimezone(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def _remote_tag_snapshot(runner, tag, commit):
    result = _run(
        runner,
        ["git", "ls-remote", "--tags", "origin", "refs/tags/{}".format(tag), "refs/tags/{}^{{}}".format(tag)],
        "remote tag lookup",
    )
    refs = {}
    for line in result.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) != 2:
            raise ValueError("remote tag lookup returned malformed output")
        object_id, ref = parts
        release_identity.validate_commit(object_id)
        if ref in refs:
            raise ValueError("remote tag lookup returned duplicate refs")
        refs[ref] = object_id
    direct = "refs/tags/{}".format(tag)
    peeled = direct + "^{}"
    if not refs:
        return {"state": "absent"}
    if direct not in refs or any(ref not in (direct, peeled) for ref in refs):
        raise ValueError("remote tag lookup returned malformed refs")
    if peeled in refs:
        return {"state": "present", "object_type": "tag", "peeled_commit": refs[peeled]}
    return {"state": "present", "object_type": "commit", "peeled_commit": refs[direct]}


def _github_release_snapshot(runner, repository, tag):
    result = runner([
        "gh", "release", "view", tag, "--repo", repository, "--json", "isDraft,tagName"
    ])
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        lowered = detail.lower()
        if "not found" in lowered or "404" in lowered:
            return {"state": "absent"}
        raise ValueError("GitHub release lookup failed{}".format(": " + detail if detail else ""))
    try:
        payload = json.loads(result.stdout)
    except (TypeError, json.JSONDecodeError) as error:
        raise ValueError("GitHub release lookup returned malformed JSON") from error
    if payload.get("tagName") != tag or type(payload.get("isDraft")) is not bool:
        raise ValueError("GitHub release lookup returned malformed release data")
    return {"state": "draft" if payload["isDraft"] else "public"}


def gather_snapshots(repository, version, commit, now, runner=subprocess_runner):
    """Gather only the immutable observations consumed by build_release_plan."""
    release_identity.validate_version(version)
    release_identity.validate_commit(commit)
    tag = "v" + version
    observed_at = _canonical_observed_at(now)

    status = _run(runner, ["git", "status", "--porcelain"], "worktree status")
    if status.stdout:
        raise ValueError("release planning requires a clean worktree")
    head = _run(runner, ["git", "rev-parse", "HEAD^{commit}"], "HEAD lookup").stdout.strip()
    release_identity.validate_commit(head)
    origin_main = _run(
        runner,
        ["git", "rev-parse", "refs/remotes/origin/main^{commit}"],
        "origin/main lookup",
    ).stdout.strip()
    release_identity.validate_commit(origin_main)
    remote_main_output = _run(
        runner,
        ["git", "ls-remote", "origin", "refs/heads/main"],
        "remote main lookup",
    ).stdout
    remote_main_lines = [line for line in remote_main_output.splitlines() if line]
    if len(remote_main_lines) != 1:
        raise ValueError("remote main lookup returned ambiguous state")
    remote_main_fields = remote_main_lines[0].split("\t")
    if len(remote_main_fields) != 2 or remote_main_fields[1] != "refs/heads/main":
        raise ValueError("remote main lookup returned malformed state")
    remote_main = remote_main_fields[0]
    release_identity.validate_commit(remote_main)
    if remote_main != origin_main:
        raise ValueError("origin/main does not match remote main")
    commit_result = runner(["git", "cat-file", "-e", commit + "^{commit}"])
    if commit_result.returncode != 0:
        raise ValueError("release commit does not exist")
    ancestor_result = runner([
        "git", "merge-base", "--is-ancestor", commit, "refs/remotes/origin/main"
    ])
    if ancestor_result.returncode not in (0, 1):
        raise ValueError("main ancestry lookup failed")

    metadata_result = _run(
        runner,
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        "Cargo metadata",
    )
    try:
        metadata = json.loads(metadata_result.stdout)
        packages = metadata["packages"]
        versions = {package["name"]: package["version"] for package in packages}
    except (TypeError, KeyError, json.JSONDecodeError) as error:
        raise ValueError("Cargo metadata returned malformed JSON") from error
    if not isinstance(packages, list) or len(versions) != len(packages) or "tapid" not in versions:
        raise ValueError("Cargo metadata returned malformed package data")

    note_path = "docs/releases/{}.md".format(version)
    note = runner(["git", "show", commit + ":" + note_path])
    note_present = note.returncode == 0 and bool(note.stdout.strip())
    lockfiles = []
    for path in ("Cargo.lock", "tests/integration/Cargo.lock"):
        result = runner(["git", "show", commit + ":" + path])
        if result.returncode == 0 and result.stdout:
            lockfiles.append(path)
        elif path == "Cargo.lock":
            raise ValueError("Cargo.lock is missing from the exact commit")

    repository_snapshot = {
        "observed_at": observed_at,
        "head": head,
        "origin_main": origin_main,
        "commit_exists": True,
        "commit_is_ancestor_of_main": ancestor_result.returncode == 0,
        "workspace_versions": versions,
        "release_note_path": note_path,
        "release_note_present": note_present,
        "lockfiles_present": lockfiles,
        "tag": _remote_tag_snapshot(runner, tag, commit),
    }
    github_snapshot = {
        "observed_at": observed_at,
        "release": _github_release_snapshot(runner, repository, tag),
    }
    return repository_snapshot, github_snapshot
