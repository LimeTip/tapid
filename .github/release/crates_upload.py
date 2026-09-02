"""Prepare and upload exact reviewed crates.io archive bytes."""

import hashlib
import http.client
import gzip
import io
import json
import os
import stat
import struct
import tarfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from urllib import parse

try:
    import tomllib
except ImportError as error:  # pragma: no cover - workflow requires Python 3.11+
    raise RuntimeError("exact crate upload requires Python 3.11 or newer") from error


MAX_METADATA_BYTES = 1024 * 1024
MAX_ARCHIVE_BYTES = 10 * 1024 * 1024
MAX_UNPACKED_BYTES = 64 * 1024 * 1024
MAX_ARCHIVE_MEMBERS = 10_000
MAX_RESPONSE_BYTES = 64 * 1024
CRATES_IO_UPLOAD_URL = "https://crates.io/api/v1/crates/new"
USER_AGENT = "tapid-release-automation/1 crates.io-exact-upload"
_METADATA_FIELDS = {
    "authors",
    "badges",
    "categories",
    "deps",
    "description",
    "documentation",
    "features",
    "homepage",
    "keywords",
    "license",
    "license_file",
    "links",
    "name",
    "readme",
    "readme_file",
    "repository",
    "rust_version",
    "vers",
}


class UploadError(RuntimeError):
    """Exact archive preparation or publication failed closed."""


class UnknownPublicationState(UploadError):
    """The connection failed after an upload may have been transmitted."""


@dataclass(frozen=True)
class ParsedUpload:
    metadata: dict
    metadata_bytes: bytes
    archive: bytes


@dataclass(frozen=True)
class PreparedUpload:
    metadata: dict
    body: bytes
    body_sha256: str
    archive_sha256: str
    archive_size: int


def _optional_string(mapping, key):
    value = mapping.get(key)
    if value is not None and not isinstance(value, str):
        raise UploadError("reviewed manifest {} must be a string".format(key))
    return value


def _string_list(mapping, key):
    value = mapping.get(key, [])
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise UploadError("reviewed manifest {} must be a string list".format(key))
    return value


def _badges(manifest):
    value = manifest.get("badges", {})
    if not isinstance(value, dict) or any(
        not isinstance(name, str)
        or not isinstance(settings, dict)
        or any(
            not isinstance(key, str) or not isinstance(item, str)
            for key, item in settings.items()
        )
        for name, settings in value.items()
    ):
        raise UploadError("reviewed manifest badges are invalid")
    return value


def _dependency(alias, specification, kind, target):
    if isinstance(specification, str):
        specification = {"version": specification}
    if not isinstance(alias, str) or not alias or not isinstance(specification, dict):
        raise UploadError("reviewed manifest dependency is invalid")
    if any(key in specification for key in ("git", "path", "registry")):
        raise UploadError(
            "reviewed packaged dependency must resolve to the crates.io registry"
        )
    if any(key in specification for key in ("artifact", "lib", "target")):
        raise UploadError("reviewed artifact dependencies are unsupported")
    version = specification.get("version")
    package_name = specification.get("package", alias)
    if not isinstance(version, str) or not version or not isinstance(package_name, str):
        raise UploadError("reviewed manifest dependency identity is invalid")
    features = specification.get("features", [])
    optional = specification.get("optional", False)
    default_features = specification.get("default-features", True)
    if not isinstance(features, list) or any(
        not isinstance(feature, str) for feature in features
    ):
        raise UploadError("reviewed manifest dependency features are invalid")
    if not isinstance(optional, bool) or not isinstance(default_features, bool):
        raise UploadError("reviewed manifest dependency flags are invalid")
    dependency = {
        "optional": optional,
        "default_features": default_features,
        "name": package_name,
        "features": features,
        "version_req": version,
        "target": target,
        "kind": kind,
    }
    if package_name != alias:
        dependency["explicit_name_in_toml"] = alias
    return dependency


def _dependency_section(manifest, section_name, kind, target=None):
    section = manifest.get(section_name, {})
    if not isinstance(section, dict):
        raise UploadError("reviewed manifest dependency section is invalid")
    return [
        _dependency(alias, section[alias], kind, target)
        for alias in sorted(section)
    ]


def _dependencies(manifest):
    dependencies = []
    for section_name, kind in (
        ("dependencies", "normal"),
        ("build-dependencies", "build"),
        ("dev-dependencies", "dev"),
    ):
        dependencies.extend(_dependency_section(manifest, section_name, kind))
    targets = manifest.get("target", {})
    if not isinstance(targets, dict):
        raise UploadError("reviewed manifest target dependencies are invalid")
    for target in sorted(targets):
        target_sections = targets[target]
        if not isinstance(target, str) or not isinstance(target_sections, dict):
            raise UploadError("reviewed manifest target dependencies are invalid")
        for section_name, kind in (
            ("dependencies", "normal"),
            ("build-dependencies", "build"),
            ("dev-dependencies", "dev"),
        ):
            dependencies.extend(
                _dependency_section(target_sections, section_name, kind, target)
            )
    return dependencies


def _features(manifest):
    features = manifest.get("features", {})
    if not isinstance(features, dict) or any(
        not isinstance(name, str)
        or not isinstance(values, list)
        or any(not isinstance(value, str) for value in values)
        for name, values in features.items()
    ):
        raise UploadError("reviewed manifest features are invalid")
    return {name: features[name] for name in sorted(features)}


def _read_regular_file(path):
    path = Path(path)
    try:
        mode = path.lstat().st_mode
    except OSError as error:
        raise UploadError("reviewed archive is unreadable") from error
    if not stat.S_ISREG(mode):
        raise UploadError("reviewed archive must be a regular non-symlink file")
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as source:
            observed = os.fstat(source.fileno())
            if not stat.S_ISREG(observed.st_mode):
                raise UploadError("reviewed archive must be a regular non-symlink file")
            return source.read(MAX_ARCHIVE_BYTES + 1)
    except OSError as error:
        raise UploadError("reviewed archive must be a regular non-symlink file") from error


def _archive_metadata(archive, expected_name, expected_version):
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(archive), mode="rb") as compressed:
            unpacked = compressed.read(MAX_UNPACKED_BYTES + 1)
        if len(unpacked) > MAX_UNPACKED_BYTES:
            raise UploadError("reviewed archive unpacked size is oversized")
        with tarfile.open(fileobj=io.BytesIO(unpacked), mode="r:") as package:
            members = package.getmembers()
            if len(members) > MAX_ARCHIVE_MEMBERS:
                raise UploadError("reviewed archive contains too many members")
            if any(
                member.size < 0 or member.size > MAX_UNPACKED_BYTES
                for member in members
            ):
                raise UploadError("reviewed archive member size is invalid")
            manifests = [
                member for member in members
                if PurePosixPath(member.name).name == "Cargo.toml"
                and len(PurePosixPath(member.name).parts) == 2
            ]
            if len(manifests) != 1:
                raise UploadError("reviewed archive must contain exactly one reviewed manifest")
            manifest_member = manifests[0]
            expected_manifest = "{}-{}/Cargo.toml".format(expected_name, expected_version)
            if manifest_member.name != expected_manifest or not manifest_member.isfile():
                raise UploadError("reviewed archive manifest identity differs from reviewed plan")
            manifest_file = package.extractfile(manifest_member)
            if manifest_file is None:
                raise UploadError("reviewed archive manifest is unreadable")
            manifest_bytes = manifest_file.read(MAX_METADATA_BYTES + 1)
            if len(manifest_bytes) > MAX_METADATA_BYTES:
                raise UploadError("reviewed archive manifest is oversized")
            try:
                manifest = tomllib.loads(manifest_bytes.decode("utf-8"))
            except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
                raise UploadError("reviewed archive manifest is malformed") from error
            package_metadata = manifest.get("package")
            if not isinstance(package_metadata, dict) or (
                package_metadata.get("name") != expected_name
                or package_metadata.get("version") != expected_version
            ):
                raise UploadError("reviewed archive manifest identity differs from reviewed plan")
            readme_setting = package_metadata.get("readme")
            if readme_setting in (None, False):
                readme_file = None
                readme = None
            elif isinstance(readme_setting, str) and readme_setting:
                readme_path = PurePosixPath(readme_setting)
                if readme_path.is_absolute() or ".." in readme_path.parts or "." in readme_path.parts:
                    raise UploadError("reviewed README path must stay within the package root")
                readme_file = readme_setting
                member_name = "{}-{}/{}".format(
                    expected_name, expected_version, readme_path.as_posix()
                )
                readme_members = [item for item in members if item.name == member_name]
                if len(readme_members) != 1 or not readme_members[0].isfile():
                    raise UploadError("reviewed README path is missing or ambiguous")
                readme_source = package.extractfile(readme_members[0])
                if readme_source is None:
                    raise UploadError("reviewed README is unreadable")
                raw_readme = readme_source.read(MAX_METADATA_BYTES + 1)
                if len(raw_readme) > MAX_METADATA_BYTES:
                    raise UploadError("reviewed README is oversized")
                try:
                    readme = raw_readme.decode("utf-8")
                except UnicodeDecodeError as error:
                    raise UploadError("reviewed README is not UTF-8") from error
            else:
                raise UploadError("reviewed README setting is invalid")
    except UploadError:
        raise
    except (EOFError, tarfile.TarError, OSError) as error:
        raise UploadError("reviewed archive is malformed") from error
    description = _optional_string(package_metadata, "description")
    license_expression = _optional_string(package_metadata, "license")
    license_file = _optional_string(package_metadata, "license-file")
    if license_file:
        license_path = PurePosixPath(license_file)
        if (
            license_path.is_absolute()
            or ".." in license_path.parts
            or "." in license_path.parts
        ):
            raise UploadError("reviewed license file path must stay within the package root")
        license_member_name = "{}-{}/{}".format(
            expected_name, expected_version, license_path.as_posix()
        )
        license_members = [item for item in members if item.name == license_member_name]
        if len(license_members) != 1 or not license_members[0].isfile():
            raise UploadError("reviewed license file is missing or ambiguous")
    expected_root = "{}-{}".format(expected_name, expected_version)
    seen_members = set()
    for member in members:
        path = PurePosixPath(member.name)
        if (
            not member.name
            or "\\" in member.name
            or path.is_absolute()
            or len(path.parts) < 2
            or path.parts[0] != expected_root
            or any(part in ("", ".", "..") for part in path.parts)
            or member.name in seen_members
            or not (member.isfile() or member.isdir())
        ):
            raise UploadError("reviewed archive member topology is invalid")
        seen_members.add(member.name)
    if not description:
        raise UploadError("reviewed manifest description is required for crates.io")
    if not license_expression and not license_file:
        raise UploadError("reviewed manifest license or license-file is required for crates.io")
    return {
        "authors": _string_list(package_metadata, "authors"),
        "badges": _badges(manifest),
        "categories": _string_list(package_metadata, "categories"),
        "deps": _dependencies(manifest),
        "description": description,
        "documentation": _optional_string(package_metadata, "documentation"),
        "features": _features(manifest),
        "homepage": _optional_string(package_metadata, "homepage"),
        "keywords": _string_list(package_metadata, "keywords"),
        "license": license_expression,
        "license_file": license_file,
        "links": _optional_string(package_metadata, "links"),
        "name": expected_name,
        "readme": readme,
        "readme_file": readme_file,
        "repository": _optional_string(package_metadata, "repository"),
        "rust_version": _optional_string(package_metadata, "rust-version"),
        "vers": expected_version,
    }


def _u32(value, label):
    if not isinstance(value, int) or value < 0 or value > 0xFFFFFFFF:
        raise UploadError("{} length does not fit Cargo upload framing".format(label))
    return struct.pack("<I", value)


def build_upload_body(metadata, archive):
    metadata_bytes = json.dumps(
        metadata, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode("utf-8")
    if len(metadata_bytes) > MAX_METADATA_BYTES:
        raise UploadError("upload metadata is oversized")
    if len(archive) > MAX_ARCHIVE_BYTES:
        raise UploadError("reviewed archive is oversized")
    return _u32(len(metadata_bytes), "metadata") + metadata_bytes + _u32(
        len(archive), "archive"
    ) + archive


def _valid_optional_string(value):
    return value is None or isinstance(value, str)


def _validate_dependency_metadata(dependency):
    required = {
        "optional",
        "default_features",
        "name",
        "features",
        "version_req",
        "target",
        "kind",
    }
    optional = {
        "registry",
        "explicit_name_in_toml",
        "artifact",
        "bindep_target",
        "lib",
    }
    if not isinstance(dependency, dict) or not required.issubset(dependency) or (
        set(dependency) - required - optional
    ):
        raise UploadError("upload metadata dependency fields are invalid")
    if type(dependency["optional"]) is not bool or type(
        dependency["default_features"]
    ) is not bool:
        raise UploadError("upload metadata dependency flags are invalid")
    if any(
        not isinstance(dependency[field], str) or not dependency[field]
        for field in ("name", "version_req")
    ):
        raise UploadError("upload metadata dependency identity is invalid")
    features = dependency["features"]
    if not isinstance(features, list) or any(
        not isinstance(feature, str) for feature in features
    ):
        raise UploadError("upload metadata dependency features are invalid")
    target = dependency["target"]
    if target is not None and (not isinstance(target, str) or not target):
        raise UploadError("upload metadata dependency target is invalid")
    kind = dependency["kind"]
    if not isinstance(kind, str) or kind not in {"normal", "build", "dev"}:
        raise UploadError("upload metadata dependency kind is invalid")
    for field in ("registry", "explicit_name_in_toml", "bindep_target"):
        if field in dependency and (
            not isinstance(dependency[field], str) or not dependency[field]
        ):
            raise UploadError("upload metadata dependency {} is invalid".format(field))
    if "artifact" in dependency:
        artifact = dependency["artifact"]
        if not isinstance(artifact, list) or not artifact or any(
            not isinstance(kind, str) or not kind for kind in artifact
        ):
            raise UploadError("upload metadata dependency artifact is invalid")
    elif "bindep_target" in dependency or dependency.get("lib") is True:
        raise UploadError("upload metadata dependency artifact context is invalid")
    if "lib" in dependency and dependency["lib"] is not True:
        raise UploadError("upload metadata dependency lib flag is invalid")


def _validate_upload_metadata(metadata):
    if not isinstance(metadata, dict) or set(metadata) != _METADATA_FIELDS:
        raise UploadError("upload metadata fields do not match Cargo's publish contract")
    if any(
        not isinstance(metadata[field], str) or not metadata[field]
        for field in ("name", "vers")
    ):
        raise UploadError("upload metadata identity is invalid")
    if any(
        not _valid_optional_string(metadata[field])
        for field in (
            "description",
            "documentation",
            "homepage",
            "readme",
            "readme_file",
            "license",
            "license_file",
            "repository",
            "links",
            "rust_version",
        )
    ):
        raise UploadError("upload metadata optional string is invalid")
    if not metadata["description"] or not (
        metadata["license"] or metadata["license_file"]
    ):
        raise UploadError("upload metadata lacks required crates.io package metadata")
    for field in ("authors", "keywords", "categories"):
        values = metadata[field]
        if not isinstance(values, list) or any(
            not isinstance(value, str) for value in values
        ):
            raise UploadError("upload metadata {} is invalid".format(field))
    features = metadata["features"]
    if not isinstance(features, dict) or any(
        not isinstance(name, str)
        or not isinstance(values, list)
        or any(not isinstance(value, str) for value in values)
        for name, values in features.items()
    ):
        raise UploadError("upload metadata features are invalid")
    badges = metadata["badges"]
    if not isinstance(badges, dict) or any(
        not isinstance(name, str)
        or not isinstance(settings, dict)
        or any(
            not isinstance(key, str) or not isinstance(value, str)
            for key, value in settings.items()
        )
        for name, settings in badges.items()
    ):
        raise UploadError("upload metadata badges are invalid")
    if not isinstance(metadata["deps"], list):
        raise UploadError("upload metadata dependencies are invalid")
    for dependency in metadata["deps"]:
        _validate_dependency_metadata(dependency)


def parse_upload_body(body):
    """Strictly reparse a complete Cargo publish request body."""
    if not isinstance(body, bytes) or len(body) < 8:
        raise UploadError("upload framing is truncated")
    metadata_length = struct.unpack_from("<I", body, 0)[0]
    if metadata_length > MAX_METADATA_BYTES:
        raise UploadError("upload metadata length is oversized")
    metadata_end = 4 + metadata_length
    if metadata_end + 4 > len(body):
        raise UploadError("upload metadata framing is truncated")
    archive_length = struct.unpack_from("<I", body, metadata_end)[0]
    if archive_length > MAX_ARCHIVE_BYTES:
        raise UploadError("upload archive length is oversized")
    archive_start = metadata_end + 4
    archive_end = archive_start + archive_length
    if archive_end != len(body):
        raise UploadError("upload framing contains truncated or trailing bytes")
    metadata_bytes = body[4:metadata_end]

    def reject_duplicate_keys(pairs):
        parsed = {}
        for key, value in pairs:
            if key in parsed:
                raise UploadError(f"upload metadata contains duplicate key: {key}")
            parsed[key] = value
        return parsed

    try:
        metadata = json.loads(
            metadata_bytes.decode("utf-8"),
            object_pairs_hook=reject_duplicate_keys,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise UploadError("upload metadata is malformed") from error
    _validate_upload_metadata(metadata)
    return ParsedUpload(metadata, metadata_bytes, body[archive_start:archive_end])


def prepare_upload(
    archive_path,
    *,
    expected_name,
    expected_version,
    expected_size,
    expected_sha256,
):
    """Read reviewed bytes once, validate them, frame, then reparse the result."""
    archive = _read_regular_file(archive_path)
    if len(archive) != expected_size:
        raise UploadError("reviewed archive size differs from reviewed plan")
    observed_sha256 = hashlib.sha256(archive).hexdigest()
    if observed_sha256 != expected_sha256:
        raise UploadError("reviewed archive checksum differs from reviewed plan")
    metadata = _archive_metadata(archive, expected_name, expected_version)
    body = build_upload_body(metadata, archive)
    reparsed = parse_upload_body(body)
    if (
        reparsed.metadata != metadata
        or len(reparsed.archive) != expected_size
        or hashlib.sha256(reparsed.archive).hexdigest() != expected_sha256
        or reparsed.archive != archive
    ):
        raise UploadError("locally reparsed upload differs from reviewed archive")
    return PreparedUpload(
        metadata=metadata,
        body=body,
        body_sha256=hashlib.sha256(body).hexdigest(),
        archive_sha256=observed_sha256,
        archive_size=len(archive),
    )


def _connection_factory(scheme, hostname, port, timeout):
    connection_type = (
        http.client.HTTPSConnection if scheme == "https" else http.client.HTTPConnection
    )
    return connection_type(hostname, port=port, timeout=timeout)


def _error_detail(status, body):
    try:
        value = json.loads(body.decode("utf-8"))
        errors = value.get("errors") if isinstance(value, dict) else None
        details = [
            item.get("detail") for item in errors or []
            if isinstance(item, dict) and isinstance(item.get("detail"), str)
        ]
    except (UnicodeDecodeError, json.JSONDecodeError):
        details = []
    if details:
        return "; ".join(details)
    return "crates.io upload returned HTTP {}".format(status)


def upload_prepared(
    prepared,
    *,
    token_reader,
    endpoint=CRATES_IO_UPLOAD_URL,
    timeout=30,
    connection_factory=_connection_factory,
    allow_insecure_endpoint=False,
):
    """Validate a prepared body, then perform exactly one non-redirecting PUT."""
    if not isinstance(prepared, PreparedUpload):
        raise UploadError("prepared upload has invalid type")
    if not isinstance(timeout, (int, float)) or not 0 < timeout <= 120:
        raise UploadError("upload timeout must be positive and bounded")
    try:
        reparsed = parse_upload_body(prepared.body)
    except UploadError as error:
        raise UploadError("prepared upload failed local revalidation") from error
    if (
        hashlib.sha256(prepared.body).hexdigest() != prepared.body_sha256
        or reparsed.metadata != prepared.metadata
        or len(reparsed.archive) != prepared.archive_size
        or hashlib.sha256(reparsed.archive).hexdigest() != prepared.archive_sha256
    ):
        raise UploadError("prepared upload failed local revalidation")

    target = parse.urlsplit(endpoint)
    local_test_endpoint = (
        allow_insecure_endpoint
        and target.scheme == "http"
        and target.hostname in {"127.0.0.1", "localhost", "::1"}
        and target.port is not None
    )
    if (
        (endpoint != CRATES_IO_UPLOAD_URL and not local_test_endpoint)
        or not target.hostname
        or target.username is not None
        or target.password is not None
        or target.query
        or target.fragment
        or target.path != "/api/v1/crates/new"
    ):
        raise UploadError("upload endpoint must be the exact crates.io URL or a local test endpoint")

    token = token_reader()
    if not isinstance(token, str) or not token:
        raise UploadError("trusted-publishing credential is unavailable")
    connection = connection_factory(target.scheme, target.hostname, target.port, timeout)
    headers = {
        "Authorization": token,
        "Content-Type": "application/octet-stream",
        "Accept": "application/json",
        "Content-Length": str(len(prepared.body)),
        "User-Agent": USER_AGENT,
    }
    try:
        # There is deliberately no retry: an exception can happen after transmission.
        connection.request("PUT", target.path, body=prepared.body, headers=headers)
        response = connection.getresponse()
        body = response.read(MAX_RESPONSE_BYTES + 1)
    except (OSError, http.client.HTTPException) as error:
        raise UnknownPublicationState(
            "publication state unknown after the single upload attempt"
        ) from error
    finally:
        connection.close()
    if len(body) > MAX_RESPONSE_BYTES:
        raise UnknownPublicationState(
            "publication state unknown after an oversized registry response"
        )
    if 300 <= response.status < 400:
        raise UploadError("crates.io upload redirect was rejected")
    if response.status >= 500:
        raise UnknownPublicationState(
            "publication state unknown after a crates.io server failure"
        )
    detail = _error_detail(response.status, body)
    try:
        value = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        value = None
    has_errors = isinstance(value, dict) and bool(value.get("errors"))
    if not 200 <= response.status < 300 or has_errors:
        raise UploadError(detail)
    return {"status": response.status, "body_sha256": prepared.body_sha256}
