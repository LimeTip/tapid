#!/usr/bin/env python3
"""Run Cargo with a build cache shared by this repository's worktrees."""

import os
from pathlib import Path
import subprocess
import sys


def cargo_environment(root, environment):
    env = dict(environment)
    if "CARGO_TARGET_DIR" not in env:
        result = subprocess.run(
            ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
            cwd=root, check=True, capture_output=True, text=True,
        )
        common_dir = Path(result.stdout.strip())
        # Worktrees have separate sources but share Git's common directory.
        # Bare and custom Git directories must retain their repository identity.
        cache_root = common_dir.parent if common_dir.name == ".git" else common_dir
        env["CARGO_TARGET_DIR"] = str(cache_root / "target" / "dev")
    return env


def main():
    if len(sys.argv) == 1:
        print("usage: python3 scripts/dev.py <cargo arguments>", file=sys.stderr)
        return 2
    root = Path(__file__).resolve().parents[1]
    try:
        return subprocess.run(
            ["cargo", *sys.argv[1:]], cwd=root,
            env=cargo_environment(root, os.environ),
        ).returncode
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"cannot run development command: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
