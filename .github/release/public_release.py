#!/usr/bin/env python3
"""Unauthenticated verification of public Tapid release evidence."""
from __future__ import annotations

import argparse
import hashlib
import html.parser
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Iterable, Optional
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlparse, urlsplit, urlunsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

import release_identity

REPOSITORY = "LimeTip/tapid"
GITHUB_API = "https://api.github.com/repos/" + REPOSITORY
METADATA_LIMIT = 1024 * 1024
ARCHIVE_LIMIT = 100 * 1024 * 1024
ARCHIVE_UNPACKED_LIMIT = 256 * 1024 * 1024
HTTP_TIMEOUT = 30
GITHUB_DOWNLOAD_ORIGINS = {
    "https://github.com",
    "https://release-assets.githubusercontent.com",
    "https://objects.githubusercontent.com",
}


class VerificationError(RuntimeError):
    """The public evidence did not satisfy the release contract."""


class WebsiteVerificationError(VerificationError):
    """Website delivery remained invalid after bounded propagation retries."""


@dataclass(frozen=True)
class FetchResult:
    """Auditable result from one bounded public fetch."""

    requested_url: str
    final_url: str
    body: bytes


class _BoundedHttpsRedirects(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urlparse(newurl).scheme != "https":
            raise VerificationError("redirect must remain on HTTPS")
        allowed_origins = getattr(req, "_tapid_allowed_origins", set())
        if _origin(newurl) not in allowed_origins:
            raise VerificationError("redirect reached an unapproved origin")
        deadline = getattr(req, "_tapid_deadline", None)
        remaining = deadline - time.monotonic() if deadline is not None else None
        if remaining is not None and remaining <= 0:
            raise VerificationError("public request exceeded total deadline")
        count = getattr(req, "_tapid_redirect_count", 0) + 1
        if count > 3:
            raise VerificationError("too many HTTP redirects")
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is None:
            return None
        redirected._tapid_redirect_count = count
        redirected._tapid_allowed_origins = allowed_origins
        redirected._tapid_deadline = deadline
        if remaining is not None:
            redirected.timeout = remaining
        return redirected


def _read_bounded(response, max_bytes: int, timeout: float, clock=time.monotonic,
                  deadline: Optional[float] = None) -> bytes:
    """Read with byte and total-wall-clock bounds, tightening the socket timeout."""
    try:
        sock = response.fp.raw._sock
    except AttributeError as error:
        raise VerificationError("cannot enforce response deadline") from error
    started = clock()
    deadline = started + timeout if deadline is None else deadline
    chunks = []
    total = 0
    while total <= max_bytes:
        remaining = deadline - clock()
        if remaining <= 0:
            raise VerificationError("public response exceeded total deadline")
        if sock.fileno() >= 0:
            sock.settimeout(max(0.001, remaining))
        chunk = response.read(min(64 * 1024, max_bytes + 1 - total))
        if not chunk:
            break
        chunks.append(chunk)
        total += len(chunk)
    body = b"".join(chunks)
    if len(body) > max_bytes:
        raise VerificationError("response exceeds byte limit")
    return body


class PublicTransport:
    """Credential-free HTTPS transport with explicit bounds and origins."""

    def __init__(self, timeout: int = HTTP_TIMEOUT):
        self.timeout = timeout
        self._opener = build_opener(ProxyHandler({}), _BoundedHttpsRedirects())

    def fetch(self, url: str, *, max_bytes: int, allowed_origins: Iterable[str]) -> FetchResult:
        parsed = urlparse(url)
        origin = _origin(url)
        origins = set(allowed_origins)
        if parsed.scheme != "https" or origin not in origins:
            raise VerificationError("public URL has an unapproved HTTPS origin")
        deadline = time.monotonic() + self.timeout
        request = Request(url, headers={"User-Agent": "tapid-public-release-verifier/1"})
        request._tapid_allowed_origins = origins
        request._tapid_deadline = deadline
        try:
            with self._opener.open(request, timeout=self.timeout) as response:
                final_url = response.geturl()
                if _origin(final_url) not in origins:
                    raise VerificationError("redirect reached an unapproved origin")
                content_length = response.headers.get("Content-Length")
                if content_length is not None:
                    try:
                        if int(content_length) > max_bytes:
                            raise VerificationError("response exceeds byte limit")
                    except ValueError as error:
                        raise VerificationError("malformed Content-Length") from error
                body = _read_bounded(response, max_bytes, self.timeout, deadline=deadline)
        except HTTPError as error:
            raise VerificationError("public endpoint returned HTTP {}".format(error.code)) from error
        except (URLError, TimeoutError, OSError) as error:
            raise VerificationError("public endpoint request failed") from error
        if len(body) > max_bytes:
            raise VerificationError("response exceeds byte limit")
        return FetchResult(url, final_url, body)


def _origin(url: str) -> str:
    parsed = urlparse(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        raise VerificationError("URL must be absolute credential-free HTTPS")
    port = parsed.port
    return "https://" + parsed.hostname.lower() + ((":" + str(port)) if port else "")


def _strict_json(body: bytes, label: str):
    def object_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise VerificationError(label + " contains duplicate JSON fields")
            result[key] = value
        return result

    try:
        return json.loads(body.decode("utf-8"), object_pairs_hook=object_pairs)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise VerificationError(label + " is malformed JSON") from error


def _checked_fetch(transport, url: str, *, max_bytes: int, allowed_origins: Iterable[str],
                   require_exact_final: bool = True):
    origins = set(allowed_origins)
    if _origin(url) not in origins:
        raise VerificationError("requested URL has an unapproved origin")
    fetched = transport.fetch(url, max_bytes=max_bytes, allowed_origins=origins)
    if fetched.requested_url != url or _origin(fetched.final_url) not in origins:
        raise VerificationError("redirected or substituted public response has an unapproved origin")
    if require_exact_final and fetched.final_url != url:
        raise VerificationError("redirected public response does not match the required route")
    if len(fetched.body) > max_bytes:
        raise VerificationError("response exceeds byte limit")
    return fetched


def _fetch_json(transport, url: str, origins: Iterable[str], label: str):
    fetched = _checked_fetch(transport, url, max_bytes=METADATA_LIMIT, allowed_origins=origins)
    return fetched, _strict_json(fetched.body, label)


def _parse_time(value, field):
    if not isinstance(value, str) or not value.endswith("Z"):
        raise VerificationError("manifest " + field + " is not canonical UTC RFC3339")
    try:
        parsed = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise VerificationError("manifest " + field + " is invalid") from error
    if parsed.microsecond:
        raise VerificationError("manifest " + field + " must have second precision")
    return parsed.astimezone(timezone.utc)


def _default_verifier(manifest_path: Path, version: str, tag: str, commit: str):
    root = Path(__file__).resolve().parents[2]
    command = [
        "cargo", "run", "--locked", "--quiet", "--manifest-path",
        str(root / ".github/release/verifier/Cargo.toml"), "--",
        str(manifest_path), str(root / "crates/tapid-signatures/data/release-keyring.json"),
        version, tag, commit,
    ]
    result = subprocess.run(command, cwd=root, text=True, capture_output=True)
    if result.returncode:
        detail = result.stderr.strip().splitlines()[-1] if result.stderr.strip() else "unknown error"
        raise VerificationError("production Rust verifier rejected manifest: " + detail)


def _redacted_url(url: str) -> str:
    parsed = urlsplit(url)
    return urlunsplit((parsed.scheme, parsed.netloc, parsed.path, "", ""))


def _evidence(fetched: FetchResult):
    return {
        "url": _redacted_url(fetched.final_url),
        "bytes": len(fetched.body),
        "sha256": hashlib.sha256(fetched.body).hexdigest(),
    }


def _verify_release_metadata_context(*, mode: str, tag: Optional[str], transport=None,
                                     verifier: Optional[Callable] = None, now=None):
    """Resolve and verify public release metadata without credentials or mutation."""
    if mode not in ("stable", "tag"):
        raise VerificationError("mode must be stable or tag")
    if mode == "stable" and tag is not None:
        raise VerificationError("stable mode does not accept a tag")
    if mode == "tag" and tag is None:
        raise VerificationError("tag mode requires an explicit tag")
    if tag is not None:
        release_identity.validate_version_tag(tag[1:] if tag.startswith("v") else "", tag)

    transport = transport or PublicTransport()
    verifier = verifier or _default_verifier
    now = now or datetime.now(timezone.utc)
    if now.tzinfo is None or now.utcoffset() is None:
        raise VerificationError("verification clock must be timezone-aware")
    now = now.astimezone(timezone.utc)
    release_url = GITHUB_API + ("/releases/latest" if mode == "stable" else "/releases/tags/" + quote(tag, safe=""))
    release_fetch, release = _fetch_json(transport, release_url, {"https://api.github.com"}, "release response")
    if not isinstance(release, dict):
        raise VerificationError("release response must be an object")
    resolved_tag = release.get("tag_name")
    if mode == "tag" and resolved_tag != tag:
        raise VerificationError("release tag does not match requested tag")
    if release.get("draft") is not False or release.get("prerelease") is not False:
        raise VerificationError("release must be public and stable")
    if not isinstance(resolved_tag, str) or not resolved_tag.startswith("v"):
        raise VerificationError("release tag is malformed")
    version = resolved_tag[1:]
    release_identity.validate_version_tag(version, resolved_tag)

    expected_archives = dict(release_identity.release_archives(version))
    expected_names = set(expected_archives.values()) | {"release-manifest.json", "stable.json"}
    assets = release.get("assets")
    if not isinstance(assets, list) or any(not isinstance(item, dict) for item in assets):
        raise VerificationError("release assets must be an array of objects")
    names = [item.get("name") for item in assets]
    if any(not isinstance(name, str) for name in names):
        raise VerificationError("release asset names must be strings")
    if len(names) != len(set(names)) or set(names) != expected_names or len(names) != 8:
        raise VerificationError("release must contain the exact canonical eight-asset set")
    asset_map = {item["name"]: item for item in assets}
    base = "https://github.com/{}/releases/download/{}".format(REPOSITORY, resolved_tag)
    for name, asset in asset_map.items():
        expected_url = base + "/" + name
        if asset.get("browser_download_url") != expected_url:
            raise VerificationError("release asset URL is not canonically bound to the tag")
        if not isinstance(asset.get("size"), int) or isinstance(asset.get("size"), bool) or asset["size"] < 1:
            raise VerificationError("release asset size is invalid")

    manifest_fetch = _checked_fetch(
        transport, asset_map["release-manifest.json"]["browser_download_url"],
        max_bytes=METADATA_LIMIT, allowed_origins=GITHUB_DOWNLOAD_ORIGINS, require_exact_final=False,
    )
    manifest = _strict_json(manifest_fetch.body, "release manifest")
    if len(manifest_fetch.body) != asset_map["release-manifest.json"]["size"]:
        raise VerificationError("release manifest size disagrees with public asset metadata")
    if not isinstance(manifest, dict):
        raise VerificationError("release manifest must be an object")
    commit = manifest.get("commit")
    try:
        release_identity.validate_commit(commit)
    except (TypeError, ValueError) as error:
        raise VerificationError("manifest commit is malformed") from error
    if manifest.get("version") != version or manifest.get("tag") != resolved_tag:
        raise VerificationError("manifest identity does not match release")

    with tempfile.TemporaryDirectory(prefix="tapid-public-release-") as directory:
        path = Path(directory) / "release-manifest.json"
        path.write_bytes(manifest_fetch.body)
        verifier(path, version, resolved_tag, commit)

    created = _parse_time(manifest.get("created_at"), "created_at")
    expires = _parse_time(manifest.get("expires_at"), "expires_at")
    if created > now or expires <= now or expires <= created:
        raise VerificationError("release manifest is stale or not yet valid")
    if expires - created != release_identity.FRESHNESS_DURATION:
        raise VerificationError("release manifest freshness duration is noncanonical")

    ref_url = GITHUB_API + "/git/ref/tags/" + quote(resolved_tag, safe="")
    _, ref = _fetch_json(transport, ref_url, {"https://api.github.com"}, "tag ref")
    ref_object = ref.get("object") if isinstance(ref, dict) else None
    if not isinstance(ref_object, dict) or ref_object.get("type") != "tag":
        raise VerificationError("release tag must be annotated")
    tag_object_id = ref_object.get("sha")
    try:
        release_identity.validate_commit(tag_object_id)
    except (TypeError, ValueError) as error:
        raise VerificationError("release tag object identity is malformed") from error
    tag_object_url = GITHUB_API + "/git/tags/" + tag_object_id
    _, tag_object = _fetch_json(transport, tag_object_url, {"https://api.github.com"}, "annotated tag")
    peeled = tag_object.get("object") if isinstance(tag_object, dict) else None
    if not isinstance(peeled, dict) or peeled.get("type") != "commit" or peeled.get("sha") != commit:
        raise VerificationError("release tag and manifest commit do not match")

    stable_fetch = _checked_fetch(
        transport, asset_map["stable.json"]["browser_download_url"],
        max_bytes=METADATA_LIMIT, allowed_origins=GITHUB_DOWNLOAD_ORIGINS, require_exact_final=False,
    )
    stable = _strict_json(stable_fetch.body, "stable pointer")
    if len(stable_fetch.body) != asset_map["stable.json"]["size"]:
        raise VerificationError("stable pointer size disagrees with public asset metadata")
    manifest_url = base + "/release-manifest.json"
    if stable != {"channel": "stable", "manifests": [manifest_url]}:
        raise VerificationError("stable pointer is not exactly bound to the release manifest")

    report = {
        "schema": "tapid-public-release-verification-v1",
        "mode": mode,
        "verified_at": now.replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "workflow": {"run_id": None, "run_url": None},
        "release": {
            "tag": resolved_tag,
            "version": version,
            "commit": commit,
            "state": "public-stable",
            "assets": sorted(
                ({"name": item["name"], "size": item["size"],
                  "url": item["browser_download_url"]} for item in assets),
                key=lambda item: item["name"],
            ),
        },
        "metadata": {
            "release": _evidence(release_fetch),
            "manifest": _evidence(manifest_fetch),
            "stable": _evidence(stable_fetch),
            "signature": "verified-production-rust",
            "freshness": "valid",
        },
        "archives": [],
        "website": {"status": "not-checked"},
    }
    return report, manifest


def verify_release_metadata(*, mode: str, tag: Optional[str], transport=None,
                            verifier: Optional[Callable] = None, now=None):
    """Resolve and verify public release metadata without exposing internal context."""
    report, _ = _verify_release_metadata_context(
        mode=mode, tag=tag, transport=transport, verifier=verifier, now=now,
    )
    return report


def _archive_member(path: Path, expected: str) -> str:
    try:
        with tarfile.open(path, mode="r|gz") as archive:
            members = iter(archive)
            first = next(members, None)
            if first is not None and first.size > ARCHIVE_UNPACKED_LIMIT:
                raise VerificationError("release archive member exceeds unpacked size limit")
            second = next(members, None)
    except (tarfile.TarError, OSError) as error:
        raise VerificationError("release archive is malformed") from error
    if first is None or second is not None:
        raise VerificationError("release archive must contain exactly one member")
    name = first.name
    if name != expected or name.startswith(("/", "\\")) or "\\" in name or ".." in name.split("/"):
        raise VerificationError("release archive member is unsafe or unexpected")
    if not first.isfile() or first.issym() or first.islnk():
        raise VerificationError("release archive member must be a regular file")
    return name


def verify_downloaded_release_assets(directory: Path, *, version: str, tag: str,
                                     commit: str, verifier: Optional[Callable] = None,
                                     now=None):
    """Verify the exact bytes downloaded from a draft release before promotion."""
    release_identity.validate_version_tag(version, tag)
    release_identity.validate_commit(commit)
    root = Path(directory)
    if not root.is_dir() or root.is_symlink():
        raise VerificationError("downloaded draft asset directory is invalid")

    expected_pairs = release_identity.release_archives(version)
    expected_names = {name for _, name in expected_pairs} | {
        "release-manifest.json", "stable.json",
    }
    entries = list(root.iterdir())
    if (len(entries) != 8 or {entry.name for entry in entries} != expected_names or
            any(not entry.is_file() or entry.is_symlink() for entry in entries)):
        raise VerificationError("downloaded draft must contain the exact canonical eight-asset set")

    manifest_path = root / "release-manifest.json"
    stable_path = root / "stable.json"
    if manifest_path.stat().st_size > METADATA_LIMIT or stable_path.stat().st_size > METADATA_LIMIT:
        raise VerificationError("downloaded draft metadata exceeds byte limit")
    manifest = _strict_json(manifest_path.read_bytes(), "release manifest")
    if not isinstance(manifest, dict):
        raise VerificationError("release manifest must be an object")
    if (manifest.get("version") != version or manifest.get("tag") != tag or
            manifest.get("commit") != commit):
        raise VerificationError("downloaded manifest identity does not match approved release")

    (verifier or _default_verifier)(manifest_path, version, tag, commit)
    clock = now or datetime.now(timezone.utc)
    if clock.tzinfo is None or clock.utcoffset() is None:
        raise VerificationError("verification clock must be timezone-aware")
    clock = clock.astimezone(timezone.utc)
    created = _parse_time(manifest.get("created_at"), "created_at")
    expires = _parse_time(manifest.get("expires_at"), "expires_at")
    if (created > clock or expires <= clock or expires <= created or
            expires - created != release_identity.FRESHNESS_DURATION):
        raise VerificationError("release manifest freshness is invalid")

    manifest_url = "https://github.com/{}/releases/download/{}/release-manifest.json".format(
        REPOSITORY, tag
    )
    stable = _strict_json(stable_path.read_bytes(), "stable pointer")
    if stable != {"channel": "stable", "manifests": [manifest_url]}:
        raise VerificationError("stable pointer is not exactly bound to the release manifest")

    artifacts = manifest.get("artifacts")
    required_fields = {"name", "target", "url", "sha256", "size"}
    if not isinstance(artifacts, list) or len(artifacts) != len(expected_pairs):
        raise VerificationError("manifest must describe exactly six archives")
    by_target = {}
    for artifact in artifacts:
        if (not isinstance(artifact, dict) or set(artifact) != required_fields or
                artifact.get("target") in by_target):
            raise VerificationError("manifest archive entry is invalid or duplicated")
        by_target[artifact["target"]] = artifact
    if set(by_target) != {target for target, _ in expected_pairs}:
        raise VerificationError("manifest archive targets are not canonical")

    archive_evidence = []
    base = "https://github.com/{}/releases/download/{}".format(REPOSITORY, tag)
    for target, name in expected_pairs:
        artifact = by_target[target]
        path = root / name
        size = path.stat().st_size
        if size < 1 or size > ARCHIVE_LIMIT:
            raise VerificationError("downloaded archive exceeds size bounds")
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if (artifact.get("name") != name or artifact.get("url") != base + "/" + name or
                artifact.get("size") != size or
                artifact.get("sha256") != digest):
            raise VerificationError("downloaded archive does not match signed manifest")
        member = _archive_member(path, "tapid.exe" if "windows" in target else "tapid")
        archive_evidence.append({
            "target": target, "name": name, "size": size,
            "sha256": digest, "member": member,
        })

    return {
        "asset_count": len(entries), "signature": "verified-production-rust",
        "freshness": "valid", "stable": "verified", "archives": archive_evidence,
    }


def _verify_archives(report, transport, manifest):
    tag = report["release"]["tag"]
    version = report["release"]["version"]
    base = "https://github.com/{}/releases/download/{}".format(REPOSITORY, tag)
    artifacts = manifest.get("artifacts") if isinstance(manifest, dict) else None
    expected_pairs = release_identity.release_archives(version)
    if not isinstance(artifacts, list) or len(artifacts) != len(expected_pairs):
        raise VerificationError("manifest must describe exactly six archives")
    by_target = {}
    required_fields = {"name", "target", "url", "sha256", "size"}
    for artifact in artifacts:
        if not isinstance(artifact, dict) or set(artifact) != required_fields:
            raise VerificationError("manifest archive entry has invalid fields")
        target = artifact.get("target")
        if target in by_target:
            raise VerificationError("manifest contains a duplicate archive target")
        by_target[target] = artifact
    if set(by_target) != {target for target, _ in expected_pairs}:
        raise VerificationError("manifest archive targets are not canonical")

    evidence = []
    release_assets = {item["name"]: item for item in report["release"]["assets"]}
    with tempfile.TemporaryDirectory(prefix="tapid-public-archives-") as directory:
        root = Path(directory)
        for target, name in expected_pairs:
            artifact = by_target[target]
            url = base + "/" + name
            size = artifact.get("size")
            digest = artifact.get("sha256")
            if release_assets[name]["size"] != size:
                raise VerificationError("release asset size disagrees with signed manifest")
            if (artifact.get("name") != name or artifact.get("url") != url or
                    not isinstance(size, int) or isinstance(size, bool) or size < 1 or size > ARCHIVE_LIMIT or
                    not isinstance(digest, str) or len(digest) != 64 or
                    any(char not in "0123456789abcdef" for char in digest) or
                    release_assets[name]["url"] != url):
                raise VerificationError("manifest archive identity is invalid")
            fetched = _checked_fetch(
                transport, url, max_bytes=ARCHIVE_LIMIT,
                allowed_origins=GITHUB_DOWNLOAD_ORIGINS, require_exact_final=False,
            )
            path = root / name
            path.write_bytes(fetched.body)
            actual_size = path.stat().st_size
            actual_digest = hashlib.sha256(path.read_bytes()).hexdigest()
            if actual_size != size:
                raise VerificationError("release archive size does not match signed manifest")
            if actual_digest != digest:
                raise VerificationError("release archive digest does not match signed manifest")
            member = _archive_member(path, "tapid.exe" if "windows" in target else "tapid")
            evidence.append({
                "target": target, "name": name, "url": _redacted_url(fetched.final_url),
                "size": actual_size, "sha256": actual_digest, "member": member,
            })
    return evidence


def _rendered_text(body: bytes) -> str:
    class TextParser(html.parser.HTMLParser):
        def __init__(self):
            super().__init__(convert_charrefs=True)
            self.parts = []
            self.hidden = 0

        def handle_starttag(self, tag, attrs):
            if tag in ("script", "style", "template"):
                self.hidden += 1
            elif tag in ("address", "article", "aside", "blockquote", "br", "div", "footer",
                         "h1", "h2", "h3", "h4", "h5", "h6", "header", "li", "main",
                         "nav", "p", "pre", "section", "table", "td", "th", "tr"):
                self.parts.append(" ")

        def handle_endtag(self, tag):
            if tag in ("script", "style", "template") and self.hidden:
                self.hidden -= 1
            elif tag in ("address", "article", "aside", "blockquote", "div", "footer",
                         "h1", "h2", "h3", "h4", "h5", "h6", "header", "li", "main",
                         "nav", "p", "pre", "section", "table", "td", "th", "tr"):
                self.parts.append(" ")

        def handle_data(self, data):
            if not self.hidden:
                self.parts.append(data)

    try:
        source = body.decode("utf-8")
    except UnicodeError as error:
        raise WebsiteVerificationError("website page is not UTF-8") from error
    parser = TextParser()
    parser.feed(source)
    return " ".join("".join(parser.parts).split())


def _verify_website_once(report, transport, repository_root: Path):
    installer_urls = {
        "install.sh": "https://tapid.dev/install.sh",
        "install.ps1": "https://tapid.dev/install.ps1",
    }
    installers = {}
    for name, url in installer_urls.items():
        fetched = _checked_fetch(
            transport, url, max_bytes=METADATA_LIMIT,
            allowed_origins={"https://tapid.dev"},
        )
        canonical = (repository_root / "scripts" / name).read_bytes()
        if fetched.body != canonical:
            raise WebsiteVerificationError("public website installer differs from canonical " + name)
        installers[name] = _evidence(fetched)

    tag = report["release"]["tag"]
    version = report["release"]["version"]
    install_commands = (
        "curl -fsSL https://tapid.dev/install.sh | bash",
        "iwr -useb https://tapid.dev/install.ps1 | iex",
    )
    page_contracts = {
        "homepage": (
            "https://tapid.dev/",
            ("Package management with clear evidence.",
             "Tapid is a JavaScript and TypeScript package manager"),
        ),
        "getting-started": (
            "https://tapid.dev/docs/getting-started/",
            ("Getting started", "Tapid", "package manager") + install_commands,
        ),
        "release": (
            "https://tapid.dev/docs/releases/" + version + "/",
            ("Tapid " + version, tag) + install_commands,
        ),
    }
    pages = {}
    for label, (url, required) in page_contracts.items():
        fetched = _checked_fetch(
            transport, url, max_bytes=2 * METADATA_LIMIT,
            allowed_origins={"https://tapid.dev"},
        )
        text = _rendered_text(fetched.body)
        if any(fragment not in text for fragment in required):
            raise WebsiteVerificationError(label + " rendered content is missing release documentation")
        pages[label] = _evidence(fetched)
    return installers, pages


def _verify_website(report, transport, repository_root: Path, sleep: Callable,
                    attempts: int = 4, delay_seconds: int = 5):
    last_error = None
    for attempt in range(1, attempts + 1):
        try:
            installers, pages = _verify_website_once(report, transport, repository_root)
            return {
                "status": "verified", "attempts": attempt,
                "installers": installers, "pages": pages,
            }
        except (VerificationError, OSError) as error:
            last_error = error
            if attempt < attempts:
                sleep(delay_seconds)
    raise WebsiteVerificationError(
        "website verification failed after {} attempts: {}".format(attempts, last_error)
    ) from last_error


def verify_public_release(*, mode: str, tag: Optional[str], transport=None,
                          verifier: Optional[Callable] = None, now=None,
                          verify_website: bool = True, repository_root: Optional[Path] = None,
                          sleep: Optional[Callable] = None):
    """Verify release metadata, every archive, and optionally website delivery."""
    transport = transport or PublicTransport()
    report, manifest = _verify_release_metadata_context(
        mode=mode, tag=tag, transport=transport, verifier=verifier, now=now,
    )
    report["archives"] = _verify_archives(report, transport, manifest)
    if verify_website:
        root = Path(repository_root) if repository_root is not None else Path(__file__).resolve().parents[2]
        report["website"] = _verify_website(report, transport, root, sleep or time.sleep)
    return report


def _parser():
    parser = argparse.ArgumentParser(
        description="Verify a Tapid public release without authentication and emit JSON evidence."
    )
    parser.add_argument("--mode", required=True, choices=("stable", "tag"))
    parser.add_argument("--tag", help="explicit v0.x.y tag; required only in tag mode")
    parser.add_argument("--output", type=Path, help="also atomically write evidence to this path")
    parser.add_argument("--skip-website", action="store_true", help="verify release assets only")
    parser.add_argument("--repository-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--workflow-run-id")
    parser.add_argument("--workflow-run-url")
    return parser


def main(argv=None, *, verify=verify_public_release):
    """Stable workflow-facing CLI entry point."""
    parser = _parser()
    args = parser.parse_args(argv)
    if args.mode == "tag" and args.tag is None:
        parser.error("--tag is required in tag mode")
    if args.mode == "stable" and args.tag is not None:
        parser.error("--tag is forbidden in stable mode")
    report = verify(
        mode=args.mode, tag=args.tag, verify_website=not args.skip_website,
        repository_root=args.repository_root,
    )
    if "workflow" in report:
        report["workflow"] = {
            "run_id": args.workflow_run_id,
            "run_url": _redacted_url(args.workflow_run_url) if args.workflow_run_url else None,
        }
    encoded = (json.dumps(report, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=args.output.parent, prefix="." + args.output.name + ".",
                                         delete=False) as handle:
            temporary = Path(handle.name)
            handle.write(encoded)
        os.replace(temporary, args.output)
    print(encoded.decode("utf-8"), end="")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except VerificationError as error:
        print(json.dumps({"schema": "tapid-public-release-verification-error-v1",
                          "error": str(error)}, sort_keys=True, separators=(",", ":")),
              file=sys.stderr)
        raise SystemExit(2)
