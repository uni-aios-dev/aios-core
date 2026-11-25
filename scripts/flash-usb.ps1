# flash-usb.ps1 - write a bootable AIOS image to a physical USB stick.
#
# The kernel ISO is an isohybrid image (Limine MBR installed with
# `limine bios-install`), so a raw byte copy boots on both legacy BIOS and
# UEFI. The script locks and dismounts every volume on the target disk, writes
# sector-aligned blocks, flushes, reinitializes the device with a bounded
# recovery path, and verifies the whole image by SHA-256 read-back.
#
# Requires: PowerShell run as Administrator (raw disk access).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\flash-usb.ps1 [-Image <path>] [-DiskNumber <n>] [-Force]
#
# Environment:
#   AIOS_FLASH_RECOVERY_TIMEOUT  reinit/rescan timeout in seconds (default 30)

param(
    [string]$Image,
    [int]$DiskNumber,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$env:Path = [System.Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [System.Environment]::GetEnvironmentVariable('Path', 'User')

function Write-Info { Write-Host '[AIOS]' -ForegroundColor Cyan -NoNewline; Write-Host " $args" }
function Write-Ok   { Write-Host '[  OK]' -ForegroundColor Green -NoNewline; Write-Host " $args" }
function Write-Warn { Write-Host '[WARN]' -ForegroundColor Yellow -NoNewline; Write-Host " $args" }
function Write-Err  { Write-Host '[FAIL]' -ForegroundColor Red -NoNewline; Write-Host " $args" }

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class AiOsFlashNative
{
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool DeviceIoControl(
        SafeFileHandle hDevice,
        uint dwIoControlCode,
        IntPtr lpInBuffer,
        uint nInBufferSize,
        IntPtr lpOutBuffer,
        uint nOutBufferSize,
        out uint lpBytesReturned,
        IntPtr lpOverlapped);
}
'@ -ErrorAction Stop

$FSCTL_LOCK_VOLUME    = [uint32]0x00090018
$FSCTL_DISMOUNT_VOLUME = [uint32]0x00090020
$FSCTL_UNLOCK_VOLUME  = [uint32]0x0009001C
$SECTOR_SIZE = 512

function Get-RecoveryTimeout {
    $raw = $env:AIOS_FLASH_RECOVERY_TIMEOUT
    $parsed = 0
    if ($raw -and [int]::TryParse($raw, [ref]$parsed) -and $parsed -gt 0) { return $parsed }
    return 30
}

function Invoke-VolumeIoctl {
    param(
        [Parameter(Mandatory = $true)]$Stream,
        [Parameter(Mandatory = $true)][uint32]$Code,
        [string]$What
    )
    $returned = [uint32]0
    $ok = [AiOsFlashNative]::DeviceIoControl(
        $Stream.SafeFileHandle,
        $Code,
        [IntPtr]::Zero,
        0,
        [IntPtr]::Zero,
        0,
        [ref]$returned,
        [IntPtr]::Zero)
    if (-not $ok) {
        $err = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        throw "$What failed ( Win32 error $err )"
    }
}

function Get-VolumePathsForDisk {
    param([Parameter(Mandatory = $true)][int]$Number)
    $paths = New-Object 'System.Collections.Generic.HashSet[string]'
    Get-Partition -DiskNumber $Number -ErrorAction SilentlyContinue | ForEach-Object {
        $part = $_
        if ($part.DriveLetter) { [void]$paths.Add("$($part.DriveLetter):\") }
        foreach ($access in @($part.AccessPaths)) {
            if ($access -like '\\?\Volume*') { [void]$paths.Add("$access") }
            elseif ($access -match '^[A-Za-z]:\\?$') { [void]$paths.Add("$access") }
        }
    }
    return @($paths)
}

function Lock-And-DismountVolumes {
    param([Parameter(Mandatory = $true)][int]$Number)
    $handles = @()
    foreach ($path in (Get-VolumePathsForDisk -Number $Number)) {
        try {
            $stream = [System.IO.File]::Open(
                $path,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::ReadWrite)
            Invoke-VolumeIoctl -Stream $stream -Code $FSCTL_LOCK_VOLUME -What "Locking '$path'"
            Invoke-VolumeIoctl -Stream $stream -Code $FSCTL_DISMOUNT_VOLUME -What "Dismounting '$path'"
            $handles += $stream
            Write-Info "locked + dismounted $path"
        } catch {
            if ($stream) { $stream.Dispose() }
            Write-Warn "could not lock/dismount '$path': $($_.Exception.Message)"
        }
    }
    return $handles
}

function Release-VolumeLocks {
    param([object[]]$Handles)
    foreach ($stream in @($Handles)) {
        try {
            Invoke-VolumeIoctl -Stream $stream -Code $FSCTL_UNLOCK_VOLUME -What 'Unlocking volume' | Out-Null
        } catch { }
        try { $stream.Dispose() } catch { }
    }
}

function Invoke-BoundedRecovery {
    param([Parameter(Mandatory = $true)][int]$Number)
    $timeout = Get-RecoveryTimeout
    $job = Start-Job -ArgumentList $Number -ScriptBlock {
        param($n)
        try { Update-Disk -Number $n -ErrorAction Stop; 'updated' }
        catch { "update failed: $($_.Exception.Message)" }
    }
    if (Wait-Job -Job $job -Timeout $timeout) {
        $result = Receive-Job -Job $job
        if ($result) { Write-Info "disk reinit: $result" }
    } else {
        Stop-Job -Job $job
        Write-Warn "Update-Disk exceeded ${timeout}s; continuing with PnP rescan"
    }
    Remove-Job -Job $job -Force

    $job = Start-Job -ScriptBlock {
        try { & pnputil.exe /rescan-disks 2>&1 | Out-Null; 'rescan complete' }
        catch { "rescan failed: $($_.Exception.Message)" }
    }
    if (Wait-Job -Job $job -Timeout $timeout) {
        $result = Receive-Job -Job $job
        if ($result) { Write-Info "PnP: $result" }
    } else {
        Stop-Job -Job $job
        Write-Warn "PnP rescan exceeded ${timeout}s"
    }
    Remove-Job -Job $job -Force
}

function Open-RawDisk {
    param(
        [Parameter(Mandatory = $true)][int]$Number,
        [Parameter(Mandatory = $true)][System.IO.FileAccess]$Access
    )
    $path = "\\.\PhysicalDrive$Number"
    return [System.IO.File]::Open(
        $path,
        [System.IO.FileMode]::Open,
        $Access,
        [System.IO.FileShare]::ReadWrite)
}

function Wait-ForDiskReady {
    param([Parameter(Mandatory = $true)][int]$Number)
    for ($attempt = 1; $attempt -le 8; $attempt++) {
        try {
            $probe = Open-RawDisk -Number $Number -Access Read
            $probe.Dispose()
            return
        } catch {
            if ($attempt -eq 4) { Invoke-BoundedRecovery -Number $Number }
            Start-Sleep -Milliseconds 500
        }
    }
    throw "PhysicalDrive$Number did not become ready after reinitialization"
}

# --- elevation check (before any destructive action) -----------------------
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $repoRoot = Split-Path -Parent $PSScriptRoot
    if (-not $Image) { $Image = Join-Path $repoRoot 'aios-kernel-run\out\aios-kernel-usb.img' }
    $Image = (Resolve-Path -LiteralPath $Image -ErrorAction SilentlyContinue).Path
    if (-not $Image) { Write-Err "Image not found: $Image"; exit 1 }

    if (-not $DiskNumber) {
        $disks = @(Get-Disk | Where-Object { $_.BusType -eq 'USB' -and $_.PartitionStyle -ne 'RAW' })
        if (-not $disks) { $disks = @(Get-Disk | Where-Object { $_.IsRemovable -or $_.BusType -eq 'USB' }) }
        if (-not $disks) { Write-Err 'No removable/USB disk found.'; exit 1 }
        $DiskNumber = ($disks | Sort-Object Number | Select-Object -First 1).Number
    }

    $childArgs = @('-ExecutionPolicy', 'Bypass', '-NoProfile', '-File', "`"$PSCommandPath`"", '-Image', "`"$Image`"", '-DiskNumber', $DiskNumber)
    if ($Force) { $childArgs += '-Force' }
    $outLog = Join-Path $env:TEMP ("aios-flash-{0}.out" -f $PID)
    $errLog = Join-Path $env:TEMP ("aios-flash-{0}.err" -f $PID)
    Write-Info 'Relaunching as Administrator...'
    $child = Start-Process powershell.exe -Verb RunAs -ArgumentList $childArgs -PassThru -Wait -RedirectStandardOutput $outLog -RedirectStandardError $errLog
    if (Test-Path $outLog) { Get-Content -LiteralPath $outLog; Remove-Item -LiteralPath $outLog -Force }
    if (Test-Path $errLog) { Get-Content -LiteralPath $errLog; Remove-Item -LiteralPath $errLog -Force }
    exit $child.ExitCode
}

# --- validate -------------------------------------------------------------
$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $Image) { $Image = Join-Path $repoRoot 'aios-kernel-run\out\aios-kernel-usb.img' }
if (-not (Test-Path -LiteralPath $Image)) {
    Write-Err "Image not found: $Image"
    Write-Info 'Build it first:  cd aios-core\aios-kernel-run && cargo run (or set AIOS_SKIP_QEMU=1)'
    exit 1
}
$Image = (Resolve-Path -LiteralPath $Image).Path

if (-not $DiskNumber) {
    $disks = @(Get-Disk | Where-Object { $_.BusType -eq 'USB' -and $_.PartitionStyle -ne 'RAW' })
    if (-not $disks) { $disks = @(Get-Disk | Where-Object { $_.IsRemovable -or $_.BusType -eq 'USB' }) }
    if (-not $disks) { Write-Err 'No removable/USB disk found.'; exit 1 }
    $DiskNumber = ($disks | Sort-Object Number | Select-Object -First 1).Number
}

$target = Get-Disk -Number $DiskNumber -ErrorAction Stop
if ($target.BusType -ne 'USB' -and -not $target.IsRemovable) {
    Write-Warn "Selected disk '$($target.FriendlyName)' is not USB/removable (BusType=$($target.BusType))."
}
if (-not $Force) {
    Write-Host ''
    Write-Warn "YOU ARE ABOUT TO DESTROY ALL DATA ON DISK $DiskNumber ($($target.FriendlyName))."
    $confirm = Read-Host 'Type YES to continue'
    if ($confirm -ne 'YES') { Write-Info 'Aborted.'; exit 0 }
}

$imgSize = (Get-Item -LiteralPath $Image).Length
$imgHash = (Get-FileHash -LiteralPath $Image -Algorithm SHA256).Hash
Write-Info "image: $Image ($imgSize bytes)"
Write-Info "SHA-256: $imgHash"

$volumeLocks = @()
try {
    $volumeLocks = @(Lock-And-DismountVolumes -Number $DiskNumber)

    $inputStream = Open-RawDisk -Number $DiskNumber -Access Read
    $outputStream = Open-RawDisk -Number $DiskNumber -Access Write
    try {
        $buffer = New-Object byte[] (1MB)
        $total = [long]0
        $sw = [Diagnostics.Stopwatch]::StartNew()
        while ($total -lt $imgSize) {
            $want = [int][Math]::Min([long]$buffer.Length, $imgSize - $total)
            $read = 0
            while ($read -lt $want) {
                $n = $inputStream.Read($buffer, $read, $want - $read)
                if ($n -le 0) { throw 'Unexpected end of image file' }
                $read += $n
            }
            $writeLength = $read
            if (($writeLength % $SECTOR_SIZE) -ne 0) {
                $pad = $SECTOR_SIZE - ($writeLength % $SECTOR_SIZE)
                for ($i = $writeLength; $i -lt $writeLength + $pad; $i++) { $buffer[$i] = 0 }
                $writeLength += $pad
            }
            $outputStream.Write($buffer, 0, $writeLength)
            $total += $read
            $pct = [math]::Round(100.0 * $total / $imgSize, 1)
            Write-Info "Flashed $([math]::Round($total / 1MB, 1)) MB / $([math]::Round($imgSize / 1MB, 1)) MB ($pct%) ..."
        }
        $outputStream.Flush($true)
        $sw.Stop()
    } finally {
        $inputStream.Dispose()
        $outputStream.Dispose()
    }

    Release-VolumeLocks -Handles $volumeLocks
    $volumeLocks = @()
    Invoke-BoundedRecovery -Number $DiskNumber
    Wait-ForDiskReady -Number $DiskNumber

    # Full-image read-back: never trust the write path alone.
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $verify = Open-RawDisk -Number $DiskNumber -Access Read
    try {
        $hbuf = New-Object byte[] (1MB)
        $remaining = $imgSize
        while ($remaining -gt 0) {
            $toRead = [int][Math]::Min([long]$hbuf.Length, $remaining)
            $n = $verify.Read($hbuf, 0, $toRead)
            if ($n -le 0) { throw "Short read-back at byte $(($imgSize - $remaining))" }
            [void]$sha.TransformBlock($hbuf, 0, $n, $hbuf, 0)
            $remaining -= $n
        }
        [void]$sha.TransformFinalBlock([byte[]]::Empty, 0, 0)
        $diskHash = [BitConverter]::ToString($sha.Hash).Replace('-', '')
    } finally {
        $verify.Dispose()
        $sha.Dispose()
    }

    Write-Host ''
    Write-Ok "Wrote $([math]::Round($total / 1MB, 1)) MB to PhysicalDrive$DiskNumber in $($sw.Elapsed.TotalSeconds)s"
    Write-Info "Image SHA-256: $imgHash"
    Write-Info "Disk  SHA-256: $diskHash"
    if ($imgHash -ne $diskHash) {
        Write-Err 'SHA-256 mismatch - flash verification failed.'
        exit 1
    }

    $marker = New-Object byte[] 512
    $mstream = Open-RawDisk -Number $DiskNumber -Access Read
    try { [void]$mstream.Read($marker, 0, 512) } finally { $mstream.Dispose() }
    $sig = '0x{0:X2}{1:X2}' -f $marker[510], $marker[511]
    if ($sig -ne '0x55AA') { Write-Warn "MBR signature is $sig (expected 0x55AA)" }
    else { Write-Info "MBR signature: $sig" }

    Write-Ok 'Flash complete and verified. Boot the USB stick and select AIOS kernel.'
} catch {
    Write-Err $_.Exception.Message
    exit 1
} finally {
    Release-VolumeLocks -Handles $volumeLocks
}
