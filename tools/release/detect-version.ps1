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
    $bytes = [IO.MemoryStream]::new()
    try {
        if (-not $process.Start()) { throw 'Invocation failed' }
        # Read raw bytes: StreamReader decoding can silently remove a BOM.
        $stdout = $process.StandardOutput.BaseStream.CopyToAsync($bytes)
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $null = $stdout.GetAwaiter().GetResult()
        $output = [Convert]::ToBase64String($bytes.ToArray())
        $null = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0 -or
            ($output -cne [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("tapid $version")) -and
             $output -cne [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("tapid $version`n")) -and
             $output -cne [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("tapid $version`r`n")))) { throw 'Failed or mismatched version' }
    } finally { $bytes.Dispose(); $process.Dispose() }
    exit 0
} catch {
    [Console]::Error.WriteLine('tapid detection: missing, invalid, failed, or mismatched version')
    exit 1
}
