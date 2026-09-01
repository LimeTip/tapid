"""Read-only Cargo and crates.io adapters for publication planning."""

import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
from pathlib import Path
from urllib import error, parse, request


_API_ROOT = "https://crates.io/api/v1/crates"
_PACKAGE_ENVIRONMENT_ALLOWLIST = {
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "LANG",
    "NO_PROXY",
    "PATH",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "SSL_CERT_FILE",
    "TMPDIR",
}
_SEMVER_CORE_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
_SEMVER_IDENTIFIER_RE = re.compile(r"^[0-9A-Za-z-]+$")
_CARGO_VERSION_RE = re.compile(r"^cargo (0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)\b")


class RepositoryError(RuntimeError):
    """A local Cargo observation failed or was malformed."""


class RegistryError(RuntimeError):
    """A classified crates.io read failure."""

    def __init__(self, kind, message):
        super().__init__(message)
        self.kind = kind


def package_content_sha256(archive):
    """Hash package contents while excluding Cargo's commit-only metadata."""
    digest = hashlib.sha256()
    seen = set()
    roots = set()
    try:
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:*") as package:
            for member in sorted(package.getmembers(), key=lambda item: item.name):
                parts = member.name.split("/")
                if len(parts) < 2 or not parts[0] or any(part in ("", ".", "..") for part in parts):
                    raise RepositoryError("crate archive contains an invalid member path")
                roots.add(parts[0])
                if member.name in seen:
                    raise RepositoryError("crate archive contains duplicate members")
                seen.add(member.name)
                cargo_vcs_info = len(parts) == 2 and parts[1] == ".cargo_vcs_info.json"
                generated_lock = len(parts) == 2 and parts[1] == "Cargo.lock"
                if generated_lock:
                    continue
                if member.isfile():
                    extracted = package.extractfile(member)
                    if extracted is None:
                        raise RepositoryError("crate archive member is unreadable")
                    payload = extracted.read()
                    if cargo_vcs_info:
                        try:
                            metadata = json.loads(payload)
                            commit = metadata["git"]["sha1"]
                            if (
                                not isinstance(metadata, dict)
                                or not isinstance(metadata["git"], dict)
                                or not isinstance(commit, str)
                                or not re.fullmatch(r"[0-9a-f]{40}", commit)
                            ):
                                raise ValueError
                            metadata["git"]["sha1"] = "0" * 40
                            payload = json.dumps(
                                metadata,
                                sort_keys=True,
                                separators=(",", ":"),
                            ).encode("utf-8")
                        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
                            raise RepositoryError(
                                "crate archive contains malformed Cargo VCS metadata"
                            ) from error
                    kind = b"file"
                elif member.isdir():
                    payload = b""
                    kind = b"directory"
                elif member.issym():
                    payload = member.linkname.encode("utf-8")
                    kind = b"symlink"
                else:
                    raise RepositoryError("crate archive contains an unsupported member type")
                for value in (
                    member.name.encode("utf-8"),
                    kind,
                    str(member.mode & 0o777).encode("ascii"),
                    payload,
                ):
                    digest.update(len(value).to_bytes(8, "big"))
                    digest.update(value)
    except (tarfile.TarError, UnicodeError) as error:
        raise RepositoryError("crate archive is malformed") from error
    if len(roots) != 1 or not seen:
        raise RepositoryError("crate archive must contain one package root")
    return digest.hexdigest()


def _run_checked(command, *, run, **kwargs):
    result = run(command, text=True, capture_output=True, **kwargs)
    if result.returncode != 0:
        detail = (result.stderr or "").strip()
        raise RepositoryError(
            "command failed ({}): {}".format(result.returncode, detail or command[0])
        )
    return result


def _require_workspace_packaging_cargo(*, run, cwd, env):
    """Require stabilized Cargo workspace packaging support."""
    result = _run_checked(["cargo", "--version"], run=run, cwd=cwd, env=env)
    matched = _CARGO_VERSION_RE.match((result.stdout or "").strip())
    if matched is None:
        raise RepositoryError("malformed cargo --version output")
    version = tuple(int(component) for component in matched.groups())
    if version < (1, 89, 0):
        raise RepositoryError(
            "Cargo 1.89 or newer is required for pre-publication workspace packaging"
        )


def read_locked_metadata(workspace, *, run=subprocess.run):
    """Read the workspace graph without changing dependency resolution."""
    result = _run_checked(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        run=run,
        cwd=workspace,
    )
    try:
        metadata = json.loads(result.stdout)
        if not isinstance(metadata, dict):
            raise ValueError
        if not isinstance(metadata["packages"], list):
            raise ValueError
        if not isinstance(metadata["workspace_members"], list):
            raise ValueError
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
        raise RepositoryError("malformed Cargo metadata") from exc
    return metadata


def _default_http_get(url, headers, timeout):
    incoming = request.Request(url, headers=headers, method="GET")
    try:
        with request.urlopen(incoming, timeout=timeout) as response:
            return response.status, response.read(), dict(response.headers.items())
    except error.HTTPError as exc:
        return exc.code, exc.read(), dict(exc.headers.items())


class CratesIoClient:
    """Bounded, unauthenticated client for the public crates.io API."""

    def __init__(
        self,
        *,
        http_get=_default_http_get,
        sleep=time.sleep,
        max_attempts=3,
        timeout=10,
    ):
        if not isinstance(max_attempts, int) or not 1 <= max_attempts <= 5:
            raise ValueError("max_attempts must be between one and five")
        self._http_get = http_get
        self._sleep = sleep
        self._max_attempts = max_attempts
        self._timeout = timeout

    def versions(self, crate_name):
        """Return deterministic version/checksum observations for one crate."""
        url = _API_ROOT + "/" + parse.quote(crate_name, safe="")
        headers = {
            "Accept": "application/json",
            "User-Agent": "tapid-release-planner/1 (https://github.com/LimeTip/tapid)",
        }
        last_kind = "transient-network"
        for attempt in range(self._max_attempts):
            try:
                status, body, response_headers = self._http_get(
                    url, headers, self._timeout
                )
            except (OSError, error.URLError) as exc:
                status, body, response_headers = 0, b"", {}
                last_kind = "transient-network"
                last_error = exc
            else:
                last_error = None
            if status == 404:
                return []
            if status in (401, 403):
                raise RegistryError("authorization", "crates.io rejected a read-only request")
            if status == 429:
                last_kind = "rate-limit"
            elif status == 0 or 500 <= status <= 599:
                last_kind = "transient-network"
            elif status != 200:
                raise RegistryError(
                    "unexpected-status", f"crates.io returned unexpected HTTP {status}"
                )
            else:
                return self._parse_versions(body)

            if attempt + 1 < self._max_attempts:
                delay = 2 ** attempt
                if status == 429:
                    try:
                        delay = min(5.0, max(0.0, float(response_headers.get("Retry-After", delay))))
                    except (TypeError, ValueError):
                        pass
                self._sleep(float(delay))
        message = (
            "crates.io request failed after bounded retries"
            if last_error is None else
            "crates.io network request failed after bounded retries"
        )
        raise RegistryError(last_kind, message)

    def archive(self, crate_name, version, expected_checksum):
        """Download exact published bytes and verify them against index metadata."""
        url = "{}/{}/{}/download".format(
            _API_ROOT,
            parse.quote(crate_name, safe=""),
            parse.quote(version, safe=""),
        )
        headers = {
            "Accept": "application/octet-stream",
            "User-Agent": "tapid-release-planner/1 (https://github.com/LimeTip/tapid)",
        }
        last_kind = "transient-network"
        for attempt in range(self._max_attempts):
            try:
                status, body, response_headers = self._http_get(url, headers, self._timeout)
            except (OSError, error.URLError):
                status, body, response_headers = 0, b"", {}
            if status == 429:
                last_kind = "rate-limit"
            elif status == 0 or 500 <= status <= 599:
                last_kind = "transient-network"
            elif status in (401, 403):
                raise RegistryError("authorization", "crates.io rejected an archive request")
            elif status != 200:
                raise RegistryError(
                    "unexpected-status",
                    f"crates.io returned unexpected archive HTTP {status}",
                )
            else:
                if hashlib.sha256(body).hexdigest() != expected_checksum:
                    raise RegistryError(
                        "checksum-mismatch",
                        "downloaded crate archive differs from crates.io checksum metadata",
                    )
                return body
            if attempt + 1 < self._max_attempts:
                delay = 2 ** attempt
                if status == 429:
                    try:
                        delay = min(5.0, max(0.0, float(response_headers.get("Retry-After", delay))))
                    except (TypeError, ValueError):
                        pass
                self._sleep(float(delay))
        raise RegistryError(last_kind, "crates.io archive request failed after bounded retries")

    @staticmethod
    def _parse_versions(body):
        try:
            payload = json.loads(body)
            versions = payload["versions"]
            if not isinstance(versions, list):
                raise ValueError
            result = []
            for version in versions:
                number = version["num"]
                checksum = version["checksum"]
                if not isinstance(number, str) or not isinstance(checksum, str):
                    raise ValueError
                if not _valid_semver(number):
                    raise ValueError
                if len(checksum) != 64 or any(character not in "0123456789abcdef" for character in checksum):
                    raise ValueError
                result.append({"version": number, "checksum": checksum})
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
            raise RegistryError(
                "malformed-response", "crates.io returned malformed version metadata"
            ) from exc
        return sorted(result, key=lambda item: (item["version"], item["checksum"]))


def package_crate(
    name,
    version,
    workspace,
    output_directory,
    *,
    run=subprocess.run,
    cargo_home=None,
):
    """Run verified Cargo packaging in an isolated target and hash its archive."""
    output_directory = Path(output_directory)
    output_directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix="cargo-package-{}-".format(name), dir=output_directory
    ) as isolated:
        environment = {
            key: value
            for key, value in os.environ.items()
            if key in _PACKAGE_ENVIRONMENT_ALLOWLIST
        }
        safe_cargo_home = Path(cargo_home) if cargo_home else Path(isolated) / "cargo-home"
        environment["HOME"] = str(safe_cargo_home.parent / "home")
        environment["CARGO_HOME"] = str(safe_cargo_home)
        environment["CARGO_TARGET_DIR"] = isolated
        _run_checked(
            [
                "cargo",
                "package",
                "--manifest-path",
                str(Path(workspace).resolve() / "Cargo.toml"),
                "-p",
                name,
                "--locked",
            ],
            run=run,
            cwd=Path(isolated),
            env=environment,
        )
        generated = Path(isolated) / "package" / "{}-{}.crate".format(name, version)
        if not generated.is_file():
            raise RepositoryError(
                "cargo package did not create the expected archive for {} {}".format(
                    name, version
                )
            )
        destination = output_directory / generated.name
        shutil.copyfile(generated, destination)
    archive = destination.read_bytes()
    return {
        "archive_path": str(destination),
        "archive_sha256": hashlib.sha256(archive).hexdigest(),
        "archive_size": len(archive),
    }


def package_workspace(
    metadata,
    workspace,
    output_directory,
    *,
    run=subprocess.run,
    cargo_home=None,
):
    """Package and hash every workspace archive before registry mutation.

    Registry-backed Cargo verification is intentionally deferred until the
    dependency-ordered versions are visible on crates.io.
    """
    output_directory = Path(output_directory)
    output_directory.mkdir(parents=True, exist_ok=True)
    members = set(metadata["workspace_members"])
    packages = sorted(
        (
            item
            for item in metadata["packages"]
            if item["id"] in members and _is_crates_io_publishable(item)
        ),
        key=lambda item: item["name"],
    )
    if len({item["name"] for item in packages}) != len(packages):
        raise RepositoryError("Cargo metadata contains duplicate publishable package names")

    with tempfile.TemporaryDirectory(
        prefix="cargo-package-workspace-", dir=output_directory
    ) as isolated:
        environment = {
            key: value
            for key, value in os.environ.items()
            if key in _PACKAGE_ENVIRONMENT_ALLOWLIST
        }
        safe_cargo_home = Path(cargo_home) if cargo_home else Path(isolated) / "cargo-home"
        environment["HOME"] = str(safe_cargo_home.parent / "home")
        environment["CARGO_HOME"] = str(safe_cargo_home)
        environment["CARGO_TARGET_DIR"] = isolated
        _require_workspace_packaging_cargo(
            run=run,
            cwd=Path(isolated),
            env=environment,
        )
        _run_checked(
            [
                "cargo",
                "package",
                "--manifest-path",
                str(Path(workspace).resolve() / "Cargo.toml"),
                "--workspace",
                "--locked",
                "--no-verify",
            ],
            run=run,
            cwd=Path(isolated),
            env=environment,
        )

        packaged = {}
        for package in packages:
            name = package["name"]
            version = package["version"]
            generated = Path(isolated) / "package" / "{}-{}.crate".format(name, version)
            if not generated.is_file():
                raise RepositoryError(
                    "cargo package did not create the expected archive for {} {}".format(
                        name, version
                    )
                )
            destination = output_directory / generated.name
            shutil.copyfile(generated, destination)
            archive = destination.read_bytes()
            packaged[name] = {
                "archive_path": str(destination),
                "archive_sha256": hashlib.sha256(archive).hexdigest(),
                "archive_size": len(archive),
            }
    return packaged


def collect_package_evidence(metadata, registry, package_adapter):
    """Collect deterministic registry and verified archive evidence."""
    members = set(metadata["workspace_members"])
    packages = sorted(
        (
            item for item in metadata["packages"]
            if item["id"] in members and _is_crates_io_publishable(item)
        ),
        key=lambda item: item["name"],
    )
    observations = {}
    for package in packages:
        name = package["name"]
        version = package["version"]
        packaged = package_adapter(name, version)
        registry_versions = registry.versions(name)
        observation = {
            "archive_sha256": packaged["archive_sha256"],
            "archive_size": packaged["archive_size"],
            "registry_versions": registry_versions,
        }
        archive_path = packaged.get("archive_path")
        matching = [item for item in registry_versions if item["version"] == version]
        if archive_path is not None:
            local_archive = Path(archive_path).read_bytes()
            if hashlib.sha256(local_archive).hexdigest() != packaged["archive_sha256"]:
                raise RepositoryError("packaged archive checksum changed during evidence collection")
            observation["archive_content_sha256"] = package_content_sha256(local_archive)
        if len(matching) == 1 and hasattr(registry, "archive"):
            published_archive = registry.archive(name, version, matching[0]["checksum"])
            observation["published_archive_sha256"] = hashlib.sha256(
                published_archive
            ).hexdigest()
            observation["published_archive_size"] = len(published_archive)
            observation["published_content_sha256"] = package_content_sha256(
                published_archive
            )
        observations[name] = observation
    return observations


def _is_crates_io_publishable(package):
    registries = package.get("publish")
    return registries is None or "crates-io" in registries


def _valid_semver(version):
    without_build, build_separator, build = version.partition("+")
    core, prerelease_separator, prerelease = without_build.partition("-")
    if not _SEMVER_CORE_RE.fullmatch(core):
        return False
    if build_separator and not _valid_identifiers(build, allow_numeric_zeroes=True):
        return False
    return not prerelease_separator or _valid_identifiers(
        prerelease, allow_numeric_zeroes=False
    )


def _valid_identifiers(value, *, allow_numeric_zeroes):
    return bool(value) and all(
        _SEMVER_IDENTIFIER_RE.fullmatch(identifier)
        and (
            allow_numeric_zeroes
            or not identifier.isdigit()
            or identifier == "0"
            or not identifier.startswith("0")
        )
        for identifier in value.split(".")
    )
