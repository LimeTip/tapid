#!/usr/bin/env python3
"""Check Tapid's physical source file architecture guidance."""

import argparse
import subprocess
import sys
from pathlib import Path, PurePosixPath


REVIEW_RECOMMENDATION = 800
CLI_MAIN_PATH = "crates/tapid-cli/src/main.rs"
CLI_MAIN_THRESHOLD = 100
EXCEPTIONS_PATH = "docs/architecture-exceptions.txt"
MIN_RATIONALE_CHARS = 20
EXCLUDED_TEST_PARTS = frozenset({"test", "tests"})
EXCLUDED_TOP_LEVEL_TREES = frozenset({"target", "generated", "build"})
EXCLUDED_TEST_FILES = frozenset({"test.rs", "tests.rs"})


def tracked_files(root):
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return sorted(
        path.decode("utf-8")
        for path in result.stdout.split(b"\0")
        if path
    )


def is_production_rust(path):
    pure_path = PurePosixPath(path)
    return (
        pure_path.suffix == ".rs"
        and pure_path.name not in EXCLUDED_TEST_FILES
        and pure_path.parts[0] not in EXCLUDED_TOP_LEVEL_TREES
        and not any(part in EXCLUDED_TEST_PARTS for part in pure_path.parts)
    )


def read_exceptions(root, tracked):
    if EXCEPTIONS_PATH not in tracked:
        return {}, []

    exceptions = {}
    errors = []
    text = (root / EXCEPTIONS_PATH).read_text(encoding="utf-8")
    for line_number, raw_line in enumerate(text.splitlines(), start=1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        path, separator, rationale = line.partition("|")
        path = path.strip()
        rationale = rationale.strip()
        if not separator or not path or not rationale:
            errors.append(
                f"{EXCEPTIONS_PATH}: line {line_number} must contain a path and rationale"
            )
            continue
        if len(rationale) < MIN_RATIONALE_CHARS:
            errors.append(
                f"{EXCEPTIONS_PATH}: line {line_number} rationale must be at least "
                f"{MIN_RATIONALE_CHARS} characters"
            )
            continue
        if path in exceptions:
            errors.append(f"{EXCEPTIONS_PATH}: duplicate exception for {path}")
            continue
        exceptions[path] = rationale
    return exceptions, errors


def physical_line_count(path):
    with path.open("rb") as source_file:
        return sum(1 for _ in source_file)


def check(root):
    tracked = tracked_files(root)
    production_files = [path for path in tracked if is_production_rust(path)]
    line_counts = {
        path: physical_line_count(root / path) for path in production_files
    }
    exceptions, errors = read_exceptions(root, set(tracked))
    advisories = []

    for path in sorted(exceptions):
        if path not in production_files:
            errors.append(
                f"{EXCEPTIONS_PATH}: exception path is not a tracked production Rust file: {path}"
            )
            continue
        if path != CLI_MAIN_PATH:
            errors.append(
                f"{EXCEPTIONS_PATH}: exception path has no hard architecture threshold: {path}"
            )
            continue
        if line_counts[path] <= CLI_MAIN_THRESHOLD:
            errors.append(
                f"{EXCEPTIONS_PATH}: exception is stale at {line_counts[path]} physical lines: {path}"
            )

    for path in production_files:
        line_count = line_counts[path]
        if path == CLI_MAIN_PATH:
            if line_count > CLI_MAIN_THRESHOLD and path not in exceptions:
                errors.append(
                    f"{path}: {line_count} physical lines exceeds entrypoint threshold "
                    f"{CLI_MAIN_THRESHOLD}; keep main.rs to argument dispatch and exit conversion "
                    f"or document an exception in {EXCEPTIONS_PATH}"
                )
        elif line_count > REVIEW_RECOMMENDATION:
            advisories.append(
                f"{path}: {line_count} physical lines exceeds the "
                f"{REVIEW_RECOMMENDATION}-line review recommendation; review cohesion, "
                "module depth, and navigability before deciding whether to split it"
            )

    return production_files, sorted(advisories), sorted(errors)


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path.cwd(),
        help="repository root, default: current directory",
    )
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    root = args.root.resolve()
    try:
        production_files, advisories, errors = check(root)
    except (OSError, subprocess.CalledProcessError, UnicodeDecodeError) as error:
        print(f"Architecture check could not run: {error}")
        return 2

    if errors:
        print("Architecture check failed:")
        for error in errors:
            print(f"- {error}")
        return 1

    if advisories:
        print("Architecture review recommended:")
        for advisory in advisories:
            print(f"- {advisory}")

    noun = "file" if len(production_files) == 1 else "files"
    print(
        f"Architecture check passed: {len(production_files)} production Rust {noun} scanned"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
