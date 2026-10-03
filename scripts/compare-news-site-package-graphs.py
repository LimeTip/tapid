#!/usr/bin/env python3
"""Compare an npm reference install with the Tapid-managed news-site install."""

from __future__ import annotations

import argparse
import json
import platform
import sys
from pathlib import Path
from urllib.parse import urlsplit

Pair = tuple[str, str]
Edge = tuple[str, str, str, str, str, str, str]


def read_json(path: Path) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read JSON from {path}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"expected a JSON object in {path}")
    return value


def package_pair(manifest: dict) -> Pair | None:
    name, version = manifest.get("name"), manifest.get("version")
    if isinstance(name, str) and isinstance(version, str):
        return name, version
    return None


def installed_nodes(project: Path) -> dict[Pair, list[dict]]:
    node_modules = project / "node_modules"
    result: dict[Pair, list[dict]] = {}
    if not node_modules.is_dir():
        raise ValueError(f"missing installed node_modules tree: {node_modules}")
    for manifest_path in sorted(node_modules.rglob("package.json")):
        relative_path = manifest_path.relative_to(node_modules)
        parts = relative_path.parts
        if any(parts[index : index + 2] == ("dist", "compiled") for index in range(len(parts) - 1)):
            continue
        try:
            manifest = read_json(manifest_path)
        except ValueError:
            continue
        pair = package_pair(manifest)
        if pair is None:
            continue
        relative = relative_path.parent.as_posix()
        result.setdefault(pair, []).append(
            {"path": relative, "manifest": manifest, "manifestPath": manifest_path}
        )
    return result


def resolve_child(project: Path, parent: Path, name: str) -> Path | None:
    current = parent
    while current == project or project in current.parents:
        candidate = current / "node_modules" / name / "package.json"
        if candidate.is_file():
            return candidate
        if current == project:
            break
        current = current.parent
    return None


def root_edges(manifest: dict):
    for key, kind in (
        ("dependencies", "dependency"),
        ("devDependencies", "devDependency"),
        ("optionalDependencies", "optionalDependency"),
        ("peerDependencies", "peerDependency"),
    ):
        values = manifest.get(key, {}) or {}
        if isinstance(values, dict):
            for name, requirement in values.items():
                optional = kind == "optionalDependency" or (
                    kind == "peerDependency"
                    and (manifest.get("peerDependenciesMeta", {}).get(name, {}) or {}).get(
                        "optional", False
                    )
                )
                yield name, str(requirement), kind, optional


def package_edges(manifest: dict):
    for key, kind in (("dependencies", "dependency"), ("optionalDependencies", "optionalDependency")):
        values = manifest.get(key, {}) or {}
        if isinstance(values, dict):
            for name, requirement in values.items():
                yield name, str(requirement), kind, kind == "optionalDependency"


def peer_edges(manifest: dict):
    peers = manifest.get("peerDependencies", {}) or {}
    metadata = manifest.get("peerDependenciesMeta", {}) or {}
    if isinstance(peers, dict):
        for name, requirement in peers.items():
            optional = (metadata.get(name, {}) or {}).get("optional", False)
            yield name, str(requirement), bool(optional)


def reachable_graph(project: Path) -> dict:
    project = project.resolve()
    root_manifest = read_json(project / "package.json")
    root_pair = package_pair(root_manifest) or ("<root>", "")
    queue: list[tuple[Path, Pair, str, str, str, bool]] = [
        (project, root_pair, name, requirement, kind, optional)
        for name, requirement, kind, optional in root_edges(root_manifest)
    ]
    visited_edges: set[tuple[str, str, str, str]] = set()
    reachable: set[Pair] = set()
    dependency_edges: set[Edge] = set()
    peer_edges_seen: set[Edge] = set()
    missing_required: set[tuple[str, str, str]] = set()
    visited_packages: set[str] = set()

    while queue:
        parent_dir, parent_pair, name, requirement, kind, optional = queue.pop(0)
        edge_key = (str(parent_dir), name, kind, requirement)
        if edge_key in visited_edges:
            continue
        visited_edges.add(edge_key)
        child_manifest_path = resolve_child(project, parent_dir, name)
        child_pair: Pair | None = None
        child_manifest: dict | None = None
        child_dir = parent_dir
        if child_manifest_path is not None:
            child_manifest = read_json(child_manifest_path)
            child_pair = package_pair(child_manifest)
            child_dir = child_manifest_path.parent
            if child_pair is not None:
                reachable.add(child_pair)
                visited_packages.add(str(child_manifest_path))
        elif not optional:
            missing_required.add((parent_pair[0] + "@" + parent_pair[1], name, requirement))

        resolved_name, resolved_version = child_pair or ("<missing>", "")
        dependency_edges.add(
            (parent_pair[0], parent_pair[1], name, requirement, kind, resolved_name, resolved_version)
        )
        if child_manifest is None:
            continue

        current_pair = child_pair or ("<invalid>", "")
        for dep_name, dep_requirement, dep_kind, dep_optional in package_edges(child_manifest):
            queue.append(
                (child_dir, current_pair, dep_name, dep_requirement, dep_kind, dep_optional)
            )
        for peer_name, peer_requirement, peer_optional in peer_edges(child_manifest):
            provider_path = resolve_child(project, child_dir, peer_name)
            provider_pair = package_pair(read_json(provider_path)) if provider_path else None
            if provider_path and provider_pair:
                reachable.add(provider_pair)
                queue.append(
                    (child_dir, current_pair, peer_name, peer_requirement, "peerProvider", True)
                )
            elif not peer_optional:
                missing_required.add((f"{current_pair[0]}@{current_pair[1]}", peer_name, peer_requirement))
            provider_name, provider_version = provider_pair or ("<missing>", "")
            peer_edges_seen.add(
                (
                    current_pair[0],
                    current_pair[1],
                    peer_name,
                    peer_requirement,
                    "optional" if peer_optional else "required",
                    provider_name,
                    provider_version,
                )
            )

    return {
        "root": root_pair,
        "pairs": reachable,
        "dependencyEdges": dependency_edges,
        "peerEdges": peer_edges_seen,
        "missingRequired": missing_required,
        "visitedPackages": visited_packages,
    }


def registry_origin(value: str | None) -> str | None:
    if not value:
        return None
    parsed = urlsplit(value)
    if parsed.scheme and parsed.netloc:
        return f"{parsed.scheme.lower()}://{parsed.netloc.lower()}"
    return None


def npm_lock_records(project: Path, nodes: dict[Pair, list[dict]]) -> dict[Pair, list[dict]]:
    lock = read_json(project / "package-lock.json")
    if lock.get("lockfileVersion") != 3:
        raise ValueError(f"npm reference must use lockfileVersion 3, got {lock.get('lockfileVersion')!r}")
    records = lock.get("packages", {})
    result: dict[Pair, list[dict]] = {}
    for pair, instances in nodes.items():
        for instance in instances:
            key = "node_modules/" + instance["path"]
            record = records.get(key)
            if not isinstance(record, dict):
                continue
            result.setdefault(pair, []).append(record)
    return result


def tapid_lock_records(project: Path) -> dict[Pair, list[dict]]:
    lock = read_json(project / "tapid.lock")
    records = lock.get("packages", {})
    result: dict[Pair, list[dict]] = {}
    for record in records.values():
        if not isinstance(record, dict):
            continue
        pair = package_pair(record)
        if pair:
            result.setdefault(pair, []).append(record)
    return result


def sorted_pairs(values: set[Pair]) -> list[dict]:
    return [{"name": name, "version": version} for name, version in sorted(values)]


def edge_rows(edges: set[Edge]) -> list[dict]:
    return [
        {
            "parent": {"name": edge[0], "version": edge[1]},
            "dependency": edge[2],
            "range": edge[3],
            "kind": edge[4],
            "provider": None if edge[5] == "<missing>" else {"name": edge[5], "version": edge[6]},
        }
        for edge in sorted(edges)
    ]


def package_metadata(project: Path, nodes: dict[Pair, list[dict]], records: dict[Pair, list[dict]], *, npm: bool):
    result = {}
    for pair, instances in nodes.items():
        matching_records = records.get(pair, [])
        if npm:
            sources = {
                origin
                for record in matching_records
                if (origin := registry_origin(record.get("resolved"))) is not None
            }
            integrities = {record["integrity"] for record in matching_records if isinstance(record.get("integrity"), str)}
            platform = {
                (tuple(record.get("os", [])), tuple(record.get("cpu", [])), tuple(record.get("libc", [])))
                for record in matching_records
                if record.get("optional") is True
                and any(record.get(field) for field in ("os", "cpu", "libc"))
            }
        else:
            sources = {
                origin
                for record in matching_records
                if (origin := registry_origin(record.get("registry"))) is not None
            }
            integrities = {
                record["artifactIntegrity"]
                for record in matching_records
                if isinstance(record.get("artifactIntegrity"), str)
            }
            platform = {
                record.get("platformContext", "")
                for record in matching_records
                if record.get("platformContext") not in (None, "os=;cpu=;libc=")
            }
        result[pair] = {"sources": sources, "integrities": integrities, "platform": platform}
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--npm-root", required=True, type=Path, help="root of the npm reference install")
    parser.add_argument("--tapid-root", required=True, type=Path, help="root of the Tapid-managed install")
    parser.add_argument("--json", dest="json_path", type=Path, help="write deterministic JSON report")
    parser.add_argument("--text", dest="text_path", type=Path, help="write concise text report")
    args = parser.parse_args()

    npm_root, tapid_root = args.npm_root.resolve(), args.tapid_root.resolve()
    try:
        npm_nodes = installed_nodes(npm_root)
        tapid_nodes = installed_nodes(tapid_root)
        npm_graph = reachable_graph(npm_root)
        tapid_graph = reachable_graph(tapid_root)
        npm_records = npm_lock_records(npm_root, npm_nodes)
        tapid_records = tapid_lock_records(tapid_root)
    except ValueError as error:
        print(f"graph comparison error: {error}", file=sys.stderr)
        return 2

    npm_pairs, tapid_pairs = set(npm_nodes), set(tapid_nodes)
    npm_reachable, tapid_reachable = npm_graph["pairs"], tapid_graph["pairs"]
    npm_meta = package_metadata(npm_root, npm_nodes, npm_records, npm=True)
    tapid_meta = package_metadata(tapid_root, tapid_nodes, tapid_records, npm=False)
    shared_reachable = npm_reachable & tapid_reachable

    source_mismatches = []
    integrity_mismatches = []
    metadata_missing = []
    for pair in sorted(shared_reachable):
        baseline = npm_meta.get(pair, {"sources": set(), "integrities": set()})
        evaluated = tapid_meta.get(pair, {"sources": set(), "integrities": set()})
        if not baseline["sources"] or not evaluated["sources"]:
            metadata_missing.append({"name": pair[0], "version": pair[1], "field": "source"})
        elif not baseline["sources"].intersection(evaluated["sources"]):
            source_mismatches.append(
                {"name": pair[0], "version": pair[1], "npm": sorted(baseline["sources"]), "tapid": sorted(evaluated["sources"])}
            )
        if not baseline["integrities"] or not evaluated["integrities"]:
            metadata_missing.append({"name": pair[0], "version": pair[1], "field": "integrity"})
        elif not baseline["integrities"].intersection(evaluated["integrities"]):
            integrity_mismatches.append(
                {"name": pair[0], "version": pair[1], "npm": sorted(baseline["integrities"]), "tapid": sorted(evaluated["integrities"])}
            )

    platform_npm = {
        pair for pair in npm_reachable if npm_meta.get(pair, {}).get("platform")
    }
    platform_tapid = {
        pair for pair in tapid_reachable if tapid_meta.get(pair, {}).get("platform")
    }
    peer_npm, peer_tapid = npm_graph["peerEdges"], tapid_graph["peerEdges"]
    dep_npm, dep_tapid = npm_graph["dependencyEdges"], tapid_graph["dependencyEdges"]
    report = {
        "schemaVersion": 1,
        "baseline": "npm",
        "evaluated": "Tapid",
        "environment": {"system": platform.system(), "machine": platform.machine(), "libc": platform.libc_ver()[0]},
        "counts": {
            "npmPhysicalPackages": len(npm_pairs),
            "tapidPhysicalPackages": len(tapid_pairs),
            "npmReachablePackages": len(npm_reachable),
            "tapidReachablePackages": len(tapid_reachable),
            "npmLockRecords": len(read_json(npm_root / "package-lock.json").get("packages", {})) - 1,
            "tapidLockRecords": len(read_json(tapid_root / "tapid.lock").get("packages", {})),
        },
        "graphSnapshot": {
            "reachablePackages": {"npm": sorted_pairs(npm_reachable), "tapid": sorted_pairs(tapid_reachable)},
            "dependencyEdges": {"npm": edge_rows(dep_npm), "tapid": edge_rows(dep_tapid)},
            "peerEdges": {"npm": edge_rows(peer_npm), "tapid": edge_rows(peer_tapid)},
            "platformOptionalPackages": {
                "npm": sorted_pairs(platform_npm),
                "tapid": sorted_pairs(platform_tapid),
            },
            "provenance": [
                {
                    "name": name,
                    "version": version,
                    "npm": {
                        "registryOrigins": sorted(npm_meta.get((name, version), {}).get("sources", set())),
                        "integrities": sorted(npm_meta.get((name, version), {}).get("integrities", set())),
                    },
                    "tapid": {
                        "registryOrigins": sorted(tapid_meta.get((name, version), {}).get("sources", set())),
                        "integrities": sorted(tapid_meta.get((name, version), {}).get("integrities", set())),
                    },
                }
                for name, version in sorted(npm_pairs | tapid_pairs)
            ],
        },
        "reachablePackageDifferences": {"npmOnly": sorted_pairs(npm_reachable - tapid_reachable), "tapidOnly": sorted_pairs(tapid_reachable - npm_reachable)},
        "physicalPackageDifferences": {"npmOnly": sorted_pairs(npm_pairs - tapid_pairs), "tapidOnly": sorted_pairs(tapid_pairs - npm_pairs)},
        "unreachablePhysicalPackages": {
            "npm": sorted_pairs(npm_pairs - npm_reachable),
            "tapid": sorted_pairs(tapid_pairs - tapid_reachable),
        },
        "dependencyEdgeDifferences": {"npmOnly": edge_rows(dep_npm - dep_tapid), "tapidOnly": edge_rows(dep_tapid - dep_npm)},
        "peerEdgeDifferences": {"npmOnly": edge_rows(peer_npm - peer_tapid), "tapidOnly": edge_rows(peer_tapid - peer_npm)},
        "platformOptionalPackageDifferences": {
            "npmOnly": sorted_pairs(platform_npm - platform_tapid),
            "tapidOnly": sorted_pairs(platform_tapid - platform_npm),
        },
        "sourceIdentityMismatches": source_mismatches,
        "integrityMismatches": integrity_mismatches,
        "missingProvenance": metadata_missing,
        "missingRequiredEdges": {"npm": sorted(npm_graph["missingRequired"]), "tapid": sorted(tapid_graph["missingRequired"])},
    }

    lines = [
        "# News-site dependency graph comparison",
        "Baseline: npm; evaluated package manager: Tapid.",
        f"Reachable package identities: npm {len(npm_reachable)}, Tapid {len(tapid_reachable)}.",
        f"Physical installed identities: npm {len(npm_pairs)}, Tapid {len(tapid_pairs)}.",
        f"Reachable package differences: npm-only {len(npm_reachable - tapid_reachable)}, Tapid-only {len(tapid_reachable - npm_reachable)}.",
        f"Dependency-edge differences: npm-only {len(dep_npm - dep_tapid)}, Tapid-only {len(dep_tapid - dep_npm)}.",
        f"Peer-edge differences: npm-only {len(peer_npm - peer_tapid)}, Tapid-only {len(peer_tapid - peer_npm)}.",
        f"Platform-optional package differences: npm-only {len(platform_npm - platform_tapid)}, Tapid-only {len(platform_tapid - platform_npm)}.",
        "Shared platform-optional packages: " + (", ".join(f"{n}@{v}" for n, v in sorted(platform_npm & platform_tapid)) or "none"),
        f"Source mismatches: {len(source_mismatches)}; integrity mismatches: {len(integrity_mismatches)}; missing provenance fields: {len(metadata_missing)}.",
        "",
        "## Physical-only packages (reported even when unreachable)",
        "npm-only: " + (", ".join(f"{n}@{v}" for n, v in sorted(npm_pairs - tapid_pairs)) or "none"),
        "Tapid-only: " + (", ".join(f"{n}@{v}" for n, v in sorted(tapid_pairs - npm_pairs)) or "none"),
    ]
    text = "\n".join(lines) + "\n"
    encoded = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.json_path:
        args.json_path.parent.mkdir(parents=True, exist_ok=True)
        args.json_path.write_text(encoded, encoding="utf-8")
    else:
        print(encoded, end="")
    if args.text_path:
        args.text_path.parent.mkdir(parents=True, exist_ok=True)
        args.text_path.write_text(text, encoding="utf-8")
    print(text, end="", file=sys.stderr)

    has_mismatch = any(
        (
            npm_reachable != tapid_reachable,
            dep_npm != dep_tapid,
            peer_npm != peer_tapid,
            platform_npm != platform_tapid,
            source_mismatches,
            integrity_mismatches,
            metadata_missing,
            npm_graph["missingRequired"],
            tapid_graph["missingRequired"],
        )
    )
    return 1 if has_mismatch else 0


if __name__ == "__main__":
    raise SystemExit(main())
