#!/usr/bin/env python3
"""Create provider-neutral stable channel metadata without signing claims."""
import argparse
import json
import sys
from pathlib import Path
from urllib.parse import urlparse


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True)
    parser.add_argument("--endpoint", action="append", required=True)
    args = parser.parse_args()
    if not args.endpoint:
        parser.error("at least one manifest endpoint is required")
    for endpoint in args.endpoint:
        parsed = urlparse(endpoint)
        if parsed.scheme != "https" or not parsed.netloc:
            print(f"error: manifest endpoint must be an absolute HTTPS URL: {endpoint}", file=sys.stderr)
            return 2
        if not endpoint.endswith(".json"):
            print(f"error: manifest endpoint must name a JSON manifest: {endpoint}", file=sys.stderr)
            return 2
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps({"channel": "stable", "manifests": args.endpoint},
                                 ensure_ascii=False, separators=(",", ":"), sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
