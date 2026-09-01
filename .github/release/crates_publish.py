"""Resumable execution for reviewed Tapid crates.io publication plans."""

import argparse
import copy
import hashlib
import itertools
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import crates_plan
import crates_repository


_PUBLISH_ENVIRONMENT_ALLOWLIST = {
    "HTTPS_PROXY", "HTTP_PROXY", "LANG", "NO_PROXY", "PATH", "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN", "SSL_CERT_FILE", "TMPDIR",
}
_MAX_RECOVERY_CANDIDATES = 18


class PublicationError(RuntimeError):
    """Publication stopped before the next package could be verified."""


def _atomic_write(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(value, output, sort_keys=True, separators=(",", ":"))
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _matching_version(versions, name, version, checksum):
    matches = [item for item in versions if item["version"] == version]
    if len(matches) > 1:
        raise PublicationError(f"registry returned ambiguous bytes for {name} {version}")
    if not matches:
        return False
    if matches[0]["checksum"] != checksum:
        raise PublicationError(f"registry checksum drift for {name} {version}")
    return True


def cargo_publish(name, version, workspace, token, *, run=subprocess.run):
    """Publish one package while limiting the credential to its process environment."""
    workspace = Path(workspace).resolve()
    environment = {
        key: value for key, value in os.environ.items()
        if key in _PUBLISH_ENVIRONMENT_ALLOWLIST
    }
    environment["CARGO_REGISTRY_TOKEN"] = token
    command = [
        "cargo", "publish", "--manifest-path", str(workspace / "Cargo.toml"),
        "-p", name, "--locked", "--no-verify",
    ]
    try:
        result = run(
            command,
            cwd=workspace,
            env=environment,
            text=True,
            capture_output=True,
            timeout=600,
        )
    except subprocess.TimeoutExpired as error:
        raise PublicationError(f"cargo publish timed out for {name} {version}") from error
    if result.returncode != 0:
        raise PublicationError(
            f"cargo publish rejected {name} {version} (exit code {result.returncode})"
        )


def verify_registry_package(name, version, workspace, *, run=subprocess.run):
    """Verify one package against registry-visible dependencies without credentials."""
    workspace = Path(workspace).resolve()
    with tempfile.TemporaryDirectory(prefix="cargo-registry-verify-") as isolated:
        environment = {
            key: value for key, value in os.environ.items()
            if key in _PUBLISH_ENVIRONMENT_ALLOWLIST
        }
        environment.update({
            "HOME": str(Path(isolated) / "home"),
            "CARGO_HOME": str(Path(isolated) / "cargo-home"),
            "CARGO_TARGET_DIR": str(Path(isolated) / "target"),
        })
        command = [
            "cargo", "package", "--manifest-path", str(workspace / "Cargo.toml"),
            "-p", name, "--locked",
        ]
        try:
            result = run(
                command,
                cwd=workspace,
                env=environment,
                text=True,
                capture_output=True,
                timeout=1200,
            )
        except subprocess.TimeoutExpired as error:
            raise PublicationError(
                f"registry-resolved package verification timed out for {name} {version}"
            ) from error
        if result.returncode != 0:
            raise PublicationError(
                f"registry-resolved package verification failed for {name} {version} "
                f"(exit code {result.returncode})"
            )


def package_workspace_archives(workspace, output_directory, plan, *, run=subprocess.run):
    """Build every workspace archive together using Cargo's workspace overlay."""
    workspace = Path(workspace).resolve()
    output_directory = Path(output_directory)
    output_directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix="cargo-workspace-package-", dir=output_directory
    ) as isolated:
        target = Path(isolated) / "target"
        cargo_home = Path(isolated) / "cargo-home"
        environment = {
            key: value for key, value in os.environ.items()
            if key in _PUBLISH_ENVIRONMENT_ALLOWLIST
        }
        environment.update({
            "HOME": str(Path(isolated) / "home"),
            "CARGO_HOME": str(cargo_home),
            "CARGO_TARGET_DIR": str(target),
        })
        command = [
            "cargo", "package", "--manifest-path", str(workspace / "Cargo.toml"),
            "--workspace", "--locked", "--no-verify",
        ]
        try:
            result = run(
                command,
                cwd=Path(isolated),
                env=environment,
                text=True,
                capture_output=True,
                timeout=1200,
            )
        except subprocess.TimeoutExpired as error:
            raise PublicationError("workspace archive packaging timed out") from error
        if result.returncode != 0:
            raise PublicationError(
                f"workspace archive packaging failed (exit code {result.returncode})"
            )

        archives = {}
        for entry in plan.get("packages", []):
            name = entry["name"]
            version = entry["version"]
            generated = target / "package" / f"{name}-{version}.crate"
            if not generated.is_file():
                raise PublicationError(f"workspace packaging omitted {name} {version}")
            destination = output_directory / generated.name
            shutil.copyfile(generated, destination)
            payload = destination.read_bytes()
            archives[name] = {
                "archive_path": str(destination),
                "archive_sha256": hashlib.sha256(payload).hexdigest(),
                "archive_size": len(payload),
            }
    return archives


def recover_reviewed_plan(current, expected_digest):
    """Recover digest-bound intent after an exact publication-order prefix landed."""
    if current.get("plan_digest") != crates_plan.digest_publication_plan(current):
        raise PublicationError("recomputed plan content does not match its digest")
    if current.get("plan_digest") == expected_digest:
        return copy.deepcopy(current)

    packages = current.get("packages", [])
    if not isinstance(packages, list):
        raise PublicationError("recomputed publication plan package shape is malformed")
    entries = {item.get("name"): item for item in packages if isinstance(item, dict)}
    if len(entries) != len(packages) or None in entries:
        raise PublicationError("recomputed publication plan package names must be unique")
    remaining = current.get("publication_order")
    if not isinstance(remaining, list):
        raise PublicationError("recomputed publication order is malformed")
    def recoverable(item):
        if item.get("action") != "skip" or item.get("classification") != "unchanged":
            return False
        byte_exact = item.get("observed_registry_checksum") == item.get("archive_sha256")
        archive_content = item.get("archive_content_sha256")
        content_exact = (
            item.get("observed_registry_checksum") == item.get("expected_registry_checksum")
            and isinstance(item.get("published_archive_size"), int)
            and item["published_archive_size"] > 0
            and isinstance(archive_content, str)
            and crates_plan._SHA256_RE.fullmatch(archive_content)
            and archive_content == item.get("published_content_sha256")
        )
        return byte_exact or content_exact

    eligible = sorted(name for name, item in entries.items() if recoverable(item))
    if len(eligible) > _MAX_RECOVERY_CANDIDATES:
        raise PublicationError("too many exact registry candidates to recover safely")
    edges = {
        name: [dependency.get("name") for dependency in item.get("internal_dependencies", [])]
        for name, item in entries.items()
    }
    for count in range(1, len(eligible) + 1):
        for completed_tuple in itertools.combinations(eligible, count):
            completed = set(completed_tuple)
            proposed = completed | set(remaining)
            candidate_order = crates_plan._topological_order(proposed, edges)
            if set(candidate_order[:count]) != completed or candidate_order[count:] != remaining:
                continue
            candidate = copy.deepcopy(current)
            candidate_entries = {item["name"]: item for item in candidate["packages"]}
            for name in completed:
                item = candidate_entries[name]
                if item.get("observed_registry_checksum") != item.get("archive_sha256"):
                    item["archive_sha256"] = item["observed_registry_checksum"]
                    item["archive_size"] = item["published_archive_size"]
                item["classification"] = (
                    "changed" if item.get("prior_registry_versions") else "unpublished"
                )
                item["action"] = "publish"
                item["expected_registry_checksum"] = item["archive_sha256"]
                item["observed_registry_checksum"] = None
                if "published_content_sha256" in item:
                    item["published_content_sha256"] = None
                if "published_archive_size" in item:
                    item["published_archive_size"] = None
            candidate["publication_order"] = candidate_order
            candidate["plan_digest"] = crates_plan.digest_publication_plan(candidate)
            if candidate["plan_digest"] == expected_digest:
                _validate_reviewed_shape(candidate)
                return candidate
    raise PublicationError(
        "recomputed plan digest differs from reviewed digest and is not an exact published prefix"
    )


def validate_recomputed_plan(reviewed, current, expected_digest):
    """Allow only exact-checksum publication of a verified dependency-order prefix."""
    if reviewed.get("plan_digest") != expected_digest:
        raise PublicationError("reviewed plan digest does not match workflow input")
    if crates_plan.digest_publication_plan(reviewed) != expected_digest:
        raise PublicationError("reviewed plan content does not match its digest")
    if current.get("plan_digest") != crates_plan.digest_publication_plan(current):
        raise PublicationError("recomputed plan content does not match its digest")
    for field in (
        "schema",
        "source_commit",
        "cargo_lock_sha256",
        "integration_lock_sha256",
        "package_verification",
        "required_dependent_updates",
    ):
        if current.get(field) != reviewed.get(field):
            raise PublicationError(f"recomputed plan {field} drift")
    if not reviewed.get("preflight", {}).get("ok") or not current.get("preflight", {}).get("ok"):
        raise PublicationError("publication plan preflight did not pass")

    reviewed_packages = {item["name"]: item for item in reviewed.get("packages", [])}
    current_packages = {item["name"]: item for item in current.get("packages", [])}
    if set(reviewed_packages) != set(current_packages):
        raise PublicationError("recomputed package set drift")
    immutable_fields = (
        "name", "version", "internal_dependencies", "archive_content_sha256",
        "prior_registry_versions",
        "expected_registry_checksum",
    )
    for name, original in reviewed_packages.items():
        observed = current_packages[name]
        if any(observed.get(field) != original.get(field) for field in immutable_fields):
            raise PublicationError(f"package archive drift for {name} {original.get('version')}")
        semantic_completed_prefix = (
            original.get("action") == "publish"
            and observed.get("action") == "skip"
            and observed.get("classification") == "unchanged"
            and original.get("archive_sha256") == original.get("expected_registry_checksum")
            and observed.get("observed_registry_checksum") == original.get("archive_sha256")
            and observed.get("published_archive_size") == original.get("archive_size")
            and isinstance(original.get("archive_content_sha256"), str)
            and crates_plan._SHA256_RE.fullmatch(original["archive_content_sha256"])
            and original["archive_content_sha256"] == observed.get("archive_content_sha256")
            == observed.get("published_content_sha256")
        )
        if not semantic_completed_prefix and (
            observed.get("archive_sha256") != original.get("archive_sha256")
            or observed.get("archive_size") != original.get("archive_size")
        ):
            raise PublicationError(f"package archive drift for {name} {original.get('version')}")

    completed = []
    remaining = []
    saw_remaining = False
    for name in reviewed.get("publication_order", []):
        observed = current_packages[name]
        expected_checksum = reviewed_packages[name]["expected_registry_checksum"]
        exact_skip = (
            observed.get("action") == "skip"
            and observed.get("classification") == "unchanged"
            and observed.get("observed_registry_checksum") == expected_checksum
        )
        if exact_skip:
            if saw_remaining:
                raise PublicationError("verified prior publications must form an exact prefix")
            completed.append(name)
        elif observed.get("action") == "publish" and observed.get("observed_registry_checksum") is None:
            saw_remaining = True
            remaining.append(name)
        else:
            raise PublicationError(f"registry drift for {name} is not safely resumable")
    if current.get("publication_order") != remaining:
        raise PublicationError("recomputed publication order is not the reviewed remaining prefix")
    return completed


def _validate_reviewed_shape(plan):
    packages = plan.get("packages")
    order = plan.get("publication_order")
    if not isinstance(packages, list) or not isinstance(order, list):
        raise PublicationError("publication plan package shape is malformed")
    names = [item.get("name") for item in packages if isinstance(item, dict)]
    if len(names) != len(packages) or len(set(names)) != len(names):
        raise PublicationError("publication plan package names must be unique")
    publish_names = {
        item["name"] for item in packages if item.get("action") == "publish"
    }
    if len(set(order)) != len(order) or set(order) != publish_names:
        raise PublicationError("publication order must contain every publish action exactly once")
    positions = {name: index for index, name in enumerate(order)}
    for item in packages:
        if item["name"] not in positions:
            continue
        for dependency in item.get("internal_dependencies", []):
            dependency_name = dependency.get("name")
            if dependency_name in positions and positions[dependency_name] >= positions[item["name"]]:
                raise PublicationError("publication dependency order is invalid")


def _packaged_archive_matches(entry, packaged, *, allow_semantic_metadata_drift):
    """Accept exact bytes, or normalized metadata drift for a checksum-verified prefix."""
    if not isinstance(packaged, dict):
        return False
    byte_exact = (
        packaged.get("archive_sha256") == entry.get("archive_sha256")
        and packaged.get("archive_size") == entry.get("archive_size")
    )
    if byte_exact:
        return True
    expected_content = entry.get("archive_content_sha256")
    if (
        not allow_semantic_metadata_drift
        or not isinstance(expected_content, str)
        or not crates_plan._SHA256_RE.fullmatch(expected_content)
    ):
        return False
    try:
        archive = Path(packaged["archive_path"]).read_bytes()
        return crates_repository.package_content_sha256(archive) == expected_content
    except (KeyError, OSError, TypeError, crates_repository.RepositoryError):
        return False


def execute_publication(
    plan,
    *,
    expected_digest,
    expected_commit,
    registry,
    package_adapter,
    publish_adapter,
    progress_path,
    verify_adapter=None,
    sleep=time.sleep,
    max_poll_attempts=5,
    dry_run=False,
):
    """Package, publish, and read back a reviewed plan in dependency order."""
    if plan.get("schema") != crates_plan.SCHEMA:
        raise PublicationError("unsupported publication plan schema")
    if plan.get("plan_digest") != expected_digest:
        raise PublicationError("reviewed plan digest does not match workflow input")
    if crates_plan.digest_publication_plan(plan) != expected_digest:
        raise PublicationError("publication plan content does not match its digest")
    if plan.get("source_commit") != expected_commit:
        raise PublicationError("publication source commit drift")
    if plan.get("package_verification") != "archives-hashed-without-registry-verification":
        raise PublicationError("plan must contain pre-mutation archive hash evidence")
    if not plan.get("preflight", {}).get("ok"):
        raise PublicationError("publication plan preflight did not pass")
    if not isinstance(max_poll_attempts, int) or not 1 <= max_poll_attempts <= 10:
        raise ValueError("max_poll_attempts must be between one and ten")
    _validate_reviewed_shape(plan)

    entries = {entry["name"]: entry for entry in plan["packages"]}
    progress = {
        "schema": "tapid-crates-publication-progress-v1",
        "source_commit": expected_commit,
        "plan_digest": expected_digest,
        "status": "dry-run" if dry_run else "publishing",
        "verified": [],
        "first_unverified": plan["publication_order"][0] if plan["publication_order"] else None,
    }
    _atomic_write(progress_path, progress)

    try:
        # Complete every read-only registry/package check before the first mutation.
        initial_registry = {}
        for name in plan["publication_order"]:
            entry = entries[name]
            version = entry["version"]
            checksum = entry["expected_registry_checksum"]
            initial_registry[name] = _matching_version(
                registry.versions(name), name, version, checksum
            )
            packaged = package_adapter(name, version)
            if not _packaged_archive_matches(
                entry,
                packaged,
                allow_semantic_metadata_drift=initial_registry[name],
            ):
                raise PublicationError(f"package archive drift for {name} {version}")

        saw_unpublished = False
        for name in plan["publication_order"]:
            if not initial_registry[name]:
                saw_unpublished = True
            elif saw_unpublished:
                raise PublicationError(
                    "verified prior publications must form an exact dependency-order prefix"
                )

        verification_required = not dry_run or any(initial_registry.values())
        if verification_required and verify_adapter is None:
            raise PublicationError("registry-resolved package verification is required")

        for index, name in enumerate(plan["publication_order"]):
            entry = entries[name]
            version = entry["version"]
            checksum = entry["expected_registry_checksum"]
            if initial_registry[name]:
                verify_adapter(name, version)
                state = "already-published"
            elif dry_run:
                state = "would-publish"
            else:
                publish_adapter(name, version)
                for attempt in range(max_poll_attempts):
                    if _matching_version(registry.versions(name), name, version, checksum):
                        break
                    if attempt + 1 < max_poll_attempts:
                        sleep(float(2 ** attempt))
                else:
                    raise PublicationError(
                        f"registry visibility timeout for {name} {version}; resume with the same commit and reviewed digest"
                    )
                verify_adapter(name, version)
                state = "published"
            progress["verified"].append({
                "name": name,
                "version": version,
                "checksum": checksum,
                "state": state,
            })
            progress["first_unverified"] = (
                plan["publication_order"][index + 1]
                if index + 1 < len(plan["publication_order"]) else None
            )
            _atomic_write(progress_path, progress)
    except Exception:
        progress["status"] = "failed"
        progress["resume"] = (
            "Resolve the failure, then rerun with the same commit and reviewed digest; "
            "already-visible exact checksums will not be republished."
        )
        _atomic_write(progress_path, progress)
        raise

    progress["status"] = "dry-run" if dry_run else "complete"
    _atomic_write(progress_path, progress)
    return progress


def _sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def _validate_local_source(plan, expected_commit, workspace, run):
    commands = (
        ["git", "rev-parse", "HEAD"],
        ["git", "status", "--porcelain", "--untracked-files=no"],
    )
    results = [
        run(command, cwd=workspace, text=True, capture_output=True)
        for command in commands
    ]
    if results[0].returncode != 0 or results[0].stdout.strip() != expected_commit:
        raise PublicationError("checked-out source commit drift")
    if results[1].returncode != 0 or results[1].stdout.strip():
        raise PublicationError("tracked source drift")
    for label, path, field in (
        ("Cargo.lock", workspace / "Cargo.lock", "cargo_lock_sha256"),
        (
            "integration Cargo.lock",
            workspace / "tests" / "integration" / "Cargo.lock",
            "integration_lock_sha256",
        ),
    ):
        if _sha256(path) != plan.get(field):
            raise PublicationError(f"{label} drift")


def _parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--expect-digest", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--progress", type=Path, required=True)
    parser.add_argument("--dry-run", action="store_true")
    return parser


def main(
    argv=None,
    *,
    workspace=None,
    run=subprocess.run,
    registry=None,
    package_adapter=None,
    publish_adapter=None,
    environ=None,
):
    """Validate exact local state and execute one reviewed publication plan."""
    arguments = _parser().parse_args(argv)
    workspace = Path(workspace or Path(__file__).resolve().parents[2]).resolve()
    try:
        plan = json.loads(arguments.plan.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PublicationError("publication plan is unreadable or malformed") from error
    if not isinstance(plan, dict):
        raise PublicationError("publication plan must be a JSON object")
    _validate_local_source(plan, arguments.commit, workspace, run)

    registry = registry or crates_repository.CratesIoClient()
    environment = os.environ if environ is None else environ
    token = environment.get("CARGO_REGISTRY_TOKEN")
    if not arguments.dry_run and plan.get("publication_order") and not token:
        raise PublicationError("trusted-publishing credential is unavailable")

    with tempfile.TemporaryDirectory(prefix="tapid-crates-publish-") as temporary:
        if package_adapter is None:
            archives = package_workspace_archives(
                workspace, Path(temporary) / "archives", plan, run=run
            )
            package_adapter = lambda name, version: archives[name]
        if publish_adapter is None:
            publish_adapter = lambda name, version: cargo_publish(
                name, version, workspace, token, run=run
            )
        execute_publication(
            plan,
            expected_digest=arguments.expect_digest,
            expected_commit=arguments.commit,
            registry=registry,
            package_adapter=package_adapter,
            publish_adapter=publish_adapter,
            verify_adapter=lambda name, version: verify_registry_package(
                name, version, workspace, run=run
            ),
            progress_path=arguments.progress,
            dry_run=arguments.dry_run,
        )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (
        OSError,
        PublicationError,
        crates_repository.RegistryError,
        crates_repository.RepositoryError,
    ) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
