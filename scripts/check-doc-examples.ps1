# Native PowerShell documentation lane; no Unix-to-PowerShell translation.
# Requires PowerShell 7. Best-effort process-tree cleanup, not a sandbox.
param(
    [Parameter(Mandatory)][string]$Binary,
    [Parameter(Mandatory)][string]$ExpectedSha256,
    [Parameter(Mandatory)][string]$ExpectedVersion,
    [Parameter(Mandatory)][string]$ReleaseTag,
    [Parameter(Mandatory)][string]$ReportPath
)
$ErrorActionPreference = 'Stop'
$report = @{schema_version=1; lane='published'; platform=[Environment]::OSVersion.Platform.ToString(); release_tag=$ReleaseTag; status='failed'; commands=@(); assertions=@()}
$root = Join-Path ([IO.Path]::GetTempPath()) ('tapid-doc-native-' + [guid]::NewGuid().ToString('N'))
$originalLocation = Get-Location

function Invoke-BoundedTapid([string[]]$Arguments) {
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $installed).Hash.ToLowerInvariant() -cne $ExpectedSha256) {
        $report.failure_class = 'provenance'; throw 'binary digest mismatch'
    }
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $installed
    $info.WorkingDirectory = (Get-Location).Path
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $info.Environment.Clear()
    foreach ($key in @('SystemRoot', 'WINDIR')) {
        $value = [Environment]::GetEnvironmentVariable($key)
        if ($value) { $info.Environment[$key] = $value }
    }
    foreach ($key in @('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_CACHE_HOME', 'TMP', 'TEMP', 'TMPDIR')) { $info.Environment[$key] = $homeDir }
    $info.Environment['PATH'] = $binDir + [IO.Path]::PathSeparator + $(if ($IsWindows) { Join-Path $env:SystemRoot 'System32' } else { '/usr/bin:/bin' })
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    $output = [Text.StringBuilder]::new()
    try {
        $null = $process.Start()
        $readers = @($process.StandardOutput, $process.StandardError)
        $buffers = @([char[]]::new(4096), [char[]]::new(4096))
        $tasks = @($readers[0].ReadAsync($buffers[0], 0, 4096), $readers[1].ReadAsync($buffers[1], 0, 4096))
        $watch = [Diagnostics.Stopwatch]::StartNew()
        while ($null -ne $tasks[0] -or $null -ne $tasks[1] -or -not $process.HasExited) {
            if ($watch.Elapsed.TotalSeconds -gt 120) { $report.failure_class='timeout'; throw 'command exceeded 120 seconds' }
            for ($i=0; $i -lt 2; $i++) {
                if ($null -eq $tasks[$i] -or -not $tasks[$i].IsCompleted) { continue }
                $count = $tasks[$i].GetAwaiter().GetResult()
                if ($count -eq 0) { $tasks[$i] = $null; continue }
                if ($output.Length + $count -gt 65536) { $report.failure_class='output-limit'; throw 'command output exceeded 65536 characters' }
                $null = $output.Append($buffers[$i], 0, $count)
                $tasks[$i] = $readers[$i].ReadAsync($buffers[$i], 0, 4096)
            }
            Start-Sleep -Milliseconds 10
        }
        $process.WaitForExit()
        return @{exit_code=$process.ExitCode; output=$output.ToString()}
    } finally {
        if ($process.Id -and -not $process.HasExited) { $process.Kill($true); $null = $process.WaitForExit(5000) }
        $process.Dispose()
    }
}

try {
    if ($ReleaseTag -cnotmatch '^v[0-9]+\.[0-9]+\.[0-9]+$' -or $ExpectedVersion -cne ('tapid ' + $ReleaseTag.Substring(1))) { throw 'release identity mismatch' }
    if ($ExpectedSha256 -cnotmatch '^[a-f0-9]{64}$' -or (Get-FileHash -Algorithm SHA256 -LiteralPath $Binary).Hash.ToLowerInvariant() -cne $ExpectedSha256) {
        $report.failure_class = 'provenance'; throw 'binary digest mismatch'
    }
    $example = Join-Path $PSScriptRoot '../docs/examples/quickstart.ps1'
    if ((Get-Item -LiteralPath $example).Length -gt 32768) { throw 'example exceeds size limit' }
    $lines = @(Get-Content -LiteralPath $example | Where-Object { $_.Trim() })
    if ($lines.Count -eq 0 -or $lines.Count -gt 16) { throw 'invalid example command count' }
    foreach ($line in $lines) {
        if ($line -cnotmatch '^(New-Item -ItemType Directory [a-z][a-z0-9-]*|Set-Location [a-z][a-z0-9-]*|tapid (init|i is-char|install --offline --frozen))$') { throw 'unsupported native command vocabulary' }
    }
    $binDir = Join-Path $root 'bin'
    $homeDir = Join-Path $root 'home'
    $project = Join-Path $root 'project'
    $null = New-Item -ItemType Directory -Path $binDir, $homeDir, $project
    $installed = Join-Path $binDir 'tapid.exe'
    Copy-Item -LiteralPath $Binary -Destination $installed
    Set-Location $project
    $version = Invoke-BoundedTapid @('--version')
    if ($version.exit_code -ne 0 -or $version.output.Trim() -cne $ExpectedVersion) { $report.failure_class='provenance'; throw 'binary version mismatch' }
    $report.binary = @{path=$Binary; sha256=$ExpectedSha256; version=$version.output.Trim()}
    $report.script_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $example).Hash.ToLowerInvariant()
    foreach ($line in $lines) {
        $command = @{command=$line; exit_code=$null; output=''}
        $report.commands += $command
        $before = $null
        if ($line -eq 'tapid install --offline --frozen') { $before = (Get-FileHash -Algorithm SHA256 -LiteralPath 'tapid.lock').Hash }
        if ($line.StartsWith('tapid ')) {
            $result = Invoke-BoundedTapid ($line.Split(' ')[1..($line.Split(' ').Count-1)])
            $command.exit_code = $result.exit_code
            $command.output = $result.output
            if ($result.exit_code -ne 0) { $report.failure_class='command'; throw 'native command failed' }
        } else {
            # The entire vocabulary was validated before any command executed.
            & ([scriptblock]::Create($line)) | Out-Null
            $command.exit_code = 0
        }
        if ($before -and (Get-FileHash -Algorithm SHA256 -LiteralPath 'tapid.lock').Hash -cne $before) { $report.failure_class='assertion'; throw 'frozen lockfile changed' }
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath $installed).Hash.ToLowerInvariant() -cne $ExpectedSha256) { $report.failure_class='provenance'; throw 'binary changed' }
    }
    foreach ($path in @('package.json', 'node_modules/is-char/package.json')) { if ((Get-Item -LiteralPath $path).Length -gt 1048576) { throw 'manifest exceeds size limit' } }
    $manifest = Get-Content -Raw -LiteralPath 'package.json' | ConvertFrom-Json
    $package = Get-Content -Raw -LiteralPath 'node_modules/is-char/package.json' | ConvertFrom-Json
    if (-not $manifest.dependencies.'is-char' -or $package.name -cne 'is-char' -or -not $before) { $report.failure_class='assertion'; throw 'package/replay assertions failed' }
    $report.assertions = @('manifest', 'is-char-installed', 'frozen-lockfile-unchanged')
    $report.status = 'passed'
} catch {
    $report.error = $_.Exception.Message
    if (-not $report.failure_class) { $report.failure_class = 'execution' }
} finally {
    Set-Location $originalLocation
    if (Test-Path -LiteralPath $root) { Remove-Item -Recurse -Force -LiteralPath $root }
    $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $ReportPath -Encoding utf8
}
if ($report.status -ne 'passed') { exit 1 }
