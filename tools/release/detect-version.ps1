# Offline inventory only; invoking an executable does not authenticate it.
$ErrorActionPreference = 'Stop'
try {
    if ($args.Count -ne 4) { throw 'Expected -ExecutablePath and -ExpectedVersion' }
    $values = @{}
    for ($i = 0; $i -lt 4; $i += 2) {
        $name = $args[$i]
        if ($name -notin @('-ExecutablePath', '-ExpectedVersion') -or $values.ContainsKey($name)) { throw 'Invalid arguments' }
        $values[$name] = [string]$args[$i + 1]
    }
    $executable = $values['-ExecutablePath']
    $version = $values['-ExpectedVersion']
    if (-not [IO.Path]::IsPathFullyQualified($executable) -or
        -not [IO.File]::Exists($executable) -or
        $version -cnotmatch '\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z') { throw 'Invalid path or stable version' }
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $executable
    $info.ArgumentList.Add('--version')
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    try {
        if (-not $process.Start()) { throw 'Invocation failed' }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $output = $stdout.GetAwaiter().GetResult()
        $null = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0 -or
            (-not [string]::Equals($output, "tapid $version", [StringComparison]::Ordinal) -and
             -not [string]::Equals($output, "tapid $version`n", [StringComparison]::Ordinal) -and
             -not [string]::Equals($output, "tapid $version`r`n", [StringComparison]::Ordinal))) { throw 'Failed or mismatched version' }
    } finally { $process.Dispose() }
    exit 0
} catch {
    [Console]::Error.WriteLine('tapid detection: missing, invalid, failed, or mismatched version')
    exit 1
}
