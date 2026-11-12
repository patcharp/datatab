# Build DataTab on a Windows PC: the release datatab.exe and the installer.
#
# Run from the root of a DataTab checkout:
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -SkipInstaller
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Jobs 2
#
# Needs: Git, rustup, Visual Studio Build Tools with the C++ workload (it brings
# the Windows SDK and fxc.exe), and Inno Setup 6 (not needed with -SkipInstaller).
# Set CARGO_TARGET_DIR to build outside .\target. Default build jobs: 4.
# Writes <target>\windows-package\ and leaves the tracked tree clean.
# Works on Windows PowerShell 5.1 and PowerShell 7.

param(
    [ValidateRange(1, 256)]
    [int]$Jobs = 4,
    [switch]$SkipInstaller
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Fail([string]$Message) {
    Write-Host "build-windows: $Message" -ForegroundColor Red
    exit 1
}

function Find-Iscc {
    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    $candidates = @()
    if (${env:ProgramFiles(x86)}) { $candidates += (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe') }
    if ($env:ProgramFiles) { $candidates += (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe') }
    if ($env:LOCALAPPDATA) { $candidates += (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe') }
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    return $null
}

# 1. Check the checkout and the tools.
Set-Location -LiteralPath (Join-Path $PSScriptRoot '..')
foreach ($required in @('Cargo.toml', 'resources\windows\installer.iss',
                        'packaging\icons\datatab.ico', 'LICENSE-MIT', 'LICENSE-APACHE')) {
    if (-not (Test-Path -LiteralPath $required)) {
        Fail "missing $required; is this a DataTab checkout?"
    }
}
foreach ($tool in @('git', 'cargo', 'rustc')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        Fail "$tool is not on PATH; install it first (rustup: https://rustup.rs)"
    }
}
$iscc = $null
if (-not $SkipInstaller) {
    $iscc = Find-Iscc
    if (-not $iscc) { Fail 'ISCC.exe (Inno Setup 6) not found; install Inno Setup 6, or pass -SkipInstaller to build only datatab.exe' }
}

$commit = (git rev-parse HEAD).Trim()
$dirty = (git status --porcelain)
$commitNote = $commit
if ($dirty) {
    $commitNote = "$commit (with local changes)"
    Write-Host 'build-windows: note: the checkout has local changes' -ForegroundColor Yellow
}

$version = $null
$inPackageSection = $false
foreach ($line in (Get-Content -LiteralPath 'Cargo.toml')) {
    if ($line -match '^\[') {
        $inPackageSection = ($line.Trim() -eq '[workspace.package]')
    } elseif ($inPackageSection -and ($line -match '^version\s*=\s*"([^"]+)"')) {
        $version = $Matches[1]
        break
    }
}
if (-not $version) { Fail 'could not read the version from [workspace.package] in Cargo.toml' }

$targetDir = 'target'
if ($env:CARGO_TARGET_DIR) { $targetDir = $env:CARGO_TARGET_DIR }
$exe = Join-Path $targetDir 'release\datatab.exe'
$stage = Join-Path $targetDir 'windows-package'

# 2. Build the release binary.
$env:CARGO_BUILD_JOBS = "$Jobs"
$buildStart = Get-Date
cargo build -p datatab --release --locked
$buildExit = $LASTEXITCODE
$buildMinutes = [math]::Round(((Get-Date) - $buildStart).TotalMinutes, 1)
if ($buildExit -ne 0) { Fail "cargo build failed with exit code $buildExit; if it ran out of memory, try -Jobs 2 or -Jobs 1" }
if (-not (Test-Path -LiteralPath $exe)) { Fail "cargo finished but $exe is missing" }

# 3. Stage the installer inputs outside the tracked tree, then run Inno Setup.
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null

$isccExit = 'not run'
$setup = $null
if (-not $SkipInstaller) {
    Copy-Item -LiteralPath 'resources\windows\installer.iss' -Destination $stage
    Copy-Item -LiteralPath $exe -Destination $stage
    Copy-Item -LiteralPath 'packaging\icons\datatab.ico' -Destination $stage
    Copy-Item -LiteralPath 'LICENSE-MIT', 'LICENSE-APACHE' -Destination $stage

    # installer.iss reads these. Clear the name and id so a stray value in the
    # environment cannot turn this build into another channel.
    Remove-Item Env:\DATATAB_APP_NAME -ErrorAction SilentlyContinue
    Remove-Item Env:\DATATAB_APP_ID -ErrorAction SilentlyContinue
    $env:DATATAB_APP_VERSION = $version
    & $iscc (Join-Path $stage 'installer.iss')
    $isccExit = $LASTEXITCODE
    if ($isccExit -ne 0) { Fail "ISCC failed with exit code $isccExit" }

    $setup = Join-Path $stage 'output\datatab-windows-amd64-setup.exe'
    if (-not (Test-Path -LiteralPath $setup)) { Fail "ISCC finished but $setup is missing" }
}

# 4. Report.
$exeInfo = (Get-Item -LiteralPath $exe).VersionInfo
$exeHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash.ToLower()
$rustc = (rustc --version).Trim()
$os = Get-CimInstance Win32_OperatingSystem
$exePath = (Resolve-Path -LiteralPath $exe).Path

$lines = @(
    'Windows build report',
    "- commit: $commitNote",
    "- OS: $($os.Caption) $($os.Version)",
    "- $rustc",
    "- version: $version",
    "- cargo build -p datatab --release --locked: exit $buildExit, $buildMinutes min, jobs $Jobs",
    "- datatab.exe ProductName '$($exeInfo.ProductName)', FileVersion '$($exeInfo.FileVersion)', ProductVersion '$($exeInfo.ProductVersion)'",
    "- datatab.exe: $exePath",
    "- datatab.exe SHA-256: $exeHash"
)
if ($SkipInstaller) {
    $lines += '- installer: skipped (-SkipInstaller)'
} else {
    $setupHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $setup).Hash.ToLower()
    $lines += "- ISCC: exit $isccExit"
    $lines += "- datatab-windows-amd64-setup.exe SHA-256: $setupHash"
    $lines += "- installer: $((Resolve-Path -LiteralPath $setup).Path)"
}
$report = $lines -join [Environment]::NewLine
$reportPath = Join-Path $stage 'report.txt'
Set-Content -LiteralPath $reportPath -Value $report -Encoding UTF8
Write-Host ''
Write-Host $report
Write-Host "build-windows: report saved to $reportPath" -ForegroundColor Green
