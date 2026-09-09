#requires -RunAsAdministrator
# fix-wsl2.ps1 - provision WSL2 + Docker Desktop engine on a Windows host
# for the AIOS-LIVE ISO build (live/build.sh).
#
# Usage (run in an elevated PowerShell):
#   powershell -ExecutionPolicy Bypass -File scripts\fix-wsl2.ps1 [-LogPath <file>]
#
# What it does:
#   1. Enables the WSL + VirtualMachinePlatform optional features (no restart yet).
#   2. Installs/updates the WSL2 kernel (Windows Update path, then manual MSI fallback).
#   3. Starts Docker Desktop and waits for the engine.
#   4. Prints a final readiness report.
#
# Hardware prerequisite THIS script CANNOT fix: boot into the UEFI/BIOS setup
# and enable "Intel Virtualization Technology (VT-x)" (plus "VT-d" if present),
# then reboot. Everything else is handled here.

param([string]$LogPath = "")

if ($LogPath) {
    Start-Transcript -Path $LogPath -Force -ErrorAction SilentlyContinue | Out-Null
}
Write-Host ("fix-wsl2.ps1 started at {0}" -f (Get-Date -Format o))

$ErrorActionPreference = "Continue"
$stepsOk = @()

function Write-Step([string]$name) {
    Write-Host ("`n=== {0} ===" -f $name) -ForegroundColor Cyan
}

Write-Step "1/4 enable optional features (WSL, VirtualMachinePlatform)"
dism /online /Enable-Feature /FeatureName:Microsoft-Windows-Subsystem-Linux /All /NoRestart | Out-Host
dism /online /Enable-Feature /FeatureName:VirtualMachinePlatform /All /NoRestart | Out-Host

Write-Step "2/4 install/update WSL2 kernel"
wsl --update | Out-Host
if ($LASTEXITCODE -ne 0) {
    Write-Host ("wsl --update failed ({0}); trying 'wsl --install --no-distribution' (Store WSL + kernel)..." -f $LASTEXITCODE) -ForegroundColor Yellow
    wsl --install --no-distribution | Out-Host
}
$kernel = "C:\Windows\System32\lxss\tools\kernel"
if (-not (Test-Path $kernel) -and $LASTEXITCODE -ne 0) {
    Write-Host "kernel still missing; trying manual kernel MSI (aka.ms/wslkernel)..." -ForegroundColor Yellow
    $msi = Join-Path $env:TEMP "wsl_update_x64.msi"
    try {
        Invoke-WebRequest -Uri "https://aka.ms/wslkernel" -OutFile $msi -UseBasicParsing
        Start-Process msiexec.exe -ArgumentList ("/i `"{0}`" /qn /norestart" -f $msi) -Wait
        Remove-Item $msi -ErrorAction SilentlyContinue
    }
    catch {
        Write-Host ("kernel MSI install failed: {0}" -f $_.Exception.Message) -ForegroundColor Red
    }
}
if (Test-Path $kernel) {
    Write-Host "WSL2 kernel present at $kernel" -ForegroundColor Green
    $stepsOk += "WSL2 kernel present"
}
else {
    Write-Host "WSL2 kernel NOT found yet - re-run this script after the reboot." -ForegroundColor Yellow
}

Write-Step "3/4 virtualization check"
$virt = (Get-CimInstance Win32_Processor).VirtualizationFirmwareEnabled
$hv = (Get-CimInstance Win32_ComputerSystem).HypervisorPresent
Write-Host ("VirtualizationFirmwareEnabled: {0} | HypervisorPresent: {1}" -f $virt, $hv)
if (-not $hv) {
    Write-Output "`n!! virtualization is DISABLED in firmware (VT-x off)."
    Write-Output "   Boot into UEFI/BIOS setup, enable 'Intel Virtualization Technology (VT-x)' [+ 'VT-d'],"
    Write-Output "   save & reboot. A reboot is required for both the features and VT-x."
    Write-Output "   After reboot, run: docker info  or  scripts/fix-wsl2.ps1 again."
    $stepsOk += "UEFI: enable VT-x, reboot"
}
else {
    $stepsOk += "hypervisor online"
}

Write-Step "4/4 start Docker Desktop"
$dd = "C:\Program Files\Docker\Docker\Docker Desktop.exe"
if (Test-Path $dd) {
    $ddProc = Get-Process -Name "Docker Desktop" -ErrorAction SilentlyContinue
    if (-not $ddProc) {
        Start-Process $dd
    }
    $ok = $false
    $i = 0
    while ($i -lt 60) {
        Start-Sleep -Seconds 5
        $sv = docker info --format '{{.ServerVersion}}' 2>$null
        if ($LASTEXITCODE -eq 0 -and $sv) {
            $ok = $true
            Write-Host ("engine up: {0}" -f $sv.Trim()) -ForegroundColor Green
            break
        }
        $i = $i + 1
    }
    if ($ok) {
        $stepsOk += "docker engine online"
    }
    else {
        Write-Host "docker engine did not come up in 5 min. See Docker Desktop logs." -ForegroundColor Yellow
    }
}
else {
    Write-Host ("Docker Desktop not found at {0}. Install it first." -f $dd) -ForegroundColor Yellow
}

Write-Host "`n=== READINESS REPORT ===" -ForegroundColor Cyan
$stepsOk | ForEach-Object { Write-Host (" - {0}" -f $_) -ForegroundColor Green }
Write-Host "`nOnce the docker engine is online, build the ISO with:"
Write-Host "  powershell -ExecutionPolicy Bypass -File scripts\build-live-iso.ps1" -ForegroundColor Green

if ($LogPath) {
    Stop-Transcript -ErrorAction SilentlyContinue | Out-Null
}