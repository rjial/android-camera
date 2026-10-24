# Builds the Windows installer, dist\acam-<VERSION>-windows-x86_64-setup.exe:
# acam.exe + the virtual camera DLL (release), the OBS plugin, then Inno Setup.
#
#   packaging\windows\build-installer.ps1 [-ObsDir <OBS install or unzipped OBS>]
#
# Needs Rust, Visual Studio 2022 with the C++ workload, git, OBS (for the plugin, see
# obs-plugin\build-windows.ps1) and Inno Setup 6.
param(
    [string]$ObsDir = "C:\Program Files\obs-studio"
)
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$version = (Get-Content (Join-Path $root "VERSION") -Raw).Trim()
$dist = Join-Path $root "dist"
$stage = Join-Path $dist "windows-stage"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Write-Host "== acam $version (release)"
cmd /c "cargo build --release --locked --manifest-path `"$root\pc\Cargo.toml`" -p acam -p vcam-source 2>&1"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$target = Join-Path $root "pc\target\release"
Copy-Item (Join-Path $target "acam.exe"), (Join-Path $target "vcam_source.dll") $stage
Copy-Item (Join-Path $root "README.md"), (Join-Path $root "CHANGELOG.md"), (Join-Path $root "LICENSE") $stage

Write-Host "== OBS plugin"
& (Join-Path $root "obs-plugin\build-windows.ps1") -ObsDir $ObsDir
$plugin = Join-Path $stage "obs-plugin\acam-source"
New-Item -ItemType Directory -Force -Path (Join-Path $plugin "bin\64bit") | Out-Null
Copy-Item (Join-Path $root "obs-plugin\build-win\acam-source.dll") (Join-Path $plugin "bin\64bit")
Copy-Item -Recurse (Join-Path $root "obs-plugin\data") $plugin

Write-Host "== installer"
$iscc = @(
    (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source,
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 (ISCC.exe) not found" }
cmd /c "`"$iscc`" /Q /DAppVersion=$version `"/DStageDir=$stage`" `"/O$dist`" `"$PSScriptRoot\acam.iss`" 2>&1"
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
Write-Host "Built $dist\acam-$version-windows-x86_64-setup.exe"
