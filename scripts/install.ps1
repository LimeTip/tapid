[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$SourceRef,
    [string]$InstallDir = (Join-Path $HOME ".local\bin"),
    [string]$Repo = "LimeTip/tapid"
)

$ErrorActionPreference = "Stop"
$MAX_RECORD_BYTES = 256KB
$MAX_CHECKSUM_BYTES = 1MB
$MAX_ARCHIVE_BYTES = 512MB
$MAX_BINARY_BYTES = 512MB
function Fail([string]$Message) { throw "tapid installer: $Message" }

function Save-BoundedHttpsFile([string]$Uri, [string]$Path, [long]$MaxBytes) {
    Add-Type -AssemblyName System.Net.Http
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $response = $null
    $source = $null
    $destinationStream = $null
    $downloadError = $null
    $cleanupError = $null
    try {
        $client.DefaultRequestHeaders.UserAgent.ParseAdd("tapid-installer")
        $currentUri = [Uri]$Uri
        if ($currentUri.Scheme -ne "https") { Fail "download URL must use HTTPS" }
        [int]$redirectCount = 0
        while ($true) {
            $response = $client.GetAsync($currentUri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
            $statusCode = [int]$response.StatusCode
            if ($statusCode -in 301, 302, 303, 307, 308) {
                if ($redirectCount -ge 10) { Fail "download encountered too many redirects" }
                $nextUri = $response.Headers.Location
                if (-not $nextUri) { Fail "download redirect did not include a target" }
                if (-not $nextUri.IsAbsoluteUri) { $nextUri = [Uri]::new($currentUri, $nextUri) }
                if ($nextUri.Scheme -ne "https") { Fail "download redirect target must use HTTPS" }
                $response.Dispose()
                $response = $null
                $currentUri = $nextUri
                $redirectCount += 1
                continue
            }
            $null = $response.EnsureSuccessStatusCode()
            if ($response.RequestMessage.RequestUri.Scheme -ne "https") { Fail "download redirected outside HTTPS" }
            break
        }
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
        $downloadError = $_
    } finally {
        try { if ($destinationStream) { $destinationStream.Dispose() } } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { if ($source) { $source.Dispose() } } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { if ($response) { $response.Dispose() } } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { $client.Dispose() } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { $handler.Dispose() } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        if ($downloadError -or $cleanupError) { Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue }
    }
    if ($downloadError) { throw $downloadError }
    if ($cleanupError) { throw $cleanupError }
}

function Test-ReleaseVersion([string]$Value) {
    if ($Value -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z') { return $false }
    foreach ($part in $Value.Split('.')) {
        if ($part.Length -gt 20 -or ($part.Length -eq 20 -and [string]::CompareOrdinal($part, '18446744073709551615') -gt 0)) { return $false }
    }
    return $true
}

function Assert-ReleaseHttpsUrl([string]$Url) {
    if ($Url -cnotmatch '^https://[A-Za-z0-9][A-Za-z0-9.-]*(:[0-9]+)?(/.*)?\z' -or $Url -match '[^\x21-\x7e]|[@?#\\]') {
        Fail "release record URL must be a safe HTTPS URL"
    }
}

function Read-ReleaseRecord([string]$Path, [string]$RequestedVersion, [string]$Target) {
    if ((Get-Item -LiteralPath $Path).Length -gt $MAX_RECORD_BYTES) { Fail "release record exceeds the size limit" }
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -eq 0 -or $bytes[-1] -ne 10) { Fail "release record must end with a newline" }
    foreach ($byte in $bytes) {
        if ($byte -gt 126 -or ($byte -lt 32 -and $byte -ne 9 -and $byte -ne 10)) { Fail "release record must contain ASCII fields and LF lines" }
    }
    $lines = [Text.Encoding]::ASCII.GetString($bytes).Split([char]10)
    if ($lines[-1] -eq '') { $lines = $lines[0..($lines.Length - 2)] }
    $header = $lines[0].Split([char]9)
    if ($header.Length -ne 2 -or $header[0] -cne 'tapid-release-v1' -or -not (Test-ReleaseVersion $header[1])) {
        Fail "invalid release record header"
    }
    $recordVersion = $header[1]
    if ($RequestedVersion -ne 'latest' -and $RequestedVersion -cne $recordVersion) { Fail "release record version does not match requested version" }
    $targets = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $selected = $null
    for ($index = 1; $index -lt $lines.Length; $index++) {
        $fields = $lines[$index].Split([char]9)
        if ($fields.Length -ne 5 -or $fields[0] -cnotmatch '^[A-Za-z0-9_-]+\z') { Fail "invalid release record artifact" }
        if (-not $targets.Add($fields[0]) -or -not $names.Add($fields[1])) { Fail "duplicate release record artifact" }
        if ($fields[1] -cne "tapid-$recordVersion-$($fields[0]).tar.gz") { Fail "invalid release record archive name" }
        if ($fields[2] -cnotmatch '^[1-9][0-9]{0,8}\z' -or [long]$fields[2] -gt $MAX_ARCHIVE_BYTES) { Fail "invalid release record archive size" }
        if ($fields[3] -cnotmatch '^[0-9a-f]{64}\z') { Fail "invalid release record checksum" }
        Assert-ReleaseHttpsUrl $fields[4]
        if ($fields[0] -ceq $Target) {
            $selected = [pscustomobject]@{ Version = $recordVersion; Archive = $fields[1]; Size = [long]$fields[2]; Sha256 = $fields[3]; Url = $fields[4] }
        }
    }
    if (-not $selected) { Fail "release record is missing this platform" }
    return $selected
}

function Test-AbsolutePath([string]$Path) {
    if ([string]::IsNullOrEmpty($Path) -or $Path -match '[\r\n]') { return $false }
    if ([IO.Path]::DirectorySeparatorChar -eq '\') {
        return $Path -match '^(?:[A-Za-z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+(?:[\\/].*)?\z)'
    }
    return $Path.StartsWith('/')
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
if (-not (Test-AbsolutePath $InstallDir)) { Fail "install directory must be an absolute path" }
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
        Move-Item -LiteralPath $stagedMarker -Destination (Join-Path $InstallDir ".tapid-managed") -Force
        Move-Item -LiteralPath $staged -Destination $destination -Force
        try {
            Configure-UserPath $InstallDir
        } catch {
            Write-Warning "Tapid was installed, but the user PATH could not be updated: $($_.Exception.Message)"
        }
        Write-Output "Installed Tapid from $SourceRef into $destination"
        Print-PathGuidance
        exit 0
    } finally {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $stagedMarker -Force -ErrorAction SilentlyContinue
    }
}

$legacyRelease = $PSBoundParameters.ContainsKey('Repo') -or $env:TAPID_RELEASE_BASE_URL -or $env:TAPID_RELEASE_DISCOVERY_URL
if ($Version -ne 'latest') {
    if (-not (Test-ReleaseVersion ($Version -creplace '^v', ''))) { Fail "version must be a stable release such as v0.1.0" }
    $Version = $Version -creplace '^v', ''
    # These versions predate release records. Request failures never select this path.
    if (-not $env:TAPID_RELEASE_RECORD_URL -and $Version -match '^0\.0\.([0-9]|10)\z') { $legacyRelease = $true }
}
if ($env:TAPID_RELEASE_RECORD_URL -and $legacyRelease) { Fail "release record URL cannot be combined with legacy release overrides" }

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$target = switch ($architecture) {
    "X64" { "x86_64-pc-windows-msvc" }
    "Arm64" { "aarch64-pc-windows-msvc" }
    default { Fail "unsupported Windows architecture: $architecture" }
}
if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) { Fail "tar.exe is required for Windows release installation" }

$expectedSize = $null
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("tapid-install-" + [guid]::NewGuid().ToString("N"))
$checksumsPath = Join-Path $tempRoot "SHA256SUMS"
$extractRoot = Join-Path $tempRoot "extracted"
$staged = Join-Path $InstallDir (".tapid.tmp." + [guid]::NewGuid().ToString("N") + ".exe")
$stagedMarker = Join-Path $InstallDir (".tapid-marker.tmp." + [guid]::NewGuid().ToString("N"))
try {
    New-Item -ItemType Directory -Force -Path $extractRoot | Out-Null
    if ($legacyRelease) {
        $ReleaseBaseUrl = if ($env:TAPID_RELEASE_BASE_URL) { $env:TAPID_RELEASE_BASE_URL.TrimEnd('/') } else { "https://github.com/$Repo/releases/download" }
        $ReleaseDiscoveryUrl = if ($env:TAPID_RELEASE_DISCOVERY_URL) { $env:TAPID_RELEASE_DISCOVERY_URL } else { "https://github.com/$Repo/releases/latest" }
        if ($ReleaseBaseUrl -notmatch '^https://') { Fail "stable release base URL must use HTTPS" }
        if ($ReleaseDiscoveryUrl -notmatch '^https://') { Fail "stable release discovery URL must use HTTPS" }
        if ($Version -eq 'latest') {
            try {
                $discovery = Invoke-WebRequest -Method Head -UseBasicParsing -MaximumRedirection 10 $ReleaseDiscoveryUrl
            } catch { Fail "could not contact the stable release discovery endpoint" }
            $resolvedUri = $discovery.BaseResponse.ResponseUri
            if (-not $resolvedUri -and $discovery.BaseResponse.RequestMessage) {
                $resolvedUri = $discovery.BaseResponse.RequestMessage.RequestUri
            }
            if (-not $resolvedUri) { Fail "stable release discovery endpoint did not expose its final URL" }
            if ($resolvedUri.AbsolutePath -cnotmatch '/releases/tag/(v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*))\z') { Fail "stable release discovery endpoint did not resolve a release tag" }
            $Version = $Matches[1] -creplace '^v', ''
            if (-not (Test-ReleaseVersion $Version)) { Fail "version must be a stable release such as v0.1.0" }
        }
        $archive = "tapid-$Version-$target.tar.gz"
        $base = "$ReleaseBaseUrl/v$Version"
        Save-BoundedHttpsFile "$base/SHA256SUMS" $checksumsPath $MAX_CHECKSUM_BYTES
        if ((Get-Item -LiteralPath $checksumsPath).Length -gt $MAX_CHECKSUM_BYTES) { Fail "SHA256SUMS exceeds the size limit" }
        $pattern = '^([0-9a-fA-F]{64})\s{2}' + [regex]::Escape($archive) + '$'
        $matches = @(Get-Content -LiteralPath $checksumsPath | Where-Object { $_ -match $pattern })
        if ($matches.Count -ne 1) { Fail "SHA256SUMS does not contain exactly one checksum for $archive" }
        $null = $matches[0] -match $pattern
        $expected = $Matches[1].ToLowerInvariant()
        $archiveUrl = "$base/$archive"
    } else {
        $recordUrl = if ($env:TAPID_RELEASE_RECORD_URL) { $env:TAPID_RELEASE_RECORD_URL } elseif ($Version -eq 'latest') { 'https://tapid.dev/releases/v1/latest.tsv' } else { "https://tapid.dev/releases/v1/v$Version.tsv" }
        Assert-ReleaseHttpsUrl $recordUrl
        $recordPath = Join-Path $tempRoot 'release.tsv'
        Save-BoundedHttpsFile $recordUrl $recordPath $MAX_RECORD_BYTES
        $record = Read-ReleaseRecord $recordPath $Version $target
        $Version = $record.Version
        $archive = $record.Archive
        $expected = $record.Sha256
        $expectedSize = $record.Size
        $archiveUrl = $record.Url
    }
    $archivePath = Join-Path $tempRoot $archive
    Save-BoundedHttpsFile $archiveUrl $archivePath $MAX_ARCHIVE_BYTES
    $actualSize = (Get-Item -LiteralPath $archivePath).Length
    if ($actualSize -gt $MAX_ARCHIVE_BYTES) { Fail "release archive exceeds the size limit" }
    if ($null -ne $expectedSize -and $actualSize -ne $expectedSize) { Fail "release archive size does not match release record" }
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
    Move-Item -LiteralPath $stagedMarker -Destination (Join-Path $InstallDir ".tapid-managed") -Force
    Move-Item -LiteralPath $staged -Destination $destination -Force
    try {
        Configure-UserPath $InstallDir
    } catch {
        Write-Warning "Tapid was installed, but the user PATH could not be updated: $($_.Exception.Message)"
    }
    Write-Output "Installed Tapid v$Version into $destination"
    Print-PathGuidance
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stagedMarker -Force -ErrorAction SilentlyContinue
}
