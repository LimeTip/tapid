"""Read-only Cargo and crates.io adapters for publication planning."""

import hashlib
import json
import os
import re
import shutil
import subprocess
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
        packaged = package_adapter(name, package["version"])
        observations[name] = {
            "archive_sha256": packaged["archive_sha256"],
            "archive_size": packaged["archive_size"],
            "registry_versions": registry.versions(name),
        }
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
