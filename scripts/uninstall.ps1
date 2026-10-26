[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $HOME ".local\bin")
)

$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    throw "tapid uninstaller: $Message"
}

function Test-AbsolutePath([string]$Path) {
    if ([string]::IsNullOrEmpty($Path) -or $Path -match '[\r\n]') { return $false }
    if ([IO.Path]::DirectorySeparatorChar -eq '\') {
        return $Path -match '^(?:[A-Za-z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+(?:[\\/].*)?\z)'
    }
    return $Path.StartsWith('/')
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
if (-not (Test-AbsolutePath $InstallDir)) {
    Fail "install directory must be an absolute path"
}

$destination = Join-Path $InstallDir "tapid.exe"
$marker = Join-Path $InstallDir ".tapid-managed"

if (-not (Test-Path -LiteralPath $marker -PathType Any)) {
    Fail "refusing foreign install marker: $marker"
}
$markerItem = Get-Item -LiteralPath $marker -Force
if (($markerItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or -not ($markerItem -is [IO.FileInfo])) {
    Fail "refusing foreign install marker: $marker"
}
if ([IO.File]::ReadAllText($marker) -cne "tapid-managed-v1`n") {
    Fail "refusing invalid install marker: $marker"
}

# Validate the executable before changing persistence, including missing-binary retries.
if (Test-Path -LiteralPath $destination -PathType Any) {
    $item = Get-Item -LiteralPath $destination -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or -not ($item -is [IO.FileInfo])) { Fail 'refusing unsafe executable' }
}
$receipt = Join-Path $InstallDir '.tapid-path-v1'
if (Get-Item -LiteralPath $receipt -Force -ErrorAction SilentlyContinue) {
    Assert-PathReceiptLocation $receipt $true
    $values = Read-PathReceipt $receipt $InstallDir
    if ($values.inserted -ceq '1') {
        $before = [pscustomobject]@{ Present = $values.present -ceq '1'; Value = $values.before; Kind = $values.kind }
        $after = [pscustomobject]@{ Present = $true; Value = $values.after; Kind = $values.kind }
        $current = Read-UserPath
        if (Test-PathSnapshot $current $before) { }
        elseif (Test-PathSnapshot $current $after) {
            # Re-read immediately before replacing; never infer ownership from equivalence.
            if (-not (Test-PathSnapshot (Read-UserPath) $after)) { Fail 'user PATH changed during cleanup' }
            Write-UserPath $before
        } else { Fail 'user PATH changed since installation; preserving PATH and receipt for cleanup' }
        if (-not (Test-PathSnapshot (Read-UserPath) $before)) { Fail 'user PATH cleanup read-back verification failed' }
    }
    $null = Read-PathReceipt $receipt $InstallDir
    Remove-Item -LiteralPath $receipt -Recurse -Force
    Write-Output 'Removed Tapid-owned PATH configuration'
}

if (-not (Test-Path -LiteralPath $destination -PathType Any)) {
    Write-Output "Tapid is not installed at $destination"
    Remove-Item -LiteralPath $marker -Force
    exit 0
}

$item = Get-Item -LiteralPath $destination -Force
if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    Fail "refusing to remove symlink or reparse point: $destination"
}
if (-not ($item -is [IO.FileInfo])) {
    Fail "refusing to remove non-regular path: $destination"
}

Remove-Item -LiteralPath $destination -Force
Write-Output "Removed $destination"
Remove-Item -LiteralPath $marker -Force
