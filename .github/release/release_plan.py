"""Pure, fail-closed construction of guarded Tapid release plans."""

from datetime import datetime, timezone

import release_identity


SCHEMA = "tapid-release-plan-v1"
MAX_SNAPSHOT_AGE_SECONDS = 300


def _parse_timestamp(value, field):
    if not isinstance(value, str) or not value.endswith("Z"):
        raise ValueError("{} must be a canonical UTC timestamp".format(field))
    try:
        parsed = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise ValueError("{} must be a canonical UTC timestamp".format(field)) from error
    if parsed.strftime("%Y-%m-%dT%H:%M:%SZ") != value:
        raise ValueError("{} must be a canonical UTC timestamp".format(field))
    return parsed


def _require_fresh(snapshot, name, now):
    observed = _parse_timestamp(snapshot.get("observed_at"), name + ".observed_at")
    age = (now.astimezone(timezone.utc) - observed).total_seconds()
    if age < 0 or age > MAX_SNAPSHOT_AGE_SECONDS:
        raise ValueError("{} snapshot is stale".format(name))


def _classify_tag(tag, commit):
    state = tag.get("state")
    if state == "absent":
        return "create"
    if state != "present":
        raise ValueError("tag snapshot has an invalid state")
    if tag.get("object_type") != "tag":
        raise ValueError("release tag must be annotated")
    if tag.get("peeled_commit") != commit:
        raise ValueError("release tag does not peel to the exact commit")
    return "reuse-exact-annotated"


def _classify_release(release):
    state = release.get("state")
    if state not in ("absent", "draft", "public"):
        raise ValueError("GitHub release snapshot has an invalid state")
    return state


def build_release_plan(inputs, repository_snapshot, github_snapshot, now):
    """Convert immutable repository/GitHub observations into a reviewable plan."""
    repository = inputs.get("repository")
    if repository != "LimeTip/tapid":
        raise ValueError("repository must be LimeTip/tapid")
    version = release_identity.validate_version(inputs.get("version"))
    commit = release_identity.validate_commit(inputs.get("commit"))
    tag = "v" + version

    if not isinstance(now, datetime) or now.tzinfo is None or now.utcoffset() is None:
        raise ValueError("now must be a timezone-aware datetime")
    _require_fresh(repository_snapshot, "repository", now)
    _require_fresh(github_snapshot, "github", now)

    versions = repository_snapshot.get("workspace_versions")
    expected_note = "docs/releases/{}.md".format(version)
    lockfiles = repository_snapshot.get("lockfiles_present", ())
    checks = {
        "exact_main": (
            repository_snapshot.get("head") == commit
            and repository_snapshot.get("origin_main") == commit
            and repository_snapshot.get("commit_exists") is True
            and repository_snapshot.get("commit_is_ancestor_of_main") is True
        ),
        "cargo_version": isinstance(versions, dict) and versions.get("tapid") == version,
        "release_notes": (
            repository_snapshot.get("release_note_path") == expected_note
            and repository_snapshot.get("release_note_present") is True
        ),
        "locked_source": "Cargo.lock" in lockfiles,
    }

    tag_state = _classify_tag(repository_snapshot.get("tag", {}), commit)
    release_state = _classify_release(github_snapshot.get("release", {}))
    eligible = all(checks.values()) and release_state != "public"
    timestamps = release_identity.freshness_timestamps(lambda: now)
    urls = release_identity.github_release_urls(repository, version, tag)
    workflow_dispatch = {
        "version": version,
        "tag": tag,
        "commit": commit,
        "created_at": timestamps["created_at"],
        "expires_at": timestamps["expires_at"],
        "base_url": urls["base_url"],
        "manifest_endpoints": urls["manifest_endpoint"],
    }
    plan = {
        "schema": SCHEMA,
        "repository": repository,
        "version": version,
        "tag": tag,
        "commit": commit,
        "tag_state": tag_state,
        "release_state": release_state,
        "eligible": eligible,
        "checks": checks,
        "created_at": timestamps["created_at"],
        "expires_at": timestamps["expires_at"],
        "urls": urls,
        "workflow_dispatch": workflow_dispatch,
    }
    plan["plan_digest"] = release_identity.plan_digest(plan)
    return plan


def validate_plan(plan):
    """Validate a serialized release plan and its self-contained digest."""
    if not isinstance(plan, dict) or plan.get("schema") != SCHEMA:
        raise ValueError("unsupported release plan schema")
    expected_fields = {
        "schema",
        "repository",
        "version",
        "tag",
        "commit",
        "tag_state",
        "release_state",
        "eligible",
        "checks",
        "created_at",
        "expires_at",
        "urls",
        "workflow_dispatch",
        "plan_digest",
    }
    if set(plan) != expected_fields:
        raise ValueError("release plan fields do not match the schema")
    expected = plan.get("plan_digest")
    unsigned = dict(plan)
    unsigned.pop("plan_digest", None)
    if expected != release_identity.plan_digest(unsigned):
        raise ValueError("release plan digest does not match its contents")
    release_identity.validate_version_tag(plan.get("version"), plan.get("tag"))
    release_identity.validate_commit(plan.get("commit"))
    if plan.get("repository") != "LimeTip/tapid":
        raise ValueError("repository must be LimeTip/tapid")
    created_at = _parse_timestamp(plan.get("created_at"), "created_at")
    expires_at = _parse_timestamp(plan.get("expires_at"), "expires_at")
    if expires_at - created_at != release_identity.FRESHNESS_DURATION:
        raise ValueError("release plan freshness window is not canonical")
    urls = release_identity.github_release_urls(
        plan.get("repository"), plan.get("version"), plan.get("tag")
    )
    expected_dispatch = {
        "version": plan.get("version"),
        "tag": plan.get("tag"),
        "commit": plan.get("commit"),
        "created_at": plan.get("created_at"),
        "expires_at": plan.get("expires_at"),
        "base_url": urls["base_url"],
        "manifest_endpoints": urls["manifest_endpoint"],
    }
    if plan.get("urls") != urls or plan.get("workflow_dispatch") != expected_dispatch:
        raise ValueError("release plan workflow dispatch inputs are not canonical")
    if plan.get("release_state") == "public":
        raise ValueError("public releases cannot be mutated")
    expected_checks = {
        "exact_main": True,
        "cargo_version": True,
        "release_notes": True,
        "locked_source": True,
    }
    if plan.get("checks") != expected_checks or plan.get("eligible") is not True:
        raise ValueError("release plan preconditions are not eligible for mutation")
    if plan.get("tag_state") not in ("create", "reuse-exact-annotated"):
        raise ValueError("release plan has an unsafe tag state")
    return plan
