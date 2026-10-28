<#
.SYNOPSIS
Install Tapid with optional user PATH persistence.
.DESCRIPTION
Without -ConfigurePath, stored and process PATH remain unchanged. The installer
prints a literal command for activation. -ConfigurePath opts into receipt-backed
user-scope persistence; future sessions must load the updated environment.
Each download has a 60-second total deadline across requests, redirects, and body
reads. Progress does not renew it. Timeout removes partial downloads; separate
downloads receive separate deadlines.
.PARAMETER ConfigurePath
Persist the install directory in user PATH, preserving existing tokens and kind.
#>
[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$SourceRef,
    [string]$InstallDir = (Join-Path $HOME ".local\bin"),
    [string]$Repo = "LimeTip/tapid",
    [switch]$ConfigurePath
)

$ErrorActionPreference = "Stop"
$MAX_RECORD_BYTES = 256KB
$MAX_CHECKSUM_BYTES = 1MB
$MAX_ARCHIVE_BYTES = 512MB
$MAX_BINARY_BYTES = 536870912
# Release CI fills these values from the exact built archives before publication.
$BootstrapVersion = '@TAPID_BOOTSTRAP_VERSION@'
$BootstrapBaseUrl = '@TAPID_BOOTSTRAP_BASE_URL@'
$BootstrapPins = @{
# @TAPID_BOOTSTRAP_PINS@
}
function Expand-VerifiedArchive([string]$archivePath, [string]$extractRoot) {
    New-Item -ItemType Directory -Path $extractRoot -Force | Out-Null
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
    return $extracted
}

function Fail([string]$Message) { throw "tapid installer: $Message" }

# Private construction and deadline seams; installer callers keep the three-argument interface.
function New-DownloadHandler { return [Net.Http.HttpClientHandler]::new() }
function Get-DownloadTimeoutMilliseconds { return 60000 }

# PowerShell 5.1 cannot run scriptblock continuations on arbitrary .NET threads.
# Keep bounded waits and late-result ownership in a small .NET Framework-compatible helper.
function Initialize-DownloadDeadline {
    if ('TapidDownloadDeadline' -as [type]) { return }
    $references = @{}
    if ($PSVersionTable.PSVersion.Major -le 5) { $references.ReferencedAssemblies = @('System.Net.Http.dll', 'System.dll', 'System.Core.dll') }
    Add-Type @references -TypeDefinition @'
using System;
using System.Diagnostics;
using System.IO;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;
public sealed class TapidDownloadDeadline : IDisposable {
    readonly Stopwatch clock = Stopwatch.StartNew();
    readonly CancellationTokenSource cancellation = new CancellationTokenSource();
    readonly int milliseconds;
    public TapidDownloadDeadline(int milliseconds) {
        this.milliseconds = milliseconds;
        cancellation.CancelAfter(milliseconds);
    }
    public CancellationToken Token { get { return cancellation.Token; } }
    public bool Expired { get { return clock.ElapsedMilliseconds >= milliseconds || cancellation.IsCancellationRequested; } }
    public void Check() { if (Expired) throw new TimeoutException("tapid installer: download timed out"); }
    T Await<T>(Task<T> task, bool ownsResult) {
        try {
            Check();
            int remaining = Math.Max(1, milliseconds - (int)clock.ElapsedMilliseconds);
            // Wait does not rely on the operation honoring cancellation (not all Framework streams do).
            try { if (!task.Wait(remaining)) throw new TimeoutException("tapid installer: download timed out"); }
            catch (AggregateException) { return task.GetAwaiter().GetResult(); }
            Check();
            return task.GetAwaiter().GetResult();
        } catch {
            // A response/stream can arrive after we stop waiting. Observe faults and release
            // late resources without a PowerShell runspace or access to the destination file.
            task.ContinueWith(t => {
                if (t.IsFaulted) { var observed = t.Exception; }
                else if (t.Status == TaskStatus.RanToCompletion && ownsResult) {
                    var resource = t.Result as IDisposable;
                    if (resource != null) { try { resource.Dispose(); } catch { } }
                }
            }, CancellationToken.None, TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            throw;
        }
    }
    public HttpResponseMessage Response(Task<HttpResponseMessage> task) { return Await(task, true); }
    public Stream Stream(Task<Stream> task) { return Await(task, true); }
    public int Read(Task<int> task) { return Await(task, false); }
    public void Cancel() { cancellation.Cancel(); }
    public void Dispose() { cancellation.Dispose(); }
}
'@
}

function Save-BoundedHttpsFile([string]$Uri, [string]$Path, [long]$MaxBytes) {
    Add-Type -AssemblyName System.Net.Http
    Initialize-DownloadDeadline
    $deadline = [TapidDownloadDeadline]::new((Get-DownloadTimeoutMilliseconds))
    $handler = $null
    $client = $null
    $response = $null
    $source = $null
    $destinationStream = $null
    $downloadError = $null
    $cleanupError = $null
    try {
        $handler = New-DownloadHandler
        $handler.AllowAutoRedirect = $false
        $client = [Net.Http.HttpClient]::new($handler)
        $client.DefaultRequestHeaders.UserAgent.ParseAdd("tapid-installer")
        $currentUri = [Uri]$Uri
        if ($currentUri.Scheme -ne "https") { Fail "download URL must use HTTPS" }
        [int]$redirectCount = 0
        while ($true) {
            $deadline.Check()
            $response = $deadline.Response($client.GetAsync($currentUri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $deadline.Token))
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
        $source = $deadline.Stream($response.Content.ReadAsStreamAsync())
        $destinationStream = [IO.File]::Create($Path)
        $buffer = [byte[]]::new(65536)
        [long]$total = 0
        while (($read = $deadline.Read($source.ReadAsync($buffer, 0, $buffer.Length, $deadline.Token))) -gt 0) {
            $total += $read
            if ($total -gt $MaxBytes) { Fail "download exceeds the size limit" }
            $deadline.Check()
            $destinationStream.Write($buffer, 0, $read)
        }
        $deadline.Check()
    } catch {
        $downloadError = $_
        if ($deadline.Expired) { $downloadError = [TimeoutException]::new("tapid installer: download timed out") }
    } finally {
        try { if ($downloadError) { $deadline.Cancel() } } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { if ($destinationStream) { $destinationStream.Dispose() } } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { if ($source) { $source.Dispose() } } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { if ($response) { $response.Dispose() } } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { if ($client) { $client.Dispose() } } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { if ($handler) { $handler.Dispose() } } catch { if (-not $cleanupError) { $cleanupError = $_ } }
        try { $deadline.Dispose() } catch { if (-not $cleanupError) { $cleanupError = $_ } }
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
# Comparison never rewrites stored tokens or resolves relative paths against cwd.
function Get-PathComparisonKey([string]$Token, [bool]$Expand = $true) {
    if ([string]::IsNullOrEmpty($Token) -or $Token -match '[\r\n;]' -or $Token -cne $Token.Trim()) { return $null }
    $expanded = if ($Expand) { [Environment]::ExpandEnvironmentVariables($Token) } else { $Token }
    if ($Expand -and $expanded -match '%[^%]*%') { return $null }
    $literal = $expanded.Replace('/', '\')
    if ($literal -notmatch '^(?:[A-Za-z]:\\|\\\\[^\\]+\\[^\\]+(?:\\.*)?$)') { return $null }
    if ($literal -match '[<>"|?*]' -or $literal.Substring(2).Contains(':')) { return $null }
    # Preserve drive roots; UNC share roots compare with or without their last separator.
    $key = $literal.TrimEnd([char]92)
    if ($key -match '^[A-Za-z]:$') { $key += '\' }
    return $key.ToUpperInvariant()
}
# Read raw registry bytes: environment expansion is only for comparison keys.
function Read-UserPath {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
    try {
        if ($null -eq $key -or $key.GetValueNames() -inotcontains 'Path') {
            return [pscustomobject]@{ Present = $false; Value = ''; Kind = 'String' }
        }
        $kind = $key.GetValueKind('Path').ToString()
        if ($kind -notin 'String', 'ExpandString') { Fail 'unsupported user PATH registry kind' }
        $value = $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($value.Length -gt 4MB) { Fail 'user PATH exceeds the size limit' }
        return [pscustomobject]@{ Present = $true; Value = [string]$value; Kind = $kind }
    } finally { if ($key) { $key.Dispose() } }
}
function Write-UserPath($Snapshot) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    try {
        if ($Snapshot.Present) {
            $valueKind = [Microsoft.Win32.RegistryValueKind]$Snapshot.Kind
            $key.SetValue('Path', $Snapshot.Value, $valueKind)
        } else { $key.DeleteValue('Path', $false) }
    } finally { $key.Dispose() }
}
function Test-PathSnapshot($Left, $Right) {
    return $Left.Present -eq $Right.Present -and $Left.Kind -ceq $Right.Kind -and $Left.Value -ceq $Right.Value
}
function Assert-PathReceiptLocation([string]$Path, [bool]$Directory) {
    $current = $Path
    $first = $true
    while (-not [string]::IsNullOrEmpty($current)) {
        if (Test-Path -LiteralPath $current) {
            $item = Get-Item -LiteralPath $current -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { Fail 'unsafe PATH receipt reparse point' }
            if ($first -and [IO.Path]::DirectorySeparatorChar -eq '\') {
                $owner = (Get-Acl -LiteralPath $current).GetOwner([Security.Principal.SecurityIdentifier]).Value
                $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
                try {
                    if ($owner -cne $identity.User.Value -and $owner -cne $identity.Owner.Value) { Fail 'PATH receipt must be owned by the current user or token owner' }
                } finally { $identity.Dispose() }
            }
            if (($first -and -not $Directory -and -not ($item -is [IO.FileInfo])) -or
                ((-not $first -or $Directory) -and -not ($item -is [IO.DirectoryInfo]))) { Fail 'unsafe PATH receipt location' }
        }
        $first = $false
        $parent = Split-Path -Parent $current
        if ($parent -ceq $current) { break }
        $current = $parent
    }
}
function Read-PathReceipt([string]$Receipt, [string]$Directory) {
    Assert-PathReceiptLocation $Receipt $true
    $names = @('manifest', 'directory', 'before', 'after', 'present', 'kind', 'inserted', 'state')
    $children = @(Get-ChildItem -LiteralPath $Receipt -Force)
    if ($children.Count -ne $names.Count) { Fail 'invalid PATH receipt fields' }
    $values = @{}
    foreach ($name in $names) {
        $path = Join-Path $Receipt $name
        Assert-PathReceiptLocation $path $false
        $limit = if ($name -in 'before', 'after') { 8MB } else { 64KB }
        if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or (Get-Item -LiteralPath $path -Force).Length -gt $limit) { Fail 'invalid or oversized PATH receipt field' }
        $values[$name] = [IO.File]::ReadAllText($path, [Text.UTF8Encoding]::new($false, $true))
    }
    if ($values.manifest -cne "tapid-path-v1`nwindows`n1`n" -or $values.directory -cne $Directory -or
        $values.present -cnotmatch '^[01]\z' -or $values.inserted -cnotmatch '^[01]\z' -or
        $values.kind -cnotmatch '^(String|ExpandString)\z' -or $values.state -cnotmatch '^(pending|complete)\z') { Fail 'invalid PATH receipt' }
    $expected = if ($values.inserted -ceq '0') { $values.before } elseif ($values.present -ceq '0') { $Directory } else { $values.before + ';' + $Directory }
    if ($values.after -cne $expected -or ($values.present -ceq '0' -and ($values.before -cne '' -or $values.kind -cne 'String'))) { Fail 'inconsistent PATH receipt snapshots' }
    return $values
}
function Set-PathReceiptComplete([string]$Receipt) {
    $state = Join-Path $Receipt 'state'
    Assert-PathReceiptLocation $state $false
    $stage = Join-Path (Split-Path -Parent $Receipt) ('.tapid-path-state-' + [guid]::NewGuid().ToString('N'))
    try {
        [IO.File]::WriteAllText($stage, 'complete', [Text.UTF8Encoding]::new($false))
        [IO.File]::Replace($stage, $state, [System.Management.Automation.Language.NullString]::Value)
        if ([IO.File]::ReadAllText($state) -cne 'complete') { Fail 'PATH receipt read-back failed' }
    } finally { if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Force } }
}
function Configure-UserPath([string]$Directory) {
    if (-not $ConfigurePath) { return }
    $key = Get-PathComparisonKey $Directory $false
    if ($null -eq $key) { Fail "install directory must be a fully qualified literal Windows PATH entry" }
    $receipt = Join-Path $InstallDir '.tapid-path-v1'
    Assert-PathReceiptLocation $receipt $true
    $current = Read-UserPath
    if (Test-Path -LiteralPath $receipt) {
        $values = Read-PathReceipt $receipt $Directory
    } else {
        $entries = if ($current.Present) { $current.Value.Split([char]59) } else { @() }
        $inserted = @($entries | Where-Object { (Get-PathComparisonKey $_) -ceq $key }).Count -eq 0
        $after = if (-not $inserted) { $current.Value } elseif (-not $current.Present) { $Directory } else { $current.Value + ';' + $Directory }
        $values = @{
            manifest = "tapid-path-v1`nwindows`n1`n"; directory = $Directory; before = $current.Value; after = $after
            present = if ($current.Present) { '1' } else { '0' }; kind = $current.Kind
            inserted = if ($inserted) { '1' } else { '0' }; state = 'pending'
        }
        $stage = Join-Path $InstallDir ('.tapid-path-stage-' + [guid]::NewGuid().ToString('N'))
        try {
            New-Item -ItemType Directory -Path $stage | Out-Null
            foreach ($name in $values.Keys) { [IO.File]::WriteAllText((Join-Path $stage $name), $values[$name], [Text.UTF8Encoding]::new($false)) }
            $null = Read-PathReceipt $stage $Directory
            Assert-PathReceiptLocation $receipt $true
            [IO.Directory]::Move($stage, $receipt)
        } finally { if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force } }
    }
    $before = [pscustomobject]@{ Present = $values.present -ceq '1'; Value = $values.before; Kind = $values.kind }
    $after = [pscustomobject]@{ Present = $true; Value = $values.after; Kind = $values.kind }
    if ($values.inserted -ceq '0') { $after.Present = $before.Present }
    $current = Read-UserPath
    if (Test-PathSnapshot $current $after) { }
    elseif ($values.state -ceq 'pending' -and (Test-PathSnapshot $current $before)) { Write-UserPath $after }
    else { Fail 'user PATH changed since the ownership receipt; preserving edits' }
    if (-not (Test-PathSnapshot (Read-UserPath) $after)) { Fail 'user PATH read-back verification failed' }
    Set-PathReceiptComplete $receipt
    $script:PathUpdated = $true
}
function Print-PathGuidance {
    if ($PathUpdated) { Write-Output "User PATH persistence verified. Future sessions must load the updated user environment." }
    Write-Output "To activate Tapid in this PowerShell session, run:"
    $literal = $InstallDir.Replace("'", "''")
    $quoted = "'" + $literal + "'"
    Write-Output ('$env:Path = $(if (@($env:Path -split '';'' ) -icontains ' + $quoted + ') { $env:Path } elseif ($null -eq $env:Path) { ' + $quoted + ' } else { ' + $quoted + ' + '';'' + $env:Path })')
}
function Test-RegularDestination([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Any)) { return }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or -not ($item -is [IO.FileInfo])) {
        Fail "destination must be a regular file and not a reparse point"
    }
}

if ($Repo -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\z') { Fail "repository must be OWNER/REPO" }
if ($ConfigurePath -and $InstallDir.Contains('%')) { Fail 'install directory containing percent characters requires manual PATH activation' }
if ($InstallDir -match ';') { Fail "install directory must not contain the PATH delimiter" }
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
            throw
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
        $checksumLines = @(Get-Content -LiteralPath $checksumsPath | Where-Object { $_ -match $pattern })
        if ($checksumLines.Count -ne 1) { Fail "SHA256SUMS does not contain exactly one checksum for $archive" }
        $null = $checksumLines[0] -match $pattern
        $expected = $Matches[1].ToLowerInvariant()
        $archiveUrl = "$base/$archive"
    } else {
        $recordUrl = if ($env:TAPID_RELEASE_RECORD_URL) { $env:TAPID_RELEASE_RECORD_URL } elseif ($Version -eq 'latest') { 'https://tapid.dev/releases/v1/latest.tsv' } else { "https://tapid.dev/releases/v1/v$Version.tsv" }
        Assert-ReleaseHttpsUrl $recordUrl
        $recordPath = Join-Path $tempRoot 'release.tsv'
        Save-BoundedHttpsFile $recordUrl $recordPath $MAX_RECORD_BYTES
        $signaturePath = Join-Path $tempRoot 'release.tsv.sig'
        Save-BoundedHttpsFile "$recordUrl.sig" $signaturePath $MAX_RECORD_BYTES
        if ((Get-Item -LiteralPath $signaturePath).Length -eq 0) { Fail "release record signature is empty" }
        $bootstrapHash = $BootstrapPins[$target]
        if (-not $bootstrapHash) { Fail "installer bootstrap pins are missing; use a published release installer" }
        $bootstrapArchive = "tapid-$BootstrapVersion-$target.tar.gz"
        $bootstrapPath = Join-Path $tempRoot 'bootstrap.tar.gz'
        Save-BoundedHttpsFile "$BootstrapBaseUrl/$bootstrapArchive" $bootstrapPath $MAX_ARCHIVE_BYTES
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath $bootstrapPath).Hash.ToLowerInvariant() -cne $bootstrapHash) { Fail "bootstrap archive checksum mismatch" }
        $bootstrap = Expand-VerifiedArchive $bootstrapPath (Join-Path $tempRoot 'bootstrap')
        & $bootstrap __verify-release-record $recordPath $signaturePath
        if ($LASTEXITCODE -ne 0) { Fail "release record signature verification failed" }
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
    $extracted = Expand-VerifiedArchive $archivePath $extractRoot
    if (-not $legacyRelease) {
        & $bootstrap __prepare-release-install $Version $BootstrapVersion $archivePath $destination
        if ($LASTEXITCODE -ne 0) { Fail "installer release policy rejected" }
    } else {
        $statePath = Join-Path $InstallDir '.tapid-release-state.json'
        if (Test-Path -LiteralPath $statePath) { Fail "legacy installer cannot replace release-managed state; use tapid upgrade" }
        if (Test-Path -LiteralPath $destination) {
            $installedDigest = (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash
            $selectedDigest = (Get-FileHash -Algorithm SHA256 -LiteralPath $extracted).Hash
            if ($installedDigest -cne $selectedDigest) { Fail "legacy installer cannot replace an existing installation; use tapid upgrade" }
        }
    }
    Copy-Item -LiteralPath $extracted -Destination $staged -Force
    [IO.File]::WriteAllBytes($stagedMarker, [Text.Encoding]::ASCII.GetBytes("tapid-managed-v1`n"))
    Move-Item -LiteralPath $stagedMarker -Destination (Join-Path $InstallDir ".tapid-managed") -Force
    Move-Item -LiteralPath $staged -Destination $destination -Force
    try {
        Configure-UserPath $InstallDir
    } catch {
        Write-Warning "Tapid was installed, but the user PATH could not be updated: $($_.Exception.Message)"
        throw
    }
    Write-Output "Installed Tapid v$Version into $destination"
    Print-PathGuidance
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stagedMarker -Force -ErrorAction SilentlyContinue
}
