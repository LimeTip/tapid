[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$SourceRef,
    [string]$InstallDir = (Join-Path $HOME ".local\bin"),
    [string]$Repo = "LimeTip/tapid"
)

$ErrorActionPreference = "Stop"
$MAX_CHECKSUM_BYTES = 1MB
$MAX_ARCHIVE_BYTES = 512MB
$MAX_BINARY_BYTES = 512MB
function Fail([string]$Message) { throw "tapid installer: $Message" }

function Save-BoundedHttpsFile([string]$Uri, [string]$Path, [long]$MaxBytes) {
    if ($env:TAPID_TEST_FIXTURE) {
        Copy-Item -LiteralPath (Join-Path $env:TAPID_TEST_FIXTURE ([IO.Path]::GetFileName($Uri))) -Destination $Path
        if ((Get-Item -LiteralPath $Path).Length -gt $MaxBytes) { Fail "download exceeds the size limit" }
        return
    }
    $handler = [Net.Http.HttpClientHandler]::new()
    $client = [Net.Http.HttpClient]::new($handler)
    $response = $null
    $source = $null
    $destinationStream = $null
    try {
        $client.DefaultRequestHeaders.UserAgent.ParseAdd("tapid-installer")
        $response = $client.GetAsync($Uri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        $response.EnsureSuccessStatusCode()
        if ($response.RequestMessage.RequestUri.Scheme -ne "https") { Fail "download redirected outside HTTPS" }
        if ($response.Content.Headers.ContentLength -gt $MaxBytes) { Fail "download exceeds the size limit" }
        $source = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $destinationStream = [IO.File]::Create($Path)
        $buffer = [byte[]]::new(65536)
        [long]$total = 0
        while (($read = $source.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $total += $read
            if ($total -gt $MaxBytes) { Fail "download exceeds the size limit" }
            $destinationStream.Write($buffer, 0, $read)
        }
    } catch {
        Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
        throw
    } finally {
        if ($destinationStream) { $destinationStream.Dispose() }
        if ($source) { $source.Dispose() }
        if ($response) { $response.Dispose() }
        $client.Dispose()
        $handler.Dispose()
    }
}

$PathUpdated = $false
function Configure-UserPath([string]$Directory) {
    $normalized = ([IO.Path]::GetFullPath($Directory)).TrimEnd('\\')
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\\') })
    $alreadyConfigured = @($entries | Where-Object { ([IO.Path]::GetFullPath($_)).TrimEnd('\\') -ieq $normalized }).Count -gt 0
    if (-not $alreadyConfigured) {
        [Environment]::SetEnvironmentVariable("Path", (($entries + $normalized) -join ';'), "User")
    }
    if (($env:Path -split ';' | ForEach-Object { $_.TrimEnd('\\') }) -notcontains $normalized) {
        $env:Path = if ([string]::IsNullOrEmpty($env:Path)) { $normalized } else { "$normalized;$env:Path" }
    }
    $script:PathUpdated = $true
}
function Print-PathGuidance {
    if ($PathUpdated) {
        Write-Output "Tapid is ready in this PowerShell session and future user sessions."
        Write-Output "Open a new terminal if another process does not see the updated PATH."
    }
}
function Test-RegularDestination([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Any)) { return }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or -not ($item -is [IO.FileInfo])) {
        Fail "destination must be a regular file and not a reparse point"
    }
}

if ($Repo -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\z') { Fail "repository must be OWNER/REPO" }
if (-not [IO.Path]::IsPathFullyQualified($InstallDir)) { Fail "install directory must be an absolute path" }
if ($PSBoundParameters.ContainsKey("Version") -and $PSBoundParameters.ContainsKey("SourceRef")) { Fail "use either -Version or -SourceRef, not both" }
if ($PSBoundParameters.ContainsKey("SourceRef") -and [string]::IsNullOrWhiteSpace($SourceRef)) { Fail "-SourceRef requires a non-empty value" }
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$destination = Join-Path $InstallDir "tapid.exe"
Test-RegularDestination $destination

if (-not [string]::IsNullOrEmpty($SourceRef)) {
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Fail "git is required for -SourceRef" }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Fail "cargo is required for -SourceRef" }
    if ($SourceRef.StartsWith("-")) { Fail "source ref must not start with '-'" }
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
    } finally {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $stagedMarker -Force -ErrorAction SilentlyContinue
    }
}

$ReleaseBaseUrl = if ($env:TAPID_RELEASE_BASE_URL) { $env:TAPID_RELEASE_BASE_URL.TrimEnd('/') } else { "https://github.com/$Repo/releases/download" }
$ReleaseDiscoveryUrl = if ($env:TAPID_RELEASE_DISCOVERY_URL) { $env:TAPID_RELEASE_DISCOVERY_URL } else { "https://github.com/$Repo/releases/latest" }
if ($ReleaseBaseUrl -notmatch '^https://') { Fail "stable release base URL must use HTTPS" }
if ($ReleaseDiscoveryUrl -notmatch '^https://') { Fail "stable release discovery URL must use HTTPS" }
if ($Version -eq "latest") {
    try {
        $discovery = Invoke-WebRequest -Method Head -UseBasicParsing -MaximumRedirection 10 $ReleaseDiscoveryUrl
        $resolvedPath = $discovery.BaseResponse.ResponseUri.AbsolutePath
        if ($resolvedPath -notmatch '/releases/tag/(v?[0-9]+\.[0-9]+\.[0-9]+)\z') { Fail "stable release discovery endpoint did not resolve a release tag" }
        $Version = $Matches[1]
    } catch { Fail "could not contact the stable release discovery endpoint" }
}
if ($Version -notmatch '^v?[0-9]+\.[0-9]+\.[0-9]+\z') { Fail "version must be a stable release such as v0.1.0" }
if (-not $Version.StartsWith("v")) { $Version = "v$Version" }

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$target = switch ($architecture) {
    "X64" { "x86_64-pc-windows-msvc" }
    "Arm64" { "aarch64-pc-windows-msvc" }
    default { Fail "unsupported Windows architecture: $architecture" }
}
if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) { Fail "tar.exe is required for Windows release installation" }

$versionWithoutV = $Version.Substring(1)
$archive = "tapid-$versionWithoutV-$target.tar.gz"
$base = "$ReleaseBaseUrl/$Version"
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("tapid-install-" + [guid]::NewGuid().ToString("N"))
$archivePath = Join-Path $tempRoot $archive
$checksumsPath = Join-Path $tempRoot "SHA256SUMS"
$extractRoot = Join-Path $tempRoot "extracted"
$staged = Join-Path $InstallDir (".tapid.tmp." + [guid]::NewGuid().ToString("N") + ".exe")
$stagedMarker = Join-Path $InstallDir (".tapid-marker.tmp." + [guid]::NewGuid().ToString("N"))
try {
    New-Item -ItemType Directory -Force -Path $extractRoot | Out-Null
    Save-BoundedHttpsFile "$base/SHA256SUMS" $checksumsPath $MAX_CHECKSUM_BYTES
    if ((Get-Item -LiteralPath $checksumsPath).Length -gt $MAX_CHECKSUM_BYTES) { Fail "SHA256SUMS exceeds the size limit" }
    $pattern = '^([0-9a-fA-F]{64})\s{2}' + [regex]::Escape($archive) + '$'
    $matches = @(Get-Content -LiteralPath $checksumsPath | Where-Object { $_ -match $pattern })
    if ($matches.Count -ne 1) { Fail "SHA256SUMS does not contain exactly one checksum for $archive" }
    $null = $matches[0] -match $pattern
    $expected = $Matches[1].ToLowerInvariant()
    Save-BoundedHttpsFile "$base/$archive" $archivePath $MAX_ARCHIVE_BYTES
    if ((Get-Item -LiteralPath $archivePath).Length -gt $MAX_ARCHIVE_BYTES) { Fail "release archive exceeds the size limit" }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { Fail "checksum verification failed for $archive" }
    $members = @(& tar.exe -tzf $archivePath)
    if ($LASTEXITCODE -ne 0 -or $members.Count -ne 1 -or $members[0] -ne "tapid.exe") { Fail "release archive must contain exactly one member named tapid.exe" }
    $details = @(& tar.exe -tvzf $archivePath)
    if ($LASTEXITCODE -ne 0 -or $details.Count -ne 1 -or $details[0] -notmatch '^-[^\r\n]*\stapid\.exe$') { Fail "release archive tapid.exe member must be a regular file" }
    if ($details[0] -notmatch '^-[^\s]+\s+\d+\s+\S+\s+\S+\s+(\d+)\s+.*\stapid\.exe\z') { Fail "cannot determine release binary uncompressed size" }
    if ([long]$Matches[1] -gt $MAX_BINARY_BYTES) { Fail "release binary exceeds the size limit" }
    & tar.exe -xzf $archivePath -C $extractRoot tapid.exe
    if ($LASTEXITCODE -ne 0) { Fail "cannot extract tapid.exe" }
    $extracted = Join-Path $extractRoot "tapid.exe"
    Test-RegularDestination $extracted
    if ((Get-Item -LiteralPath $extracted).Length -gt $MAX_BINARY_BYTES) { Fail "release binary exceeds the size limit" }
    Copy-Item -LiteralPath $extracted -Destination $staged -Force
    [IO.File]::WriteAllBytes($stagedMarker, [Text.Encoding]::ASCII.GetBytes("tapid-managed-v1`n"))
    Move-Item -LiteralPath $staged -Destination $destination -Force
    Move-Item -LiteralPath $stagedMarker -Destination (Join-Path $InstallDir ".tapid-managed") -Force
    Configure-UserPath $InstallDir
    Write-Output "Installed Tapid $Version into $destination"
    Print-PathGuidance
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stagedMarker -Force -ErrorAction SilentlyContinue
}
