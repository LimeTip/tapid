#!/usr/bin/env python3
"""Tests for unauthenticated public Tapid release verification."""
import contextlib
import gzip
import hashlib
import io
import json
import sys
import tarfile
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))

import public_release

REPOSITORY = "LimeTip/tapid"
VERSION = "0.0.8"
TAG = "v0.0.8"
COMMIT = "a" * 40
API = "https://api.github.com/repos/LimeTip/tapid"
BASE = "https://github.com/LimeTip/tapid/releases/download/v0.0.8"
NOW = datetime(2026, 9, 1, 18, 0, tzinfo=timezone.utc)


class PublicReleaseHelperTests(unittest.TestCase):
    def test_default_verifier_reports_a_bounded_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "release-manifest.json"
            manifest.write_bytes(b"{}")
            with mock.patch.object(
                public_release.subprocess,
                "run",
                side_effect=public_release.subprocess.TimeoutExpired(
                    "cargo",
                    public_release.VERIFIER_TIMEOUT,
                ),
            ) as run:
                with self.assertRaisesRegex(
                    public_release.VerificationError,
                    "verifier timed out",
                ):
                    public_release._default_verifier(manifest, VERSION, TAG, COMMIT)
            self.assertEqual(
                run.call_args.kwargs["timeout"],
                public_release.VERIFIER_TIMEOUT,
            )

    def test_rendered_text_flushes_trailing_parser_data(self):
        self.assertEqual(public_release._rendered_text(b"Tapid<"), "Tapid<")


def archive_bytes(member):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        data = b"binary"
        info = tarfile.TarInfo(member)
        info.size = len(data)
        info.mode = 0o755
        archive.addfile(info, io.BytesIO(data))
    return output.getvalue()


class FakeTransport:
    def __init__(self, responses):
        self.responses = dict(responses)
        self.calls = []

    def fetch(self, url, *, max_bytes, allowed_origins):
        self.calls.append((url, max_bytes, tuple(allowed_origins)))
        if url not in self.responses:
            raise AssertionError("unexpected fetch: " + url)
        value = self.responses[url]
        if isinstance(value, Exception):
            raise value
        final_url, body = value if isinstance(value, tuple) else (url, value)
        if len(body) > max_bytes:
            raise public_release.VerificationError("response exceeds byte limit")
        return public_release.FetchResult(url, final_url, body)


def valid_fixture():
    archives = {}
    artifacts = []
    assets = []
    for target, name in public_release.release_identity.release_archives(VERSION):
        body = archive_bytes("tapid.exe" if "windows" in target else "tapid")
        url = BASE + "/" + name
        archives[url] = body
        artifacts.append({
            "name": name,
            "target": target,
            "url": url,
            "sha256": hashlib.sha256(body).hexdigest(),
            "size": len(body),
        })
        assets.append({"name": name, "browser_download_url": url, "size": len(body)})
    manifest = {
        "schema": "tapid-release-manifest-v1",
        "product": "tapid",
        "version": VERSION,
        "tag": TAG,
        "commit": COMMIT,
        "created_at": "2026-08-31T18:00:00Z",
        "expires_at": "2026-09-30T18:00:00Z",
        "artifacts": artifacts,
        "signature": {"algorithm": "ed25519", "key_id": "release-key-2026-01", "signed_digest": "sha256-" + "0" * 64, "value": "A" * 86 + "=="},
    }
    manifest_url = BASE + "/release-manifest.json"
    stable_url = BASE + "/stable.json"
    manifest_body = json.dumps(manifest).encode()
    stable_body = json.dumps({"channel": "stable", "manifests": [manifest_url]}).encode()
    assets.extend([
        {"name": "release-manifest.json", "browser_download_url": manifest_url, "size": len(manifest_body)},
        {"name": "stable.json", "browser_download_url": stable_url, "size": len(stable_body)},
    ])
    release = {"tag_name": TAG, "draft": False, "prerelease": False, "assets": assets}
    responses = {
        API + "/releases/latest": json.dumps(release).encode(),
        API + "/git/ref/tags/" + TAG: json.dumps({"object": {"type": "tag", "sha": "b" * 40}}).encode(),
        API + "/git/tags/" + "b" * 40: json.dumps({"object": {"type": "commit", "sha": COMMIT}}).encode(),
        manifest_url: manifest_body,
        stable_url: stable_body,
        **archives,
    }
    return responses, manifest


def replace_manifest(responses, manifest):
    body = json.dumps(manifest).encode()
    responses[BASE + "/release-manifest.json"] = body
    release = json.loads(responses[API + "/releases/latest"])
    next(item for item in release["assets"] if item["name"] == "release-manifest.json")["size"] = len(body)
    responses[API + "/releases/latest"] = json.dumps(release).encode()


def replace_archive(responses, manifest, target, body):
    artifact = next(item for item in manifest["artifacts"] if item["target"] == target)
    artifact["size"] = len(body)
    artifact["sha256"] = hashlib.sha256(body).hexdigest()
    responses[artifact["url"]] = body
    release = json.loads(responses[API + "/releases/latest"])
    next(item for item in release["assets"] if item["name"] == artifact["name"])["size"] = len(body)
    responses[API + "/releases/latest"] = json.dumps(release).encode()
    replace_manifest(responses, manifest)


class PublicReleaseTests(unittest.TestCase):
    def test_verifies_exact_downloaded_draft_assets_before_promotion(self):
        responses, manifest = valid_fixture()
        verified = []

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for artifact in manifest["artifacts"]:
                (root / artifact["name"]).write_bytes(responses[artifact["url"]])
            (root / "release-manifest.json").write_bytes(
                responses[BASE + "/release-manifest.json"]
            )
            (root / "stable.json").write_bytes(responses[BASE + "/stable.json"])

            report = public_release.verify_downloaded_release_assets(
                root,
                version=VERSION,
                tag=TAG,
                commit=COMMIT,
                verifier=lambda path, version, tag, commit: verified.append(
                    (path.read_bytes(), version, tag, commit)
                ),
                now=NOW,
            )

        self.assertEqual(report["asset_count"], 8)
        self.assertEqual(len(report["archives"]), 6)
        self.assertEqual(report["signature"], "verified-production-rust")
        self.assertEqual(report["stable"], "verified")
        self.assertEqual(report["freshness"], "valid")
        self.assertEqual(
            [item["member"] for item in report["archives"]],
            ["tapid", "tapid.exe", "tapid", "tapid", "tapid.exe", "tapid"],
        )
        self.assertTrue(all(item["size"] > 0 for item in report["archives"]))
        self.assertTrue(all(len(item["sha256"]) == 64 for item in report["archives"]))
        self.assertEqual([(v, t, c) for _, v, t, c in verified], [(VERSION, TAG, COMMIT)])

    def test_stable_resolves_latest_exact_eight_assets_and_signed_identity(self):
        responses, _ = valid_fixture()
        transport = FakeTransport(responses)
        verified = []

        report = public_release.verify_release_metadata(
            mode="stable", tag=None, transport=transport,
            verifier=lambda path, version, tag, commit: verified.append((path.read_bytes(), version, tag, commit)),
            now=NOW,
        )

        self.assertEqual(report["schema"], "tapid-public-release-verification-v1")
        self.assertEqual(report["release"]["tag"], TAG)
        self.assertEqual(report["release"]["version"], VERSION)
        self.assertEqual(report["release"]["commit"], COMMIT)
        self.assertEqual(len(report["release"]["assets"]), 8)
        self.assertEqual([(v, t, c) for _, v, t, c in verified], [(VERSION, TAG, COMMIT)])
        self.assertNotIn("path", json.dumps(report))

    def test_identity_validation_failures_use_verification_error_contract(self):
        with self.assertRaisesRegex(public_release.VerificationError, "requested release tag"):
            public_release.verify_release_metadata(
                mode="tag",
                tag="not-a-version-tag",
                transport=FakeTransport({}),
                verifier=lambda path, version, tag, commit: None,
                now=NOW,
            )

        malformed_release = {
            "tag_name": "vnot-a-version",
            "draft": False,
            "prerelease": False,
            "assets": [],
        }
        with self.assertRaisesRegex(public_release.VerificationError, "release tag"):
            public_release.verify_release_metadata(
                mode="stable",
                tag=None,
                transport=FakeTransport(
                    {API + "/releases/latest": json.dumps(malformed_release).encode()}
                ),
                verifier=lambda path, version, tag, commit: None,
                now=NOW,
            )

        for version, tag, commit in (
            ("not-a-version", "vnot-a-version", COMMIT),
            (VERSION, TAG, "not-a-commit"),
        ):
            with self.subTest(version=version, tag=tag, commit=commit):
                with self.assertRaisesRegex(
                    public_release.VerificationError,
                    "downloaded release identity",
                ):
                    public_release.verify_downloaded_release_assets(
                        Path("unused"),
                        version=version,
                        tag=tag,
                        commit=commit,
                        verifier=lambda path, version, tag, commit: None,
                        now=NOW,
                    )

    def test_downloads_and_validates_all_six_archives(self):
        responses, _ = valid_fixture()
        report = public_release.verify_public_release(
            mode="stable", tag=None, transport=FakeTransport(responses),
            verifier=lambda path, version, tag, commit: None, now=NOW,
            verify_website=False,
        )

        self.assertEqual(len(report["archives"]), 6)
        self.assertEqual(
            [item["member"] for item in report["archives"]],
            ["tapid", "tapid.exe", "tapid", "tapid", "tapid.exe", "tapid"],
        )
        for item in report["archives"]:
            self.assertRegex(item["sha256"], r"^[0-9a-f]{64}$")
            self.assertGreater(item["size"], 0)
            self.assertEqual(item["url"], BASE + "/" + item["name"])

    def test_rejects_release_asset_size_that_disagrees_with_signed_manifest(self):
        responses, manifest = valid_fixture()
        release = json.loads(responses[API + "/releases/latest"])
        release["assets"][0]["size"] = manifest["artifacts"][0]["size"] + 1
        responses[API + "/releases/latest"] = json.dumps(release).encode()

        with self.assertRaisesRegex(public_release.VerificationError, "size"):
            public_release.verify_public_release(
                mode="stable", tag=None, transport=FakeTransport(responses),
                verifier=lambda path, version, tag, commit: None, now=NOW,
                verify_website=False,
            )

    def test_verifies_website_installers_and_rendered_release_pages(self):
        responses, _ = valid_fixture()
        responses.update({
            "https://tapid.dev/install.sh": b"unix installer\n",
            "https://tapid.dev/install.ps1": b"windows installer\r\n",
            "https://tapid.dev/": b"<main><h1>Package management with clear evidence.</h1><p>Tapid is a JavaScript and TypeScript package manager written in Rust.</p></main>",
            "https://tapid.dev/docs/getting-started/": b"<h1>Getting started</h1><p>Tapid package manager</p><code>curl -fsSL https://tapid.dev/install.sh | bash</code><code><span>iwr</span> <span>-</span><span>useb</span> <span>https:</span><span>//tapid.dev/install.ps1</span> <span>|</span> <span>iex</span></code>",
            "https://tapid.dev/docs/releases/0.0.8/": b"<h1>Tapid 0.0.8</h1><code>curl -fsSL https://tapid.dev/install.sh | bash</code><code><span>iwr</span> <span>-</span><span>useb</span> <span>https:</span><span>//tapid.dev/install.ps1</span> <span>|</span> <span>iex</span></code><p>signed v0.0.8 manifest</p>",
        })
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "scripts/install.sh").write_bytes(b"unix installer\n")
            (root / "scripts/install.ps1").write_bytes(b"windows installer\r\n")

            report = public_release.verify_public_release(
                mode="stable", tag=None, transport=FakeTransport(responses),
                verifier=lambda path, version, tag, commit: None, now=NOW,
                verify_website=True, repository_root=root, sleep=lambda seconds: None,
            )

        self.assertEqual(report["website"]["status"], "verified")
        self.assertEqual(report["website"]["attempts"], 1)
        self.assertEqual(report["website"]["installers"]["install.sh"]["sha256"], hashlib.sha256(b"unix installer\n").hexdigest())
        self.assertEqual(set(report["website"]["pages"]), {"homepage", "getting-started", "release"})

    def test_cli_writes_canonical_json_evidence_for_explicit_tag_mode(self):
        evidence = {"schema": "tapid-public-release-verification-v1", "release": {"tag": TAG}}
        calls = []
        stdout = io.StringIO()
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "evidence.json"
            with contextlib.redirect_stdout(stdout):
                status = public_release.main(
                    ["--mode", "tag", "--tag", TAG, "--output", str(output), "--skip-website"],
                    verify=lambda **kwargs: calls.append(kwargs) or evidence,
                )
            self.assertEqual(output.read_bytes(), (json.dumps(evidence, sort_keys=True, separators=(",", ":")) + "\n").encode())

        self.assertEqual(status, 0)
        self.assertEqual(json.loads(stdout.getvalue()), evidence)
        self.assertEqual(calls[0]["mode"], "tag")
        self.assertEqual(calls[0]["tag"], TAG)
        self.assertFalse(calls[0]["verify_website"])

    def test_cli_redacts_workflow_run_url_query(self):
        evidence = {"schema": "tapid-public-release-verification-v1", "workflow": {}}
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            public_release.main(
                ["--mode", "stable", "--skip-website", "--workflow-run-url",
                 "https://github.com/LimeTip/tapid/actions/runs/1?token=secret"],
                verify=lambda **kwargs: evidence,
            )
        self.assertNotIn("secret", stdout.getvalue())
        self.assertEqual(json.loads(stdout.getvalue())["workflow"]["run_url"],
                         "https://github.com/LimeTip/tapid/actions/runs/1")

    def test_exact_route_fetch_rejects_same_origin_redirect(self):
        transport = FakeTransport({
            "https://tapid.dev/docs/getting-started/":
                ("https://tapid.dev/", b"wrong page"),
        })
        with self.assertRaisesRegex(public_release.VerificationError, "redirect"):
            public_release._checked_fetch(
                transport, "https://tapid.dev/docs/getting-started/",
                max_bytes=100, allowed_origins={"https://tapid.dev"},
                require_exact_final=True,
            )

    def test_accepts_only_bounded_github_asset_redirect_origins(self):
        responses, _ = valid_fixture()
        archive_url = next(url for url in responses if url.endswith(".tar.gz"))
        responses[archive_url] = (
            "https://release-assets.githubusercontent.com/tapid/opaque?jwt=secret&sig=secret",
            responses[archive_url],
        )

        report = public_release.verify_public_release(
            mode="stable", tag=None, transport=FakeTransport(responses),
            verifier=lambda path, version, tag, commit: None, now=NOW,
            verify_website=False,
        )

        redirected = next(item for item in report["archives"] if item["name"] in archive_url)
        self.assertEqual(redirected["url"], "https://release-assets.githubusercontent.com/tapid/opaque")
        self.assertNotIn("secret", json.dumps(report))

    def test_explicit_tag_mode_resolves_only_requested_public_tag(self):
        responses, _ = valid_fixture()
        responses[API + "/releases/tags/" + TAG] = responses.pop(API + "/releases/latest")
        report = public_release.verify_public_release(
            mode="tag", tag=TAG, transport=FakeTransport(responses),
            verifier=lambda path, version, tag, commit: None, now=NOW,
            verify_website=False,
        )
        self.assertEqual(report["mode"], "tag")
        self.assertEqual(report["release"]["tag"], TAG)

    def test_rejects_nonpublic_or_noncanonical_asset_sets(self):
        for mutation in ("draft", "prerelease", "missing", "extra"):
            with self.subTest(mutation=mutation):
                responses, _ = valid_fixture()
                release = json.loads(responses[API + "/releases/latest"])
                if mutation in ("draft", "prerelease"):
                    release[mutation] = True
                elif mutation == "missing":
                    release["assets"].pop()
                else:
                    release["assets"].append({"name": "unexpected", "size": 1, "browser_download_url": BASE + "/unexpected"})
                responses[API + "/releases/latest"] = json.dumps(release).encode()
                with self.assertRaises(public_release.VerificationError):
                    public_release.verify_release_metadata(
                        mode="stable", tag=None, transport=FakeTransport(responses),
                        verifier=lambda path, version, tag, commit: None, now=NOW,
                    )

    def test_rejects_stale_pointer_identity_and_signature_failures(self):
        mutations = ("stable", "stale", "commit", "signature")
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                responses, manifest = valid_fixture()
                verifier = lambda path, version, tag, commit: None
                if mutation == "stable":
                    stable = json.dumps({"channel": "stable", "manifests": ["https://example.test/wrong.json"]}).encode()
                    responses[BASE + "/stable.json"] = stable
                    release = json.loads(responses[API + "/releases/latest"])
                    next(item for item in release["assets"] if item["name"] == "stable.json")["size"] = len(stable)
                    responses[API + "/releases/latest"] = json.dumps(release).encode()
                elif mutation == "stale":
                    manifest["created_at"] = "2026-07-01T18:00:00Z"
                    manifest["expires_at"] = "2026-07-31T18:00:00Z"
                    replace_manifest(responses, manifest)
                elif mutation == "commit":
                    manifest["commit"] = "c" * 40
                    replace_manifest(responses, manifest)
                else:
                    def verifier(path, version, tag, commit):
                        raise public_release.VerificationError("signature failure")
                with self.assertRaises(public_release.VerificationError):
                    public_release.verify_release_metadata(
                        mode="stable", tag=None, transport=FakeTransport(responses),
                        verifier=verifier, now=NOW,
                    )

    def test_verifies_signature_before_freshness_and_tag_network_checks(self):
        responses, manifest = valid_fixture()
        manifest["created_at"] = "2026-07-01T18:00:00Z"
        manifest["expires_at"] = "2026-07-31T18:00:00Z"
        replace_manifest(responses, manifest)
        verified = []

        with self.assertRaisesRegex(public_release.VerificationError, "stale"):
            public_release.verify_release_metadata(
                mode="stable",
                tag=None,
                transport=FakeTransport(responses),
                verifier=lambda path, version, tag, commit: verified.append(path.read_bytes()),
                now=NOW,
            )

        self.assertEqual(verified, [responses[BASE + "/release-manifest.json"]])

    def test_rejects_malformed_tag_object_identity_before_fetching_it(self):
        responses, _ = valid_fixture()
        responses[API + "/git/ref/tags/" + TAG] = json.dumps(
            {"object": {"type": "tag", "sha": "not-a-git-object"}}
        ).encode()

        with self.assertRaisesRegex(public_release.VerificationError, "tag object"):
            public_release.verify_release_metadata(
                mode="stable",
                tag=None,
                transport=FakeTransport(responses),
                verifier=lambda path, version, tag, commit: None,
                now=NOW,
            )

    def test_rejects_digest_size_duplicate_wrong_and_unsafe_archive_members(self):
        cases = ("digest", "size", "duplicate", "wrong", "unsafe")
        target = "aarch64-apple-darwin"
        for case in cases:
            with self.subTest(case=case):
                responses, manifest = valid_fixture()
                artifact = next(item for item in manifest["artifacts"] if item["target"] == target)
                if case == "digest":
                    responses[artifact["url"]] = b"tampered"
                elif case == "size":
                    artifact["size"] += 1
                    replace_manifest(responses, manifest)
                else:
                    output = io.BytesIO()
                    with tarfile.open(fileobj=output, mode="w:gz") as archive:
                        names = {"duplicate": ["tapid", "tapid"], "wrong": ["other"], "unsafe": ["../tapid"]}[case]
                        for name in names:
                            info = tarfile.TarInfo(name)
                            info.size = 1
                            archive.addfile(info, io.BytesIO(b"x"))
                    replace_archive(responses, manifest, target, output.getvalue())
                with self.assertRaises(public_release.VerificationError):
                    public_release.verify_public_release(
                        mode="stable", tag=None, transport=FakeTransport(responses),
                        verifier=lambda path, version, tag, commit: None, now=NOW,
                        verify_website=False,
                    )

    def test_rejects_cross_origin_redirect_and_retries_website_boundedly(self):
        responses, _ = valid_fixture()
        responses[BASE + "/release-manifest.json"] = (
            "https://evil.example/release-manifest.json",
            responses[BASE + "/release-manifest.json"],
        )
        with self.assertRaisesRegex(public_release.VerificationError, "origin"):
            public_release.verify_release_metadata(
                mode="stable", tag=None, transport=FakeTransport(responses),
                verifier=lambda path, version, tag, commit: None, now=NOW,
            )

        responses, _ = valid_fixture()
        responses.update({
            "https://tapid.dev/install.sh": b"wrong",
            "https://tapid.dev/install.ps1": b"windows installer\n",
            "https://tapid.dev/": b"unused",
            "https://tapid.dev/getting-started": b"unused",
            "https://tapid.dev/releases/v0.0.8": b"unused",
        })
        sleeps = []
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "scripts/install.sh").write_bytes(b"canonical")
            (root / "scripts/install.ps1").write_bytes(b"windows installer\n")
            with self.assertRaisesRegex(public_release.WebsiteVerificationError, "4 attempts"):
                public_release.verify_public_release(
                    mode="stable", tag=None, transport=FakeTransport(responses),
                    verifier=lambda path, version, tag, commit: None, now=NOW,
                    repository_root=root, sleep=sleeps.append,
                )
        self.assertEqual(sleeps, [5, 5, 5])

    def test_uses_the_exact_rust_verified_manifest_bytes_for_archive_metadata(self):
        responses, _ = valid_fixture()
        transport = FakeTransport(responses)
        public_release.verify_public_release(
            mode="stable", tag=None, transport=transport,
            verifier=lambda path, version, tag, commit: None, now=NOW,
            verify_website=False,
        )
        manifest_url = BASE + "/release-manifest.json"
        self.assertEqual([call[0] for call in transport.calls].count(manifest_url), 1)

    def test_malformed_asset_names_fail_with_a_verification_error(self):
        responses, _ = valid_fixture()
        release = json.loads(responses[API + "/releases/latest"])
        release["assets"][0]["name"] = []
        responses[API + "/releases/latest"] = json.dumps(release).encode()
        with self.assertRaises(public_release.VerificationError):
            public_release.verify_release_metadata(
                mode="stable", tag=None, transport=FakeTransport(responses),
                verifier=lambda path, version, tag, commit: None, now=NOW,
            )

    def test_transport_accepts_generator_of_allowed_origins(self):
        class Socket:
            def fileno(self):
                return -1

        class Response:
            def __init__(self):
                self.fp = type("Fp", (), {"raw": type("Raw", (), {"_sock": Socket()})()})()
                self.headers = {}
                self.body = b"public evidence"

            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

            def geturl(self):
                return "https://example.test/evidence.json"

            def read(self, size):
                body, self.body = self.body[:size], self.body[size:]
                return body

        class Opener:
            def open(self, request, timeout):
                return Response()

        transport = public_release.PublicTransport()
        transport._opener = Opener()
        origins = (origin for origin in ["https://example.test"])

        fetched = transport.fetch(
            "https://example.test/evidence.json", max_bytes=100,
            allowed_origins=origins,
        )

        self.assertEqual(fetched.body, b"public evidence")

    def test_transport_read_enforces_a_total_deadline(self):
        class Socket:
            def __init__(self):
                self.timeouts = []

            def settimeout(self, timeout):
                self.timeouts.append(timeout)

            def fileno(self):
                return 1

        class Response:
            def __init__(self):
                self.fp = type("Fp", (), {"raw": type("Raw", (), {"_sock": Socket()})()})()

            def read(self, size):
                return b"x"

        ticks = iter((0.0, 3.0, 6.0))
        with self.assertRaisesRegex(public_release.VerificationError, "deadline"):
            public_release._read_bounded(Response(), 10, 5, clock=lambda: next(ticks))

    def test_rejects_archive_member_larger_than_unpacked_bound(self):
        responses, manifest = valid_fixture()
        target = "aarch64-apple-darwin"
        info = tarfile.TarInfo("tapid")
        info.size = public_release.ARCHIVE_UNPACKED_LIMIT + 1
        body = gzip.compress(info.tobuf() + b"\0" * 1024)
        replace_archive(responses, manifest, target, body)
        with self.assertRaisesRegex(public_release.VerificationError, "unpacked size"):
            public_release.verify_public_release(
                mode="stable", tag=None, transport=FakeTransport(responses),
                verifier=lambda path, version, tag, commit: None, now=NOW,
                verify_website=False,
            )


if __name__ == "__main__":
    unittest.main()
