#!/usr/bin/env python3
"""Validate the release artifact filename-to-target mapping without extraction."""
import argparse
import re
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--root", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    name_re = re.compile(rf"^tapid-{re.escape(args.version)}-([A-Za-z0-9._-]+)\.(tar\.gz|zip)$")
    files = sorted(path for path in Path(args.root).rglob("*") if path.is_file())
    if not files:
        raise SystemExit("no release artifacts found")
    mapping = {}
    for path in files:
        match = name_re.fullmatch(path.name)
        if not match:
            raise SystemExit(f"artifact filename is not a valid target mapping: {path.name}")
        target = match.group(1)
        if target in mapping:
            raise SystemExit(f"duplicate artifact target mapping: {target}")
        if path.stat().st_size < 1:
            raise SystemExit(f"artifact is empty: {path.name}")
        mapping[target] = path
    with Path(args.output).open("w") as output:
        for target, path in mapping.items():
            output.write(f"--artifact\n{target}={path}\n")


if __name__ == "__main__":
    main()
