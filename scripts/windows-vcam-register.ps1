# Installs and registers (or unregisters) the Milestone 1 ACAM test-pattern virtual camera COM
# server (pc/vcam-source) so MFCreateVirtualCamera can find it by CLSID. Run from an elevated
# (Administrator) PowerShell.
#
# Why it installs to C:\Users\Public instead of registering the build output in place: the DLL is
# loaded by the Windows "Frame Server" and "Frame Server Monitor" services (Local Service / Local
# System). Those accounts cannot read a build directory under C:\Users\<you>\..., so registering it
# there makes IMFVirtualCamera::Start fail with E_ACCESSDENIED. C:\Users\Public is readable by those
# services AND writable by the normal user, so during Milestone 1 bring-up the DLL can be refreshed
# without admin — only the one-time CLSID registration below needs admin (it writes to HKLM\...\CLSID).
#
#   scripts\windows-vcam-register.ps1 -Register    build (if needed), copy to Program Files, register
#   scripts\windows-vcam-register.ps1 -Unregister  unregister and remove the installed copy
param(
    [switch]$Register,
    [switch]$Unregister,
    [string]$Configuration = "debug"
)
$ErrorActionPreference = "Stop"

if (-not $Register -and -not $Unregister) {
    Write-Error "usage: windows-vcam-register.ps1 -Register | -Unregister [-Configuration debug|release]"
    exit 2
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$buildDll = Join-Path $repoRoot "pc\target\x86_64-pc-windows-msvc\$Configuration\vcam_source.dll"
$installDir = Join-Path $env:PUBLIC "acam"
$installedDll = Join-Path $installDir "vcam_source.dll"

if ($Register) {
    if (-not (Test-Path $buildDll)) {
        Write-Host "Building vcam-source ($Configuration)..."
        $cargoArgs = @("build", "-p", "vcam-source", "--target", "x86_64-pc-windows-msvc")
        if ($Configuration -eq "release") { $cargoArgs += "--release" }
        & cargo @cargoArgs --manifest-path "$repoRoot\pc\Cargo.toml"
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Copy-Item -Force $buildDll $installedDll
    # Clear any Mark of the Web, which would also block the service from loading it.
    Unblock-File $installedDll
    regsvr32 /s $installedDll
    Write-Host "Installed and registered $installedDll"
} else {
    if (Test-Path $installedDll) {
        regsvr32 /s /u $installedDll
        Remove-Item -Force $installedDll
        Write-Host "Unregistered and removed $installedDll"
    } else {
        Write-Host "Nothing to unregister ($installedDll not present)"
    }
}
