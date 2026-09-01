#!/usr/bin/env python3
"""Read-only command for producing reviewed crates.io publication plans."""

import argparse
import hashlib
import subprocess
import sys
import tempfile
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
RELEASE_TOOLS = REPOSITORY_ROOT / ".github" / "release"
if str(RELEASE_TOOLS) not in sys.path:
    sys.path.insert(0, str(RELEASE_TOOLS))

import crates_plan
import crates_repository
import release_identity


def _sha256_file(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def _exact_head(workspace, run):
    result = run(
        ["git", "rev-parse", "HEAD"],
        cwd=workspace,
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        raise ValueError("could not resolve exact HEAD commit")
    return result.stdout.strip()


def _require_clean_source(workspace, run):
    result = run(
        ["git", "status", "--porcelain"],
        cwd=workspace,
        text=True,
        capture_output=True,
    )
    if result.returncode != 0 or result.stdout.strip():
        raise ValueError("planning requires clean exact source")


def _parser():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    plan = subcommands.add_parser("plan", help="write a read-only publication plan")
    plan.add_argument("--commit", required=True)
    plan.add_argument("--output", type=Path, required=True)
    return parser


def main(
    argv=None,
    *,
    workspace=REPOSITORY_ROOT,
    run=subprocess.run,
    metadata_reader=crates_repository.read_locked_metadata,
    registry=None,
    package_adapter=None,
):
    """Build a plan from exact local source, public registry, and Cargo evidence."""
    arguments = _parser().parse_args(argv)
    commit = release_identity.validate_commit(arguments.commit)
    workspace = Path(workspace)
    if _exact_head(workspace, run) != commit:
        raise ValueError("--commit must match the exact HEAD commit")
    _require_clean_source(workspace, run)

    cargo_metadata = metadata_reader(workspace, run=run)
    registry = registry or crates_repository.CratesIoClient()
    with tempfile.TemporaryDirectory(prefix="tapid-crates-plan-") as temporary:
        if package_adapter is None:
            package_adapter = lambda name, version: crates_repository.package_crate(
                name,
                version,
                workspace,
                Path(temporary),
                run=run,
                cargo_home=Path(temporary) / "cargo-home",
            )
        observations = crates_repository.collect_package_evidence(
            cargo_metadata, registry, package_adapter
        )
        plan = crates_plan.build_publication_plan(
            cargo_metadata,
            observations,
            source_commit=commit,
            cargo_lock_sha256=_sha256_file(workspace / "Cargo.lock"),
            integration_lock_sha256=_sha256_file(
                workspace / "tests" / "integration" / "Cargo.lock"
            ),
        )

    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_bytes(release_identity.canonical_json(plan) + b"\n")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, crates_repository.RepositoryError,
            crates_repository.RegistryError) as error:
        print("error: {}".format(error), file=sys.stderr)
        raise SystemExit(1)
