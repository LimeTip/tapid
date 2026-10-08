"""Offline contracts for the shared npm/Tapid news-site resolution inputs."""

import base64
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / "examples" / "news-site-consumer"
COMPARATOR = ROOT / "scripts" / "compare-news-site-package-graphs.py"


def assert_pinned_graph(test, manifest, lock):
    """Require exact native-resolution inputs, not npm-lock import behavior."""
    test.assertEqual(lock["lockfileVersion"], 3)
    records = lock["packages"]
    overrides = manifest.get("overrides", {})
    for name, version in overrides.items():
        test.assertEqual(version, records["node_modules/" + name]["version"], name)
    for kind in ("dependencies", "devDependencies"):
        test.assertEqual(manifest[kind], records[""][kind], kind)
        for name, requirement in manifest[kind].items():
            test.assertEqual(requirement, records["node_modules/" + name]["version"], name)
    for parent, record in records.items():
        if not parent:
            continue
        # This deliberately bounded fixture has one flat instance per name.
        test.assertNotIn("/node_modules/", parent.removeprefix("node_modules/"))
        for kind in ("dependencies", "optionalDependencies"):
            for name, requirement in record.get(kind, {}).items():
                provider = records["node_modules/" + name]["version"]
                effective = overrides.get(name, requirement)
                test.assertEqual(
                    effective, provider,
                    f"{parent} -> {name}: floating native requirement {effective!r}; "
                    f"pin the fixture override to npm's locked {provider}",
                )


class NewsSiteFixtureTests(unittest.TestCase):
    def setUp(self):
        self.manifest = json.loads((PROJECT / "package.json").read_text())
        self.lock = json.loads((PROJECT / "package-lock.json").read_text())

    def test_caniuse_native_requirement_cannot_select_newer_registry_version(self):
        requirement = self.manifest["overrides"].get(
            "caniuse-lite", self.lock["packages"]["node_modules/next"]["dependencies"]["caniuse-lite"]
        )
        self.assertEqual(requirement, self.lock["packages"]["node_modules/caniuse-lite"]["version"])

    def test_native_resolution_inputs_pin_entire_npm_reference_graph(self):
        assert_pinned_graph(self, self.manifest, self.lock)

    def test_missing_caniuse_pin_is_rejected_even_without_a_registry_publication(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["overrides"].pop("caniuse-lite", None)
        with self.assertRaisesRegex(AssertionError, "next -> caniuse-lite"):
            assert_pinned_graph(self, manifest, self.lock)

    def test_missing_optional_sharp_pin_is_rejected(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["overrides"].pop("sharp", None)
        with self.assertRaisesRegex(AssertionError, "next -> sharp"):
            assert_pinned_graph(self, manifest, self.lock)

    def test_override_and_committed_lock_cannot_disagree(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["overrides"]["caniuse-lite"] = "1.0.30001816"
        with self.assertRaisesRegex(AssertionError, "caniuse-lite"):
            assert_pinned_graph(self, manifest, self.lock)

    @unittest.skipUnless(
        os.environ.get("TAPID_NEWS_FIXTURE_BINARY") or os.environ.get("CI"),
        "set TAPID_NEWS_FIXTURE_BINARY for native fixture resolution (required in news-site CI)",
    )
    def test_native_resolver_replays_publication_drift_then_honors_fixture_pin(self):
        binary = str(Path(os.environ["TAPID_NEWS_FIXTURE_BINARY"]).resolve(strict=True))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            records = []
            requirement = self.lock["packages"]["node_modules/next"]["dependencies"]["caniuse-lite"]
            for name, version, dependencies in (
                ("next", "15.5.27", {"caniuse-lite": requirement}),
                ("caniuse-lite", "1.0.30001815", {}),
                ("caniuse-lite", "1.0.30001816", {}),
            ):
                manifest = {"name": name, "version": version, "dependencies": dependencies}
                data = json.dumps(manifest).encode()
                archive = io.BytesIO()
                with tarfile.open(fileobj=archive, mode="w:gz") as tar:
                    header = tarfile.TarInfo("package/package.json")
                    header.size = len(data)
                    tar.addfile(header, io.BytesIO(data))
                artifact = archive.getvalue()
                records.append({**manifest, "registry": "https://registry.npmjs.org",
                    "integrity": "sha512-" + base64.b64encode(hashlib.sha512(artifact).digest()).decode(),
                    "artifact": "base64:" + base64.b64encode(artifact).decode()})
            fixture = root / "registry.json"
            fixture.write_text(json.dumps({"packages": records}))
            for label, overrides, expected in (
                ("floating", {}, "1.0.30001816"),
                ("pinned", {"caniuse-lite": self.manifest["overrides"].get("caniuse-lite", requirement)}, "1.0.30001815"),
            ):
                with self.subTest(label=label):
                    project = root / label
                    project.mkdir()
                    (project / "package.json").write_text(json.dumps({
                        "name": "fixture", "version": "1.0.0", "dependencies": {"next": "15.5.27"}, "overrides": overrides
                    }))
                    # Supply the real npm reference too: native Tapid must not
                    # silently import it instead of applying manifest overrides.
                    (project / "package-lock.json").write_text(json.dumps(self.lock))
                    env = os.environ.copy()
                    env["HOME"] = str(root / "home")
                    Path(env["HOME"]).mkdir(exist_ok=True)
                    result = subprocess.run([binary, "install", "--project-dir", str(project),
                        "--registry-fixture", str(fixture), "--store-dir", str(root / "store")],
                        capture_output=True, text=True, timeout=30, env=env)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    installed = json.loads((project / "node_modules/caniuse-lite/package.json").read_text())
                    self.assertEqual(installed["version"], expected)

    def test_strict_comparator_rejects_caniuse_publication_drift(self):
        # Minimal installed graphs model the exact retained CI mismatch. These
        # are explicit test fixtures, not substitutes for live-install evidence.
        with tempfile.TemporaryDirectory() as temporary:
            roots = [Path(temporary) / manager for manager in ("npm", "tapid")]
            for project, version in zip(roots, ("1.0.30001814", "1.0.30001815")):
                project.mkdir()
                root = {"name": "fixture", "version": "1.0.0", "dependencies": {"next": "15.5.27"}}
                (project / "package.json").write_text(json.dumps(root))
                for name, manifest in {
                    "next": {"name": "next", "version": "15.5.27", "dependencies": {"caniuse-lite": "^1.0.30001579"}},
                    "caniuse-lite": {"name": "caniuse-lite", "version": version},
                }.items():
                    package = project / "node_modules" / name
                    package.mkdir(parents=True)
                    (package / "package.json").write_text(json.dumps(manifest))
                # Shared next provenance matches; only caniuse identity/edge differs.
                (project / "package-lock.json").write_text(json.dumps({
                    "lockfileVersion": 3, "packages": {"": root, "node_modules/next": {
                        "version": "15.5.27", "resolved": "https://registry.npmjs.org/next/-/next-15.5.27.tgz", "integrity": "sha512-test"
                    }}
                }))
                (project / "tapid.lock").write_text(json.dumps({"packages": {"next": {
                    "name": "next", "version": "15.5.27", "registry": "https://registry.npmjs.org", "artifactIntegrity": "sha512-test"
                }}}))
            report = Path(temporary) / "graph.json"
            result = subprocess.run([
                sys.executable, str(COMPARATOR), "--npm-root", str(roots[0]),
                "--tapid-root", str(roots[1]), "--json", str(report),
            ], capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 1, result.stderr)
            graph = json.loads(report.read_text())
            for manager, version in (("npmOnly", "1.0.30001814"), ("tapidOnly", "1.0.30001815")):
                self.assertEqual(graph["reachablePackageDifferences"][manager], [{"name": "caniuse-lite", "version": version}])
                edges = graph["dependencyEdgeDifferences"][manager]
                self.assertEqual(len(edges), 1)
                self.assertEqual(edges[0]["dependency"], "caniuse-lite")
                self.assertEqual(edges[0]["provider"]["version"], version)
            self.assertEqual(graph["sourceIdentityMismatches"], [])
            self.assertEqual(graph["integrityMismatches"], [])


if __name__ == "__main__":
    unittest.main()
