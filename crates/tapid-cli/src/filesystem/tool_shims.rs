//! Shell-specific global launcher serialization. Bin validation stays in the linker.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tapid_linker::ShimStrategy;
pub(crate) fn render(
    root: &Path,
    command: &str,
    strategy: ShimStrategy,
) -> Result<BTreeMap<String, String>, String> {
    let tapid = std::env::current_exe().map_err(|e| e.to_string())?;
    let tapid = tapid
        .to_str()
        .ok_or_else(|| String::from("tool launchers require a UTF-8 Tapid path"))?;
    let root = root
        .to_str()
        .ok_or_else(|| String::from("tool launchers require a UTF-8 tools path"))?;
    if [tapid, root].iter().any(|p| p.contains(['\r', '\n', '"'])) {
        return Err(String::from(
            "unsupported quote or line break in launcher path",
        ));
    }
    let mut result = BTreeMap::new();
    match strategy {
        ShimStrategy::UnixSymlink => {
            let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
            result.insert(
                command.to_owned(),
                format!(
                    "#!/bin/sh\nexec {} __tool-exec --tools-dir {} --command {} -- \"$@\"\n",
                    quote(tapid),
                    quote(root),
                    quote(command)
                ),
            );
        }
        ShimStrategy::WindowsCmdAndPowerShell => {
            use crate::filesystem::tree::{cmd_batch_path, powershell_single_quoted};
            let output = PathBuf::from(command);
            result.insert(output.with_extension("cmd").to_string_lossy().into_owned(),
                    format!("@echo off\r\n@setlocal DisableDelayedExpansion\r\n\"{}\" __tool-exec --tools-dir \"{}\" --command \"{}\" -- %*\r\n@exit /b %errorlevel%\r\n", cmd_batch_path(tapid), cmd_batch_path(root), cmd_batch_path(command)));
            // PowerShell 5 native forwarding drops empty strings. Construct
            // the native argv using the Windows CRT quoting convention.
            result.insert(output.with_extension("ps1").to_string_lossy().into_owned(),
                    format!(r#"$arguments = @('__tool-exec', '--tools-dir', '{}', '--command', '{}', '--') + $args
$quoted = foreach ($argument in $arguments) {{
    $escaped = [regex]::Replace([string]$argument, '(\\*)"', '$1$1\"')
    $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
    '"' + $escaped + '"'
}}
$start = New-Object System.Diagnostics.ProcessStartInfo
$start.FileName = '{}'
$start.UseShellExecute = $false
$start.WorkingDirectory = $ExecutionContext.SessionState.Path.CurrentFileSystemLocation.Path
$start.Arguments = $quoted -join ' '
$process = [System.Diagnostics.Process]::Start($start)
$process.WaitForExit()
$code = $process.ExitCode
$process.Dispose()
exit $code
"#, powershell_single_quoted(root), powershell_single_quoted(command), powershell_single_quoted(tapid)));
        }
    }
    Ok(result)
}
