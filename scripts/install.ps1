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
$ReleaseVerifier = $env:TAPID_RELEASE_VERIFIER
$TrustedKeys = $env:TAPID_RELEASE_TRUSTED_KEYS

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

$architecture = $env:PROCESSOR_ARCHITECTURE
$target = switch ($architecture) {
    "X64" { "x86_64-pc-windows-msvc" }
    "Arm64" { "aarch64-pc-windows-msvc" }
    default { Fail "unsupported Windows architecture: $architecture" }
}

if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
    Fail "tar.exe is required for Windows release installation"
}
if ([string]::IsNullOrEmpty($ReleaseVerifier) -or -not (Test-Path -LiteralPath $ReleaseVerifier -PathType Leaf)) {
    Fail "stable installation requires TAPID_RELEASE_VERIFIER; use -SourceRef REF for development installation"
}
if ([string]::IsNullOrEmpty($TrustedKeys) -or -not (Test-Path -LiteralPath $TrustedKeys -PathType Leaf)) {
    Fail "stable installation requires TAPID_RELEASE_TRUSTED_KEYS; use -SourceRef REF for development installation"
}

$versionWithoutV = $Version.Substring(1)
$base = "$ReleaseBaseUrl/$Version"
if ($ReleaseBaseUrl -notmatch '^https://') { Fail "stable release base URL must use HTTPS" }
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("tapid-install-" + [guid]::NewGuid().ToString("N"))
$manifestPath = Join-Path $tempRoot "manifest.json"
$extractRoot = Join-Path $tempRoot "extracted"
$staged = Join-Path $InstallDir (".tapid.tmp." + [guid]::NewGuid().ToString("N") + ".exe")
$stagedMarker = Join-Path $InstallDir (".tapid-marker.tmp." + [guid]::NewGuid().ToString("N"))
try {
    New-Item -ItemType Directory -Force -Path $extractRoot | Out-Null
    Invoke-WebRequest -UseBasicParsing "$base/manifest.json" -OutFile $manifestPath
    & $ReleaseVerifier $manifestPath $TrustedKeys $target $Version
    if ($LASTEXITCODE -ne 0) { Fail "signed release manifest verification failed" }
    $manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    if ($manifest.signature.algorithm -ne "ed25519") { Fail "signed release manifest has no Ed25519 signature" }
    if ($manifest.version -ne $versionWithoutV -or $manifest.tag -ne $Version) { Fail "signed release manifest version does not match requested version" }
    $artifact = @($manifest.artifacts | Where-Object { $_.target -eq $target })
    if ($artifact.Count -ne 1) { Fail "signed release manifest must contain exactly one artifact for $target" }
    $artifact = $artifact[0]
    $archive = "tapid-$versionWithoutV-$target.tar.gz"
    if ($artifact.name -ne $archive -or $artifact.url -notmatch '^https://') { Fail "signed release manifest artifact identity or URL is invalid" }
    if ($artifact.sha256 -notmatch '^[0-9a-f]{64}$' -or [int64]$artifact.size -lt 1) { Fail "signed release manifest artifact hash or size is invalid" }
    $artifactUrl = $artifact.url
    $expected = $artifact.sha256
    $expectedSize = [int64]$artifact.size
    $archivePath = Join-Path $tempRoot $archive
    Invoke-WebRequest -UseBasicParsing $artifactUrl -OutFile $archivePath
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { Fail "checksum verification failed for $archive" }
    if ((Get-Item -LiteralPath $archivePath).Length -ne $expectedSize) { Fail "artifact size verification failed for $archive" }
    $members = & tar.exe -tzf $archivePath
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
