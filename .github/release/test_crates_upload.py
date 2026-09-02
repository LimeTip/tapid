import gzip
import hashlib
import http.client
import http.server
import io
import json
import os
import struct
import tarfile
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

import crates_upload


def crate_archive(
    name="tapid-core",
    version="0.0.2",
    *,
    readme="README.md",
    duplicate=False,
    manifest_text=None,
    extra_files=(),
    raw_extra_files=(),
):
    manifest = (
        manifest_text
        or (
            "[package]\n"
            f'name = "{name}"\n'
            f'version = "{version}"\n'
            'authors = ["LimeTip"]\n'
            'description = "Tapid core test crate"\n'
            'license = "MIT"\n'
            'repository = "https://github.com/LimeTip/tapid"\n'
            'rust-version = "1.88"\n'
            + (f'readme = "{readme}"\n' if readme is not None else "")
        )
    ).encode()
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        members = [
            (f"{name}-{version}/Cargo.toml", manifest),
            (f"{name}-{version}/src/lib.rs", b"pub fn value() -> u8 { 1 }\n"),
        ]
        if readme is not None:
            members.append((f"{name}-{version}/{readme}", b"reviewed readme\n"))
        members.extend(
            (f"{name}-{version}/{path}", payload) for path, payload in extra_files
        )
        members.extend(raw_extra_files)
        if duplicate:
            members.append((f"{name}-{version}/Cargo.toml", manifest))
        for path, payload in members:
            member = tarfile.TarInfo(path)
            member.size = len(payload)
            member.mode = 0o644
            archive.addfile(member, io.BytesIO(payload))
    return output.getvalue()


class ExactArchivePreparationTests(unittest.TestCase):
    def _prepare(self, archive, **overrides):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "tapid-core-0.0.2.crate"
        path.write_bytes(archive)
        arguments = {
            "expected_name": "tapid-core",
            "expected_version": "0.0.2",
            "expected_size": len(archive),
            "expected_sha256": hashlib.sha256(archive).hexdigest(),
        }
        arguments.update(overrides)
        return crates_upload.prepare_upload(path, **arguments)

    def test_golden_body_uses_documented_framing_and_exact_reviewed_bytes(self):
        archive = crate_archive()
        prepared = self._prepare(archive)

        metadata_length = struct.unpack_from("<I", prepared.body, 0)[0]
        metadata_start = 4
        metadata_end = metadata_start + metadata_length
        metadata = json.loads(prepared.body[metadata_start:metadata_end])
        archive_length = struct.unpack_from("<I", prepared.body, metadata_end)[0]
        embedded = prepared.body[metadata_end + 4:]

        self.assertEqual(metadata, {
            "authors": ["LimeTip"],
            "badges": {},
            "categories": [],
            "deps": [],
            "description": "Tapid core test crate",
            "documentation": None,
            "features": {},
            "homepage": None,
            "keywords": [],
            "license": "MIT",
            "license_file": None,
            "links": None,
            "name": "tapid-core",
            "readme": "reviewed readme\n",
            "readme_file": "README.md",
            "repository": "https://github.com/LimeTip/tapid",
            "rust_version": "1.88",
            "vers": "0.0.2",
        })
        self.assertEqual(archive_length, len(archive))
        self.assertEqual(embedded, archive)
        self.assertEqual(crates_upload.parse_upload_body(prepared.body).archive, archive)

    def test_metadata_matches_cargo_publish_dependencies_and_features(self):
        manifest = """[package]
name = "tapid-core"
version = "0.0.2"
authors = ["LimeTip", "Contributor"]
description = "Complete Cargo publish metadata"
documentation = "https://docs.rs/tapid-core"
homepage = "https://tapid.dev"
keywords = ["package-manager", "security"]
categories = ["command-line-utilities"]
readme = "README.md"
license = "MIT"
repository = "https://github.com/LimeTip/tapid"
links = "tapid_core_native"
rust-version = "1.88"

[dependencies.renamed]
package = "actual-package"
version = "^1.2"
features = ["derive"]
optional = true
default-features = false

[build-dependencies.builder]
version = "3"

[target.'cfg(unix)'.dev-dependencies.test-helper]
version = "~2.4"
features = ["fixture"]

[features]
default = ["renamed"]
full = ["renamed/derive"]

[badges.maintenance]
status = "experimental"
"""
        prepared = self._prepare(crate_archive(manifest_text=manifest))

        self.assertEqual(prepared.metadata["features"], {
            "default": ["renamed"],
            "full": ["renamed/derive"],
        })
        self.assertEqual(prepared.metadata["deps"], [
            {
                "default_features": False,
                "explicit_name_in_toml": "renamed",
                "features": ["derive"],
                "kind": "normal",
                "name": "actual-package",
                "optional": True,
                "target": None,
                "version_req": "^1.2",
            },
            {
                "default_features": True,
                "features": [],
                "kind": "build",
                "name": "builder",
                "optional": False,
                "target": None,
                "version_req": "3",
            },
            {
                "default_features": True,
                "features": ["fixture"],
                "kind": "dev",
                "name": "test-helper",
                "optional": False,
                "target": "cfg(unix)",
                "version_req": "~2.4",
            },
        ])
        self.assertEqual(prepared.metadata["authors"], ["LimeTip", "Contributor"])
        self.assertEqual(prepared.metadata["badges"], {
            "maintenance": {"status": "experimental"}
        })
        self.assertEqual(prepared.metadata["links"], "tapid_core_native")

    def test_license_file_must_be_an_exact_reviewed_archive_member(self):
        manifest = """[package]
name = "tapid-core"
version = "0.0.2"
description = "License file package"
license-file = "LICENSE"
readme = "README.md"
"""
        with self.assertRaisesRegex(crates_upload.UploadError, "license file"):
            self._prepare(crate_archive(manifest_text=manifest))

        prepared = self._prepare(
            crate_archive(
                manifest_text=manifest,
                extra_files=(("LICENSE", b"reviewed license\n"),),
            )
        )
        self.assertEqual(prepared.metadata["license"], None)
        self.assertEqual(prepared.metadata["license_file"], "LICENSE")

    def test_rejects_members_outside_the_single_canonical_package_root(self):
        malformed_archives = (
            crate_archive(raw_extra_files=(("../outside", b"escape"),)),
            crate_archive(raw_extra_files=(("second-root/file", b"other"),)),
            crate_archive(extra_files=(("src/lib.rs", b"duplicate"),)),
            crate_archive(extra_files=(("path\\name", b"windows separator"),)),
        )
        for archive in malformed_archives:
            with self.subTest(archive_sha256=hashlib.sha256(archive).hexdigest()):
                with self.assertRaisesRegex(crates_upload.UploadError, "member"):
                    self._prepare(archive)

    def test_rejects_archive_drift_before_token_access(self):
        archive = crate_archive()
        token_reads = []
        with self.assertRaisesRegex(crates_upload.UploadError, "checksum"):
            self._prepare(archive[:-1] + bytes([archive[-1] ^ 1]), expected_sha256=hashlib.sha256(archive).hexdigest())
        self.assertEqual(token_reads, [])

    def test_rejects_wrong_size_and_symlink(self):
        archive = crate_archive()
        with self.assertRaisesRegex(crates_upload.UploadError, "size"):
            self._prepare(archive, expected_size=len(archive) + 1)
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "target.crate"
            target.write_bytes(archive)
            link = Path(directory) / "link.crate"
            link.symlink_to(target)
            with self.assertRaisesRegex(crates_upload.UploadError, "regular non-symlink"):
                crates_upload.prepare_upload(
                    link,
                    expected_name="tapid-core",
                    expected_version="0.0.2",
                    expected_size=len(archive),
                    expected_sha256=hashlib.sha256(archive).hexdigest(),
                )

    def test_rejects_duplicate_missing_and_mismatched_manifest(self):
        for archive, message in (
            (crate_archive(duplicate=True), "exactly one reviewed manifest"),
            (gzip.compress(b"not a tar"), "malformed"),
            (b"\x1f\x8b\x08\x00", "malformed"),
            (crate_archive(name="other"), "manifest identity"),
            (crate_archive(version="9.9.9"), "manifest identity"),
        ):
            with self.subTest(message=message):
                with self.assertRaisesRegex(crates_upload.UploadError, message):
                    self._prepare(archive)

    def test_readme_is_loaded_only_from_archive_and_must_stay_in_root(self):
        archive = crate_archive(readme=None)
        prepared = self._prepare(archive)
        self.assertIsNone(prepared.metadata["readme"])
        self.assertIsNone(prepared.metadata["readme_file"])

        with self.assertRaisesRegex(crates_upload.UploadError, "README path"):
            self._prepare(crate_archive(readme="../secret"))

    def test_parser_rejects_invalid_complete_cargo_metadata_types(self):
        prepared = self._prepare(crate_archive())
        parsed = crates_upload.parse_upload_body(prepared.body)
        invalid_values = (
            ("deps", {}),
            ("features", []),
            ("authors", "LimeTip"),
            ("description", []),
            ("badges", []),
            ("keywords", "security"),
            ("license", 7),
            ("rust_version", []),
        )
        for field, value in invalid_values:
            with self.subTest(field=field):
                metadata = dict(parsed.metadata)
                metadata[field] = value
                body = crates_upload.build_upload_body(metadata, parsed.archive)
                with self.assertRaisesRegex(crates_upload.UploadError, "metadata"):
                    crates_upload.parse_upload_body(body)

    def test_parser_rejects_invalid_cargo_dependency_metadata(self):
        prepared = self._prepare(crate_archive())
        parsed = crates_upload.parse_upload_body(prepared.body)
        dependency = {
            "default_features": True,
            "features": [],
            "kind": "normal",
            "name": "dependency",
            "optional": False,
            "target": None,
            "version_req": "^1",
        }
        invalid_dependencies = (
            "not-an-object",
            {},
            {**dependency, "unknown": "value"},
            {**dependency, "optional": 1},
            {**dependency, "default_features": 1},
            {**dependency, "name": ""},
            {**dependency, "features": "feature"},
            {**dependency, "version_req": ""},
            {**dependency, "target": 7},
            {**dependency, "kind": "runtime"},
            {**dependency, "kind": []},
            {**dependency, "kind": {}},
            {**dependency, "registry": 7},
            {**dependency, "registry": None},
            {**dependency, "registry": ""},
            {**dependency, "explicit_name_in_toml": 7},
            {**dependency, "explicit_name_in_toml": None},
            {**dependency, "artifact": "bin"},
            {**dependency, "artifact": []},
            {**dependency, "bindep_target": 7},
            {**dependency, "lib": 1},
            {**dependency, "lib": False},
        )
        for invalid in invalid_dependencies:
            with self.subTest(invalid=invalid):
                metadata = dict(parsed.metadata)
                metadata["deps"] = [invalid]
                body = crates_upload.build_upload_body(metadata, parsed.archive)
                with self.assertRaisesRegex(crates_upload.UploadError, "dependenc"):
                    crates_upload.parse_upload_body(body)

        metadata = dict(parsed.metadata)
        metadata["deps"] = [{
            **dependency,
            "registry": "https://example.invalid/index",
            "explicit_name_in_toml": "renamed",
            "artifact": ["bin"],
            "bindep_target": "x86_64-unknown-linux-gnu",
            "lib": True,
        }]
        body = crates_upload.build_upload_body(metadata, parsed.archive)
        self.assertEqual(crates_upload.parse_upload_body(body).metadata, metadata)

    def test_parser_rejects_duplicate_json_keys_at_every_depth(self):
        prepared = self._prepare(crate_archive())
        parsed = crates_upload.parse_upload_body(prepared.body)
        dependency = {
            "default_features": True,
            "features": [],
            "kind": "normal",
            "name": "dependency",
            "optional": False,
            "target": None,
            "version_req": "^1",
        }
        metadata = dict(parsed.metadata)
        metadata["deps"] = [dependency]
        canonical = json.dumps(metadata, sort_keys=True, separators=(",", ":"))
        duplicate_documents = (
            canonical.replace(
                '"name":"tapid-core"',
                '"name":"other","name":"tapid-core"',
                1,
            ),
            canonical.replace(
                '"kind":"normal"',
                '"kind":"build","kind":"normal"',
                1,
            ),
        )
        for document in duplicate_documents:
            metadata_bytes = document.encode()
            body = (
                struct.pack("<I", len(metadata_bytes))
                + metadata_bytes
                + struct.pack("<I", len(parsed.archive))
                + parsed.archive
            )
            with self.subTest(document=document):
                with self.assertRaisesRegex(crates_upload.UploadError, "duplicate"):
                    crates_upload.parse_upload_body(body)

    def test_parser_rejects_trailing_and_overflowing_lengths(self):
        archive = crate_archive()
        prepared = self._prepare(archive)
        for body in (
            prepared.body + b"trailing",
            struct.pack("<I", crates_upload.MAX_METADATA_BYTES + 1),
            prepared.body[:4] + prepared.body[4:-1],
        ):
            with self.subTest(length=len(body)):
                with self.assertRaises(crates_upload.UploadError):
                    crates_upload.parse_upload_body(body)

    def test_rejects_bounded_decompression_and_member_count_before_parsing(self):
        archive = crate_archive()
        with mock.patch.object(crates_upload, "MAX_UNPACKED_BYTES", 64):
            with self.assertRaisesRegex(crates_upload.UploadError, "unpacked size"):
                self._prepare(archive)
        with mock.patch.object(crates_upload, "MAX_ARCHIVE_MEMBERS", 2):
            with self.assertRaisesRegex(crates_upload.UploadError, "too many members"):
                self._prepare(archive)


class _RegistryHandler(http.server.BaseHTTPRequestHandler):
    status = 200
    response = b'{"ok":true}'
    requests = []

    def do_PUT(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        type(self).requests.append((self.path, dict(self.headers), body))
        self.send_response(type(self).status)
        if 300 <= type(self).status < 400:
            self.send_header("Location", "http://attacker.invalid/stolen")
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(type(self).response)

    def log_message(self, *args):
        pass


class ExactArchiveNetworkTests(unittest.TestCase):
    def setUp(self):
        _RegistryHandler.status = 200
        _RegistryHandler.response = b'{"ok":true}'
        _RegistryHandler.requests = []
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _RegistryHandler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.directory = tempfile.TemporaryDirectory()
        archive = crate_archive()
        path = Path(self.directory.name) / "tapid-core-0.0.2.crate"
        path.write_bytes(archive)
        self.archive = archive
        self.prepared = crates_upload.prepare_upload(
            path,
            expected_name="tapid-core",
            expected_version="0.0.2",
            expected_size=len(archive),
            expected_sha256=hashlib.sha256(archive).hexdigest(),
        )

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.directory.cleanup()

    @property
    def endpoint(self):
        return "http://127.0.0.1:{}/api/v1/crates/new".format(self.server.server_address[1])

    def test_sends_one_exact_request_and_reads_token_only_after_local_validation(self):
        token_reads = []
        result = crates_upload.upload_prepared(
            self.prepared,
            token_reader=lambda: token_reads.append("read") or "temporary-oidc-token",
            endpoint=self.endpoint,
            allow_insecure_endpoint=True,
            timeout=1,
        )

        self.assertEqual(result["status"], 200)
        self.assertEqual(token_reads, ["read"])
        self.assertEqual(len(_RegistryHandler.requests), 1)
        path, headers, body = _RegistryHandler.requests[0]
        self.assertEqual(path, "/api/v1/crates/new")
        self.assertEqual(headers["Authorization"], "temporary-oidc-token")
        self.assertEqual(headers["User-Agent"], crates_upload.USER_AGENT)
        self.assertEqual(body, self.prepared.body)
        self.assertEqual(crates_upload.parse_upload_body(body).archive, self.archive)

    def test_corrupt_prepared_body_is_rejected_before_token_or_network(self):
        corrupt = crates_upload.PreparedUpload(
            metadata=self.prepared.metadata,
            body=self.prepared.body[:-1] + bytes([self.prepared.body[-1] ^ 1]),
            body_sha256=self.prepared.body_sha256,
            archive_sha256=self.prepared.archive_sha256,
            archive_size=self.prepared.archive_size,
        )
        token_reads = []
        with self.assertRaisesRegex(crates_upload.UploadError, "prepared upload"):
            crates_upload.upload_prepared(
                corrupt,
                token_reader=lambda: token_reads.append("read") or "secret",
                endpoint=self.endpoint,
                allow_insecure_endpoint=True,
            )
        self.assertEqual(token_reads, [])
        self.assertEqual(_RegistryHandler.requests, [])

    def test_rejects_noncanonical_or_nonlocal_endpoints_before_token_access(self):
        token_reads = []
        for endpoint, allow_insecure in (
            ("https://attacker.invalid/api/v1/crates/new", False),
            ("http://attacker.invalid/api/v1/crates/new", True),
        ):
            with self.subTest(endpoint=endpoint):
                with self.assertRaisesRegex(crates_upload.UploadError, "endpoint"):
                    crates_upload.upload_prepared(
                        self.prepared,
                        token_reader=lambda: token_reads.append("read") or "secret",
                        endpoint=endpoint,
                        allow_insecure_endpoint=allow_insecure,
                    )
        self.assertEqual(token_reads, [])

    def test_redirect_and_registry_errors_fail_without_retry(self):
        for status, response, message in (
            (302, b"redirect", "redirect"),
            (400, b'{"errors":[{"detail":"bad crate"}]}', "bad crate"),
            (401, b'{"errors":[{"detail":"bad token"}]}', "bad token"),
            (403, b'{"errors":[{"detail":"forbidden"}]}', "forbidden"),
            (413, b'{"errors":[{"detail":"too large"}]}', "too large"),
            (429, b'{"errors":[{"detail":"slow down"}]}', "slow down"),
            (200, b'{"errors":[{"detail":"logical failure"}]}', "logical failure"),
        ):
            with self.subTest(status=status):
                _RegistryHandler.requests = []
                _RegistryHandler.status = status
                _RegistryHandler.response = response
                with self.assertRaisesRegex(crates_upload.UploadError, message):
                    crates_upload.upload_prepared(
                        self.prepared,
                        token_reader=lambda: "secret",
                        endpoint=self.endpoint,
                        allow_insecure_endpoint=True,
                        timeout=1,
                    )
                self.assertEqual(len(_RegistryHandler.requests), 1)

    def test_successful_oversized_response_is_unknown_and_never_retried(self):
        _RegistryHandler.status = 200
        _RegistryHandler.response = b"x" * (crates_upload.MAX_RESPONSE_BYTES + 1)
        with self.assertRaisesRegex(crates_upload.UnknownPublicationState, "unknown"):
            crates_upload.upload_prepared(
                self.prepared,
                token_reader=lambda: "temporary-oidc-token",
                endpoint=self.endpoint,
                allow_insecure_endpoint=True,
                timeout=1,
            )
        self.assertEqual(len(_RegistryHandler.requests), 1)

    def test_server_failure_after_request_is_unknown_and_never_retried(self):
        _RegistryHandler.status = 503
        _RegistryHandler.response = b'{"errors":[{"detail":"temporarily unavailable"}]}'
        with self.assertRaisesRegex(crates_upload.UnknownPublicationState, "unknown"):
            crates_upload.upload_prepared(
                self.prepared,
                token_reader=lambda: "secret",
                endpoint=self.endpoint,
                allow_insecure_endpoint=True,
                timeout=1,
            )
        self.assertEqual(len(_RegistryHandler.requests), 1)

    def test_connection_loss_after_request_is_unknown_and_never_retried(self):
        calls = []

        class LostConnection:
            def request(self, *args, **kwargs):
                calls.append((args, kwargs))
                raise ConnectionResetError("lost after send")

            def close(self):
                pass

        with self.assertRaisesRegex(crates_upload.UnknownPublicationState, "unknown"):
            crates_upload.upload_prepared(
                self.prepared,
                token_reader=lambda: "secret",
                connection_factory=lambda *args, **kwargs: LostConnection(),
            )
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
