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
$ManifestUrlOverride = $env:TAPID_RELEASE_MANIFEST_URL

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
    $manifestUrl = if ($ManifestUrlOverride) { $ManifestUrlOverride } else { "$ReleaseDiscoveryUrl/download/tapid-manifest.json" }
    $manifestFallback = if ($ManifestUrlOverride) { $null } else { "$ReleaseBaseUrl/latest/tapid-manifest.json" }
} else {
    if ($Version -notmatch '^v?[0-9]+\.[0-9]+\.[0-9]+$') {
        Fail "version must be a stable release such as v0.1.0"
    }
    if (-not $Version.StartsWith("v")) { $Version = "v$Version" }
    $manifestUrl = if ($ManifestUrlOverride) { $ManifestUrlOverride } else { "$ReleaseBaseUrl/$Version/tapid-manifest.json" }
    $manifestFallback = if ($ManifestUrlOverride) { $null } else { "$ReleaseBaseUrl/$Version/manifest.json" }
}
if ($Version -ne "latest" -and $Version -notmatch '^v?[0-9]+\.[0-9]+\.[0-9]+$') {
    Fail "version must be a stable release such as v0.1.0"
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
if (-not (Get-Command openssl.exe -ErrorAction SilentlyContinue)) {
    Fail "fail closed: OpenSSL with Ed25519 support is required; Windows PowerShell has no guaranteed stock verifier"
}

$versionWithoutV = if ($Version -eq "latest") { "latest" } else { $Version.Substring(1) }
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("tapid-install-" + [guid]::NewGuid().ToString("N"))
$archivePath = Join-Path $tempRoot "artifact.tar.gz"
$manifestPath = Join-Path $tempRoot "manifest.json"
$signaturePath = Join-Path $tempRoot "manifest.json.sig"
$publicKeyPath = Join-Path $tempRoot "release-signing-key.pem"
$extractRoot = Join-Path $tempRoot "extracted"
$staged = Join-Path $InstallDir (".tapid.tmp." + [guid]::NewGuid().ToString("N") + ".exe")
$stagedMarker = Join-Path $InstallDir (".tapid-marker.tmp." + [guid]::NewGuid().ToString("N"))
try {
    New-Item -ItemType Directory -Force -Path $extractRoot | Out-Null
    try { Invoke-WebRequest -UseBasicParsing $manifestUrl -OutFile $manifestPath; Invoke-WebRequest -UseBasicParsing "$manifestUrl.sig" -OutFile $signaturePath }
    catch {
        if (-not $manifestFallback) { Fail "could not contact the stable release discovery endpoint or fetch the signed manifest" }
        $manifestUrl = $manifestFallback
        try { Invoke-WebRequest -UseBasicParsing $manifestUrl -OutFile $manifestPath; Invoke-WebRequest -UseBasicParsing "$manifestUrl.sig" -OutFile $signaturePath }
        catch { Fail "could not fetch the stable signed manifest" }
    }
    # CONFIGURATION: release-signing public key only; no private key is accepted.
    if ($env:TAPID_RELEASE_PUBLIC_KEY_FILE) { Copy-Item -LiteralPath $env:TAPID_RELEASE_PUBLIC_KEY_FILE -Destination $publicKeyPath -Force }
    else { [IO.File]::WriteAllText($publicKeyPath, "-----BEGIN PUBLIC KEY-----`nMCowBQYDK2VwAyEAKH2wLpL1ZawchfeUH3TH4xxWHHwdHel/GtPSTCNy8SY=`n-----END PUBLIC KEY-----`n") }
    & openssl.exe pkeyutl -verify -pubin -inkey $publicKeyPath -rawin -in $manifestPath -sigfile $signaturePath 2>$null
    if ($LASTEXITCODE -ne 0) { Fail "Ed25519 manifest signature verification failed" }
    try { $manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json } catch { Fail "signed manifest is not valid JSON" }
    if ($manifest.schema_version -ne 1 -or $manifest.target -ne $target) { Fail "signed manifest has invalid target or schema" }
    if ($manifest.version -notmatch '^v?[0-9]+\.[0-9]+\.[0-9]+$' -or ($Version -ne "latest" -and $manifest.version -ne $Version)) { Fail "signed manifest has invalid version" }
    $Version = if ($manifest.version.StartsWith("v")) { $manifest.version } else { "v$($manifest.version)" }
    $artifactUrl = [string]$manifest.artifact_url
    $artifactSize = [long]$manifest.artifact_size
    $expected = ([string]$manifest.artifact_sha256).ToLowerInvariant()
    if ($artifactUrl -notmatch '^https://|^file://' -or $artifactSize -lt 0 -or $expected -notmatch '^[0-9a-f]{64}$') { Fail "signed manifest has invalid artifact metadata" }
    Invoke-WebRequest -UseBasicParsing $artifactUrl -OutFile $archivePath
    if ((Get-Item -LiteralPath $archivePath).Length -ne $artifactSize) { Fail "signed artifact size verification failed" }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { Fail "signed artifact SHA-256 verification failed" }
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
