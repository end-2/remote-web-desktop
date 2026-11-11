$ErrorActionPreference = 'Stop'
$repoDir = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$executables = @(
    (Join-Path $repoDir 'target\release\remote-web-desktop.exe'),
    (Join-Path $repoDir 'target\debug\remote-web-desktop.exe')
)
$servers = @(Get-Process -Name 'remote-web-desktop' -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -in $executables })
if ($servers.Count -eq 0) {
    Write-Host 'Not running.'
    exit 0
}
foreach ($server in $servers) {
    if (-not $server.HasExited) {
        Stop-Process -InputObject $server
        if (-not $server.WaitForExit(5000)) {
            throw "Server PID $($server.Id) is still running."
        }
    }
}
Write-Host 'Stopped.'
