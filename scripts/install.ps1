[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$SourceRef,
    [string]$InstallDir = (Join-Path $HOME ".local\bin"),
    [string]$Repo = "LimeTip/tapid"
)

$ErrorActionPreference = "Stop"
$ReleaseBaseUrl = if ($env:TAPID_RELEASE_BASE_URL) { $env:TAPID_RELEASE_BASE_URL.TrimEnd('/') } else { "https://github.com/$Repo/releases/download" }
$ReleaseDiscoveryUrl = if ($env:TAPID_RELEASE_DISCOVERY_URL) { $env:TAPID_RELEASE_DISCOVERY_URL } else { "https://github.com/$Repo/releases/latest" }

function Fail([string]$Message) {
    throw "tapid installer: $Message"
}

$PathUpdated = $false
function Configure-UserPath([string]$Directory) {
    $normalized = ([IO.Path]::GetFullPath($Directory)).TrimEnd('\\')
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\\') })
    $alreadyConfigured = @($entries | Where-Object {
        ([IO.Path]::GetFullPath($_)).TrimEnd('\\') -ieq $normalized
    }).Count -gt 0
    if (-not $alreadyConfigured) {
        $newUserPath = (($entries + $normalized) -join ';')
        [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    }
    if (($env:Path -split ';' | ForEach-Object { $_.TrimEnd('\\') }) -notcontains $normalized) {
        if ([string]::IsNullOrEmpty($env:Path)) {
            $env:Path = $normalized
        } else {
            $env:Path = "$normalized;$env:Path"
        }
    }
    $script:PathUpdated = $true
}

function Print-PathGuidance {
    if ($PathUpdated) {
        Write-Output "Tapid is ready in this PowerShell session and future user sessions."
        Write-Output "Open a new terminal if another process does not see the updated PATH."
    }
}

function Test-Repository([string]$Value) {
    return $Value -match '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'
}

function Test-RegularDestination([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Any)) {
        return
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        Fail "existing Tapid destination must not be a symlink or reparse point"
    }
    if (-not ($item -is [IO.FileInfo])) {
        Fail "existing Tapid destination must be a regular file"
    }
}

if (-not (Test-Repository $Repo)) {
    Fail "repository must be OWNER/REPO"
}
if (-not [IO.Path]::IsPathRooted($InstallDir)) {
    Fail "install directory must be an absolute path"
}
if ($PSBoundParameters.ContainsKey("Version") -and $PSBoundParameters.ContainsKey("SourceRef")) {
    Fail "use either -Version or -SourceRef, not both"
}
if ($PSBoundParameters.ContainsKey("SourceRef") -and [string]::IsNullOrWhiteSpace($SourceRef)) {
    Fail "-SourceRef requires a non-empty value"
}
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$destination = Join-Path $InstallDir "tapid.exe"
Test-RegularDestination $destination

if (-not [string]::IsNullOrEmpty($SourceRef)) {
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        Fail "git is required for -SourceRef"
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail "cargo is required for -SourceRef"
    }
    if ($SourceRef.StartsWith("-")) {
        Fail "source ref must not start with '-'"
    }

    $tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("tapid-install-" + [guid]::NewGuid().ToString("N"))
    $checkout = Join-Path $tempRoot "tapid"
    $cargoRoot = Join-Path $tempRoot "root"
    $staged = Join-Path $InstallDir (".tapid.tmp." + [guid]::NewGuid().ToString("N") + ".exe")
    $stagedMarker = Join-Path $InstallDir (".tapid-marker.tmp." + [guid]::NewGuid().ToString("N"))
    try {
        New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null
        & git clone --filter=blob:none --no-checkout "https://github.com/$Repo.git" $checkout
        if ($LASTEXITCODE -ne 0) { Fail "could not clone $Repo" }
        & git -C $checkout checkout --detach $SourceRef
        if ($LASTEXITCODE -ne 0) {
            & git -C $checkout fetch --filter=blob:none origin $SourceRef
            if ($LASTEXITCODE -ne 0) { Fail "could not find source ref $SourceRef in $Repo" }
            & git -C $checkout checkout --detach $SourceRef
            if ($LASTEXITCODE -ne 0) { Fail "could not check out source ref $SourceRef in $Repo" }
        }
        & cargo install --path (Join-Path $checkout "crates\tapid-cli") --locked --root $cargoRoot
        if ($LASTEXITCODE -ne 0) { Fail "cargo build failed" }
        Copy-Item -LiteralPath (Join-Path $cargoRoot "bin\tapid.exe") -Destination $staged -Force
        [IO.File]::WriteAllBytes($stagedMarker, [Text.Encoding]::ASCII.GetBytes("tapid-managed-v1`n"))
        Move-Item -LiteralPath $staged -Destination $destination -Force
        Move-Item -LiteralPath $stagedMarker -Destination (Join-Path $InstallDir ".tapid-managed") -Force
        Configure-UserPath $InstallDir
        Write-Output "Installed Tapid from $SourceRef into $destination"
        Print-PathGuidance
        exit 0
    }
    finally {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $stagedMarker -Force -ErrorAction SilentlyContinue
    }
}

if ($Version -eq "latest") {
    try {
        $discovery = Invoke-WebRequest -Method Head -UseBasicParsing -MaximumRedirection 10 $ReleaseDiscoveryUrl
        $resolvedPath = $discovery.BaseResponse.ResponseUri.AbsolutePath
        if ($resolvedPath -notmatch '/releases/tag/(v?[0-9]+\.[0-9]+\.[0-9]+)$') {
            Fail "stable release discovery endpoint did not resolve a release tag; use -SourceRef REF for development installation"
        }
        $Version = $Matches[1]
    }
    catch {
        Fail "could not contact the stable release discovery endpoint"
    }
}
if ($Version -notmatch '^v?[0-9]+\.[0-9]+\.[0-9]+$') {
    Fail "version must be a stable release such as v0.1.0"
}
if (-not $Version.StartsWith("v")) {
    $Version = "v$Version"
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$target = switch ($architecture) {
    "X64" { "x86_64-pc-windows-msvc" }
    "Arm64" { "aarch64-pc-windows-msvc" }
    default { Fail "unsupported Windows architecture: $architecture" }
}

if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
    Fail "tar.exe is required for Windows release installation"
}
if (-not (Get-Command python.exe -ErrorAction SilentlyContinue)) {
    Fail "unsupported Ed25519 verifier: python.exe is required"
}

$VerifierSource = @'
#!/usr/bin/env python3
"""Self-contained Tapid release-manifest verifier for first-install bootstrap."""
import base64, datetime, hashlib, json, re, sys

PUBLIC_KEY = "eYPvN15Ah8ytHoBd2jY+36Wh/5g1kbqhDA9TL6wPRWc="
PUBLIC_KEY_FINGERPRINT = "sha256-238d16177b1c9ae21b53476d1a9097b5011414a26e6625986ecf1799dacf47f4"
KEY_ID = "release-key-2026-01"
TARGET_RE = re.compile(r"^[A-Za-z0-9._-]+$")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")


def fail(message):
    print("tapid installer: " + message, file=sys.stderr)
    raise SystemExit(1)


def jcs(value):
    # RFC 8785 for the release schema: all numeric values are non-negative
    # integers (artifact sizes); reject other numbers rather than guessing.
    if isinstance(value, dict):
        return "{" + ",".join(json.dumps(k, ensure_ascii=False, separators=(",", ":")) + ":" + jcs(value[k]) for k in sorted(value)) + "}"
    if isinstance(value, list):
        return "[" + ",".join(jcs(item) for item in value) + "]"
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if isinstance(value, bool) or value is None:
        return json.dumps(value, separators=(",", ":"))
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return str(value)
    fail("manifest contains an unsupported number for RFC 8785 canonicalization")


def canonical(value):
    return jcs(value).encode("utf-8")


# RFC 8032 Ed25519 verification without a platform crypto CLI.
Q = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = (-121665 * pow(121666, Q - 2, Q)) % Q
I = pow(2, (Q - 1) // 4, Q)
B = (15112221349535400772501151409588531511454012693041857206046113283949847762202,
     46316835694926478169428394003475163141307993866256225615783033603165251855960)

def _ed_add(left, right):
    x1, y1 = left; x2, y2 = right
    dx = pow(1 + D * x1 * x2 * y1 * y2, Q - 2, Q)
    dy = pow(1 - D * x1 * x2 * y1 * y2, Q - 2, Q)
    return ((x1 * y2 + x2 * y1) * dx % Q, (y1 * y2 + x1 * x2) * dy % Q)

def _ed_scalarmult(point, scalar):
    result = (0, 1)
    while scalar:
        if scalar % 2: result = _ed_add(result, point)
        point = _ed_add(point, point); scalar //= 2
    return result

def _ed_decode(encoded):
    if len(encoded) != 32: raise ValueError("invalid Ed25519 point length")
    value = int.from_bytes(encoded, "little"); sign = value // (1 << 255); y = value % (1 << 255)
    if y >= Q: raise ValueError("non-canonical Ed25519 point")
    xx = (y * y - 1) * pow(D * y * y + 1, Q - 2, Q) % Q
    x = pow(xx, (Q + 3) // 8, Q)
    if (x * x - xx) % Q: x = x * I % Q
    if (x * x - xx) % Q or (x == 0 and sign): raise ValueError("invalid Ed25519 point")
    if x % 2 != sign: x = Q - x
    return x, y

def verify_ed25519(public_key, signature, message):
    if len(public_key) != 32 or len(signature) != 64:
        return False
    try: public_point = _ed_decode(public_key); signature_point = _ed_decode(signature[:32])
    except ValueError: return False
    scalar = int.from_bytes(signature[32:], "little")
    if scalar >= L: return False
    challenge = int.from_bytes(hashlib.sha512(signature[:32] + public_key + message).digest(), "little") % L
    # RFC 8032 verifies the cofactored equation, which also handles
    # otherwise-valid encoded points outside the prime-order subgroup.
    return _ed_scalarmult(B, scalar * 8) == _ed_scalarmult(
        _ed_add(signature_point, _ed_scalarmult(public_point, challenge)), 8
    )


def parse_time(value):
    try:
        parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except (AttributeError, ValueError):
        fail("signed release manifest timestamp is invalid")
    if parsed.tzinfo is None:
        fail("signed release manifest timestamp must include a timezone")
    return parsed.astimezone(datetime.timezone.utc)


def validate_freshness(created_at, expires_at, now=None):
    now = now or datetime.datetime.now(datetime.timezone.utc)
    created = parse_time(created_at)
    expires = parse_time(expires_at)
    if created > now or expires <= now or expires <= created:
        fail("signed release manifest is stale or not yet valid")


def verify(manifest_path, target, version):
    try:
        value = json.loads(open(manifest_path, encoding="utf-8").read())
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        fail("cannot parse signed release manifest: " + str(exc))
    if not isinstance(value, dict):
        fail("signed release manifest must be an object")
    allowed = {"schema", "product", "version", "tag", "commit", "created_at", "expires_at", "artifacts", "sbom", "provenance", "signature"}
    if set(value) - allowed or value.get("schema") != "tapid-release-manifest-v1" or value.get("product") != "tapid":
        fail("invalid signed release manifest identity")
    bare_version = version[1:] if version.startswith("v") else version
    if value.get("version") != bare_version or value.get("tag") != version:
        fail("signed release manifest version does not match requested version")
    signature = value.get("signature")
    if not isinstance(signature, dict) or set(signature) != {"algorithm", "key_id", "signed_digest", "value"}:
        fail("signed release manifest signature fields are invalid")
    if signature["algorithm"] != "ed25519" or signature["key_id"] != KEY_ID:
        fail("signed release manifest is not signed by the production release key")
    unsigned = dict(value); unsigned.pop("signature", None)
    unsigned_bytes = canonical(unsigned)
    digest = "sha256-" + hashlib.sha256(unsigned_bytes).hexdigest()
    if signature["signed_digest"] != digest:
        fail("signed release manifest digest mismatch")
    context = dict(unsigned)
    context["signature_context"] = {"algorithm": "ed25519", "key_id": KEY_ID, "signed_digest": digest}
    try:
        sig = base64.b64decode(signature["value"], validate=True)
        pub = base64.b64decode(PUBLIC_KEY, validate=True)
    except Exception:
        fail("signed release manifest signature encoding is invalid")
    if len(sig) != 64 or len(pub) != 32:
        fail("signed release manifest signature key material is invalid")
    if "sha256-" + hashlib.sha256(pub).hexdigest() != PUBLIC_KEY_FINGERPRINT:
        fail("embedded production release key fingerprint is invalid")
    if not verify_ed25519(pub, sig, canonical(context)):
        fail("signed release manifest Ed25519 verification failed")
    validate_freshness(value.get("created_at"), value.get("expires_at"))
    artifacts = value.get("artifacts")
    if not isinstance(artifacts, list):
        fail("signed release manifest artifacts must be an array")
    matches = [a for a in artifacts if isinstance(a, dict) and a.get("target") == target]
    if len(matches) != 1:
        fail("signed release manifest must contain exactly one artifact for " + target)
    artifact = matches[0]
    expected_name = "tapid-%s-%s.tar.gz" % (bare_version, target)
    if artifact.get("name") != expected_name or not isinstance(artifact.get("url"), str) or not artifact["url"].startswith("https://"):
        fail("signed release manifest artifact identity or URL is invalid")
    if not isinstance(artifact.get("sha256"), str) or not HEX64_RE.fullmatch(artifact["sha256"]):
        fail("signed release manifest artifact hash is invalid")
    if not isinstance(artifact.get("size"), int) or isinstance(artifact["size"], bool) or artifact["size"] < 1:
        fail("signed release manifest artifact size is invalid")
    print("\t".join((artifact["name"], artifact["url"], artifact["sha256"], str(artifact["size"]))) )

if __name__ == "__main__":
    if len(sys.argv) != 4: fail("verifier requires MANIFEST TARGET VERSION")
    verify(sys.argv[1], sys.argv[2], sys.argv[3])
'@
$versionWithoutV = $Version.Substring(1)
$base = "$ReleaseBaseUrl/$Version"
if ($ReleaseBaseUrl -notmatch '^https://') { Fail "stable release base URL must use HTTPS" }
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("tapid-install-" + [guid]::NewGuid().ToString("N"))
$manifestPath = Join-Path $tempRoot "manifest.json"
$extractRoot = Join-Path $tempRoot "extracted"
$archivePath = Join-Path $tempRoot ("tapid-$versionWithoutV-$target.tar.gz")
$staged = Join-Path $InstallDir (".tapid.tmp." + [guid]::NewGuid().ToString("N") + ".exe")
$stagedMarker = Join-Path $InstallDir (".tapid-marker.tmp." + [guid]::NewGuid().ToString("N"))
try {
    New-Item -ItemType Directory -Force -Path $extractRoot | Out-Null
    Invoke-WebRequest -UseBasicParsing "$base/release-manifest.json" -OutFile $manifestPath
    $artifactInfo = $VerifierSource | & python.exe - $manifestPath $target $Version
    if ($LASTEXITCODE -ne 0) { Fail "signed release manifest verification failed" }
    $parts = $artifactInfo -split "`t", 4
    if ($parts.Count -ne 4) { Fail "signed release manifest artifact selection failed" }
    $archive, $artifactUrl, $expected, $expectedSize = $parts
    Invoke-WebRequest -UseBasicParsing $artifactUrl -OutFile $archivePath
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { Fail "checksum verification failed for $archive" }
    if ((Get-Item -LiteralPath $archivePath).Length -ne $expectedSize) { Fail "artifact size verification failed for $archive" }
    $members = @(& tar.exe -tzf $archivePath)
    if ($LASTEXITCODE -ne 0 -or @($members).Count -ne 1 -or $members[0] -ne "tapid.exe") {
        Fail "release archive must contain exactly one member named tapid.exe"
    }
    & tar.exe -xzf $archivePath -C $extractRoot tapid.exe
    if ($LASTEXITCODE -ne 0) { Fail "cannot extract tapid.exe from release archive" }
    $extracted = Join-Path $extractRoot "tapid.exe"
    Test-RegularDestination $extracted
    Copy-Item -LiteralPath $extracted -Destination $staged -Force
    [IO.File]::WriteAllBytes($stagedMarker, [Text.Encoding]::ASCII.GetBytes("tapid-managed-v1`n"))
    Move-Item -LiteralPath $staged -Destination $destination -Force
    Move-Item -LiteralPath $stagedMarker -Destination (Join-Path $InstallDir ".tapid-managed") -Force
    Configure-UserPath $InstallDir
    Write-Output "Installed Tapid $Version into $destination"
    Print-PathGuidance
}
finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stagedMarker -Force -ErrorAction SilentlyContinue
}
