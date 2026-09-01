"""Pure helpers defining Tapid's canonical release identity."""

import hashlib
import json
import re
from datetime import timedelta, timezone


_VERSION_RE = re.compile(r"^0\.\d+\.\d+$")
_COMMIT_RE = re.compile(r"^[0-9a-f]{40,64}$")

TARGETS = (
    "aarch64-apple-darwin",
    "aarch64-pc-windows-msvc",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
)

# Signed release metadata has one fixed freshness window.
FRESHNESS_DURATION = timedelta(days=30)


def validate_version(version):
    """Validate and return a canonical Tapid zero-major version."""
    if not _VERSION_RE.fullmatch(version):
        raise ValueError("version must use canonical 0.x.y format")
    return version


def validate_version_tag(version, tag):
    """Validate and return a canonical Tapid version/tag pair."""
    validate_version(version)
    if tag != "v" + version:
        raise ValueError("version and tag must match canonical 0.x.y and v0.x.y formats")
    return version, tag


def validate_commit(commit):
    """Validate and return a full lowercase hexadecimal source commit."""
    if not _COMMIT_RE.fullmatch(commit):
        raise ValueError("commit must be 40 to 64 lowercase hexadecimal characters")
    return commit


def release_archives(version):
    """Return the immutable target/archive-name pairs in publication order."""
    validate_version(version)
    return tuple(
        (target, "tapid-{}-{}.tar.gz".format(version, target))
        for target in TARGETS
    )


def github_release_urls(repository, version, tag):
    """Derive immutable GitHub release download and manifest URLs."""
    validate_version_tag(version, tag)
    base_url = "https://github.com/{}/releases/download/{}".format(repository, tag)
    return {
        "base_url": base_url,
        "manifest_endpoint": base_url + "/release-manifest.json",
    }


def freshness_timestamps(clock):
    """Return second-precision UTC release freshness from an injected clock."""
    created = clock()
    if created.tzinfo is None or created.utcoffset() is None:
        raise ValueError("release clock must return a timezone-aware datetime")
    created = created.astimezone(timezone.utc).replace(microsecond=0)
    expires = created + FRESHNESS_DURATION
    return {
        "created_at": created.isoformat().replace("+00:00", "Z"),
        "expires_at": expires.isoformat().replace("+00:00", "Z"),
    }


def _canonical_text(value):
    if isinstance(value, dict):
        if any(not isinstance(key, str) for key in value):
            raise ValueError("canonical JSON object keys must be strings")
        items = sorted(value.items(), key=lambda item: item[0].encode("utf-16-be"))
        return "{" + ",".join(
            _canonical_text(key) + ":" + _canonical_text(item)
            for key, item in items
        ) + "}"
    if isinstance(value, list):
        return "[" + ",".join(_canonical_text(item) for item in value) + "]"
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if isinstance(value, bool) or value is None:
        return json.dumps(value, separators=(",", ":"))
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return str(value)
    raise ValueError("plan contains a value outside the supported canonical JSON subset")


def canonical_json(plan):
    """Serialize a plan with deterministic RFC 8785-compatible ordering."""
    return _canonical_text(plan).encode("utf-8")


def plan_digest(plan):
    """Return the canonical plan's prefixed SHA-256 digest."""
    return "sha256-" + hashlib.sha256(canonical_json(plan)).hexdigest()
