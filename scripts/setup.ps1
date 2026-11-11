# Run directly to bootstrap Windows before GNU Make is installed.
#Requires -RunAsAdministrator
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw 'Install Microsoft App Installer (winget), then run this script again.'
}
function Install-Package([string]$Id, [string[]]$Extra = @()) {
    & winget install --exact --id $Id --source winget --accept-source-agreements --accept-package-agreements @Extra
    # WinGet returns this code when the requested package is already up to date.
    if ($LASTEXITCODE -notin @(0, -1978335189, 3010)) {
        throw "winget failed for $Id (exit $LASTEXITCODE)"
    }
}

Install-Package 'Microsoft.VisualStudio.2022.BuildTools' @('--override', '--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --add Microsoft.VisualStudio.Component.VC.Tools.ARM64')
# An existing Build Tools installation may not have the C++ workload yet.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vsInstaller = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\setup.exe'
$vsPath = & $vswhere -latest -products Microsoft.VisualStudio.Product.BuildTools -property installationPath
if (-not $vsPath) { throw 'Visual Studio Build Tools installation was not found.' }
$modify = Start-Process -FilePath $vsInstaller -ArgumentList "modify --installPath `"$vsPath`" --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --add Microsoft.VisualStudio.Component.VC.Tools.ARM64" -Wait -PassThru
if ($modify.ExitCode -notin @(0, 3010)) { throw "C++ workload setup failed (exit $($modify.ExitCode))." }
Install-Package 'Rustlang.Rustup'
Install-Package 'NASM.NASM'
Install-Package 'GnuWin32.Make'

# Pick up programs installed during this invocation.
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User') + ';' + $env:Path
$cargoBin = if ($env:CARGO_HOME) { Join-Path $env:CARGO_HOME 'bin' } else { Join-Path $env:USERPROFILE '.cargo\bin' }
$env:Path = "$cargoBin;$env:Path"
$makeBin = Join-Path ${env:ProgramFiles(x86)} 'GnuWin32\bin'
if (Test-Path (Join-Path $makeBin 'make.exe')) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($makeBin -notin ($userPath -split ';')) {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$makeBin", 'User')
    }
    $env:Path = "$makeBin;$env:Path"
}
& rustup toolchain install stable-msvc --profile minimal
if ($LASTEXITCODE -ne 0) { throw 'MSVC Rust toolchain installation failed.' }
& cargo +stable-msvc fetch --locked
if ($LASTEXITCODE -ne 0) { throw 'Cargo dependency download failed.' }
Write-Host 'Dependencies installed. Open a new terminal (restart Windows if requested), then run make demo.'
