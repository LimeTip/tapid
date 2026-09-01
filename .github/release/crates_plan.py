"""Pure deterministic planning for Tapid crates.io publication."""

from collections import defaultdict
import re

import release_identity


SCHEMA = "tapid-crates-publication-plan-v1"
_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
_VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")
_SEMVER_CORE_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
_SEMVER_IDENTIFIER_RE = re.compile(r"^[0-9A-Za-z-]+$")


def _without_digest(plan):
    return {key: value for key, value in plan.items() if key != "plan_digest"}


def digest_publication_plan(plan):
    """Digest a publication plan without making the digest self-referential."""
    return release_identity.plan_digest(_without_digest(plan))


def _topological_order(names, edges):
    dependencies = {name: set(edges.get(name, ())) & set(names) for name in names}
    dependents = defaultdict(set)
    for name, required in dependencies.items():
        for dependency in required:
            dependents[dependency].add(name)
    ready = sorted(name for name, required in dependencies.items() if not required)
    result = []
    while ready:
        name = ready.pop(0)
        result.append(name)
        for dependent in sorted(dependents[name]):
            dependencies[dependent].discard(name)
            if not dependencies[dependent] and dependent not in result and dependent not in ready:
                ready.append(dependent)
                ready.sort()
    if len(result) != len(names):
        raise ValueError("workspace internal dependency graph contains a cycle")
    return result


def _version_tuple(version):
    match = _VERSION_RE.fullmatch(version)
    if not match:
        raise ValueError("crate versions must use explicit major.minor.patch form")
    return tuple(int(part) for part in match.groups())


def _requirement_accepts(requirement, version):
    """Evaluate the explicit Cargo requirements used by workspace path edges."""
    if not requirement or requirement == "*":
        raise ValueError("internal dependencies require an explicit version requirement")
    candidate = _version_tuple(version)
    clauses = [clause.strip() for clause in requirement.split(",")]
    if not clauses or any(not clause for clause in clauses):
        raise ValueError("internal dependencies require a valid Cargo version requirement")
    return all(_requirement_clause_accepts(clause, candidate) for clause in clauses)


def _requirement_clause_accepts(clause, candidate):
    match = re.fullmatch(r"(>=|<=|>|<|=|\^|~)?\s*([0-9xX*]+(?:\.[0-9xX*]+){0,2})", clause)
    if not match:
        raise ValueError(
            "internal dependencies require a supported explicit version requirement"
        )
    operator, text = match.groups()
    raw_parts = text.split(".")
    wildcard_at = next(
        (index for index, part in enumerate(raw_parts) if part in ("*", "x", "X")),
        None,
    )
    if wildcard_at is not None:
        if wildcard_at == 0 or any(
            part not in ("*", "x", "X") for part in raw_parts[wildcard_at:]
        ):
            raise ValueError("internal dependencies require an explicit version requirement")
        lower_parts = [int(part) for part in raw_parts[:wildcard_at]]
        lower = tuple((lower_parts + [0, 0, 0])[:3])
        upper_parts = lower_parts[:]
        upper_parts[-1] += 1
        upper = tuple((upper_parts + [0, 0, 0])[:3])
        if operator not in (None, "="):
            raise ValueError("wildcard requirements cannot use that comparator")
        return lower <= candidate < upper

    numeric = [int(part) for part in raw_parts]
    lower = tuple((numeric + [0, 0, 0])[:3])
    if operator == ">=":
        return candidate >= lower
    if operator == ">":
        return candidate > lower
    if operator == "<=":
        return candidate <= lower
    if operator == "<":
        return candidate < lower
    if operator == "=":
        return candidate == lower
    if operator == "~":
        upper = (
            (lower[0], lower[1] + 1, 0)
            if len(numeric) >= 2 else (lower[0] + 1, 0, 0)
        )
    else:  # Cargo's default requirement and explicit caret have caret semantics.
        if lower[0] > 0:
            upper = (lower[0] + 1, 0, 0)
        elif lower[1] > 0:
            upper = (0, lower[1] + 1, 0)
        elif lower[2] > 0:
            upper = (0, 0, lower[2] + 1)
        else:
            upper = (
                (1, 0, 0) if len(numeric) == 1
                else (0, 1, 0) if len(numeric) == 2
                else (0, 0, 1)
            )
    return lower <= candidate < upper


def _registry_version_is_newer(registry_version, local_version):
    registry_core, _prerelease = _parse_registry_semver(registry_version)
    local_core = _version_tuple(local_version)
    if registry_core != local_core:
        return registry_core > local_core
    # A prerelease of the same core version sorts before the stable local version.
    return False


def _parse_registry_semver(version):
    without_build, separator, build = version.partition("+")
    if separator and not _valid_identifiers(build, numeric_leading_zero_allowed=True):
        raise ValueError("crates.io returned a malformed package version")
    core, separator, prerelease = without_build.partition("-")
    match = _SEMVER_CORE_RE.fullmatch(core)
    if not match or (
        separator and not _valid_identifiers(prerelease, numeric_leading_zero_allowed=False)
    ):
        raise ValueError("crates.io returned a malformed package version")
    return tuple(int(part) for part in match.groups()), prerelease or None


def _valid_identifiers(value, *, numeric_leading_zero_allowed):
    return bool(value) and all(
        _SEMVER_IDENTIFIER_RE.fullmatch(identifier)
        and (
            numeric_leading_zero_allowed
            or not identifier.isdigit()
            or identifier == "0"
            or not identifier.startswith("0")
        )
        for identifier in value.split(".")
    )


def _is_crates_io_publishable(package):
    registries = package.get("publish")
    return registries is None or "crates-io" in registries


def build_publication_plan(
    metadata,
    observations,
    *,
    source_commit,
    cargo_lock_sha256,
    integration_lock_sha256,
):
    """Build a canonical, side-effect-free crates publication plan."""
    release_identity.validate_commit(source_commit)
    for label, digest in (
        ("Cargo.lock", cargo_lock_sha256),
        ("integration Cargo.lock", integration_lock_sha256),
    ):
        if not _SHA256_RE.fullmatch(digest):
            raise ValueError(f"{label} digest must be 64 lowercase hexadecimal characters")
    members = set(metadata["workspace_members"])
    all_workspace = {
        item["name"]: item
        for item in metadata["packages"]
        if item["id"] in members
    }
    workspace = {
        name: item for name, item in all_workspace.items()
        if _is_crates_io_publishable(item)
    }
    edges = {}
    for name, item in workspace.items():
        for dependency in item.get("dependencies", ()):
            if (
                dependency.get("path") is not None
                and dependency["name"] in all_workspace
                and dependency["name"] not in workspace
                and dependency.get("kind") in (None, "normal", "build")
            ):
                raise ValueError(
                    f"{name} depends on non-publishable workspace package {dependency['name']}"
                )
        internal = [
            dependency for dependency in item.get("dependencies", ())
            if dependency.get("path") is not None
            and dependency["name"] in workspace
            and dependency.get("kind") in (None, "normal", "build")
        ]
        for dependency in internal:
            _requirement_accepts(dependency.get("req"), workspace[dependency["name"]]["version"])
        edges[name] = sorted(dependency["name"] for dependency in internal)
    _topological_order(set(workspace), edges)

    entries = []
    proposed = set()
    for name in sorted(workspace):
        item = workspace[name]
        observation = observations[name]
        local_version = item["version"]
        published = [
            version for version in observation["registry_versions"]
            if version["version"] == local_version
        ]
        if len(published) > 1:
            raise ValueError(f"{name} {local_version} has ambiguous registry bytes")
        if any(
            _registry_version_is_newer(version["version"], local_version)
            for version in observation["registry_versions"]
        ):
            raise ValueError(
                f"{name} {local_version} has version drift behind crates.io"
            )
        if published and published[0]["checksum"] != observation["archive_sha256"]:
            raise ValueError(
                f"{name} {local_version} differs from immutable published bytes"
            )
        classification = (
            "unchanged" if published
            else "changed" if observation["registry_versions"]
            else "unpublished"
        )
        if classification in ("changed", "unpublished"):
            proposed.add(name)
        entries.append({
            "name": name,
            "version": local_version,
            "classification": classification,
            "action": "publish" if classification in ("changed", "unpublished") else "skip",
            "internal_dependencies": [
                {"name": dependency["name"], "requirement": dependency["req"]}
                for dependency in item.get("dependencies", ())
                if dependency.get("path") is not None
                and dependency["name"] in workspace
                and dependency.get("kind") in (None, "normal", "build")
            ],
            "archive_sha256": observation["archive_sha256"],
            "archive_size": observation["archive_size"],
            "expected_registry_checksum": observation["archive_sha256"],
            "observed_registry_checksum": published[0]["checksum"] if published else None,
        })

    required_updates = []
    for dependent_name in sorted(workspace):
        dependent = workspace[dependent_name]
        for dependency in dependent.get("dependencies", ()):
            dependency_name = dependency["name"]
            if (
                dependency.get("path") is not None
                and dependency_name in proposed
                and dependency_name in workspace
                and dependency.get("kind") in (None, "normal", "build")
                and not _requirement_accepts(
                    dependency.get("req"), workspace[dependency_name]["version"]
                )
            ):
                required_updates.append({
                    "dependent": dependent_name,
                    "dependency": dependency_name,
                    "current_requirement": dependency["req"],
                    "required_version": workspace[dependency_name]["version"],
                })
    errors = [
        "internal dependency requirements must accept every proposed local version"
    ] if required_updates else []

    plan = {
        "schema": SCHEMA,
        "source_commit": source_commit,
        "cargo_lock_sha256": cargo_lock_sha256,
        "integration_lock_sha256": integration_lock_sha256,
        "package_verification": "archives-hashed-without-registry-verification",
        "packages": entries,
        "publication_order": _topological_order(proposed, edges),
        "required_dependent_updates": required_updates,
        "preflight": {"ok": not errors, "errors": errors},
    }
    plan["plan_digest"] = digest_publication_plan(plan)
    return plan
