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
