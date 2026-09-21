# flash-usb.ps1 - write a bootable AIOS image to a physical USB stick.
#
# The bare-metal kernel ISO is isohybrid (Limine MBR installed via
# `limine bios-install`), so a raw byte copy boots on:
#   - legacy BIOS via the Limine MBR stage
#   - UEFI via the ESP partition's BOOTX64.EFI
#
# Requires: PowerShell run as Administrator (raw disk access).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\flash-usb.ps1 [-Image <path>] [-DiskNumber <n>]
#
# Examples:
#   powershell -ExecutionPolicy Bypass -File scripts\flash-usb.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\flash-usb.ps1 -Image out\aios-kernel-usb.img -DiskNumber 3

$ErrorActionPreference = "Stop"
$env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")

$repoRoot = Split-Path -Parent $PSScriptRoot
$defaultImage = Join-Path $repoRoot "aios-kernel-run\out\aios-kernel-usb.img"

function Write-Info  { Write-Host "[AIOS]" -ForegroundColor Cyan -NoNewline; Write-Host " $args" }
function Write-Ok    { Write-Host "[  OK]" -ForegroundColor Green -NoNewline; Write-Host " $args" }
function Write-Warn  { Write-Host "[WARN]" -ForegroundColor Yellow -NoNewline; Write-Host " $args" }
function Write-Err   { Write-Host "[FAIL]" -ForegroundColor Red -NoNewline; Write-Host " $args" }

# --- elevation check ------------------------------------------------------
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Err "Administrator privileges are required to write to a physical disk."
    Write-Info "Relaunching as Administrator..."
    Start-Process powershell.exe -Verb RunAs -ArgumentList "-ExecutionPolicy Bypass -File `"$PSCommandPath`" $(if($Image){'-Image'} $Image) $(if($DiskNumber){'-DiskNumber'} $DiskNumber)"
    exit 0
}

# --- parse args -----------------------------------------------------------
$Image = if ($Image) { $Image } else { $defaultImage }
if (-not (Test-Path $Image)) {
    Write-Err "Image not found: $Image"
    Write-Info "Build it first:  cd aios-core\aios-kernel-run && cargo run (or set AIOS_SKIP_QEMU=1)"
    exit 1
}

# --- pick the target disk -------------------------------------------------
$disks = Get-Disk | Where-Object { $_.BusType -eq 'USB' -and $_.PartitionStyle -ne 'RAW' }
if (-not $disks) {
    # also look at removable disks regardless of bus type
    $disks = Get-Disk | Where-Object { $_.IsRemovable -or $_.BusType -eq 'USB' }
}
if (-not $disks) {
    Write-Err "No removable/USB disk found."
    exit 1
}

Write-Info "Candidate USB disks:"
$disks | ForEach-Object {
    $partLabel = if ($_.NumberOfPartitions -gt 0) { "$($_.NumberOfPartitions) partition(s)" } else { "no partitions" }
    Write-Host "  Disk $($_.Number): $($_.FriendlyName)  $([math]::Round($_.Size/1GB,1)) GB  $partLabel"
}

$targetNum = if ($DiskNumber) { $DiskNumber } else {
    $def = $disks | Sort-Object Number | Select-Object -First 1
    Write-Host ""
    $ans = Read-Host "Enter DiskNumber to flash (default $($def.Number))"
    if ($ans -match '^\d+$') { [int]$ans } else { $def.Number }
}
$target = Get-Disk -Number $targetNum -ErrorAction Stop
if ($target.FriendlyName -notlike '*Kingston*' -and $target.BusType -ne 'USB') {
    Write-Warn "Selected disk is '$($target.FriendlyName)' (BusType=$($target.BusType)). Make sure it is the USB stick you intend to flash."
}
Write-Host ""
Write-Warn "YOU ARE ABOUT TO DESTROY ALL DATA ON DISK $targetNum ($($target.FriendlyName))."
$confirm = Read-Host "Type YES to continue"
if ($confirm -ne 'YES') { Write-Info "Aborted."; exit 0 }

# --- write ---------------------------------------------------------------
$imgSize = (Get-Item $Image).Length
$imgHash = (Get-FileHash $Image -Algorithm SHA256).Hash
$stream = [System.IO.File]::Open($Image, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read)
$out = [System.IO.File]::Open("\\.\PhysicalDrive$targetNum", [System.IO.FileMode]::Write, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
$buf = New-Object byte[] 1MB
$written = 0; $chunk = 0; $sw = [Diagnostics.Stopwatch]::StartNew()
try {
    while (($read = $stream.Read($buf, 0, $buf.Length)) -gt 0) {
        $out.Write($buf, 0, $read)
        $written += $read; $chunk++
        $pct = [math]::Round(100.0 * $written / $imgSize, 1)
        $mb = [math]::Round($written / 1MB, 1)
        Write-Info "Flashed $mb MB / $([math]::Round($imgSize/1MB,1)) MB ($pct%) ..."
    }
} finally {
    $out.Flush(); $out.Close(); $stream.Close()
    $sw.Stop()
}

# --- verify --------------------------------------------------------------
# Compare SHA-256 of the first image-sized bytes of the disk against the image.
$sha = [System.Security.Cryptography.SHA256Managed]::new()
$dstream = [System.IO.File]::Open("\\.\PhysicalDrive$targetNum", [System.IO.FileMode]::Read, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
$remaining = $imgSize; $hbuf = New-Object byte[] 1MB; $hashed = 0
while ($remaining -gt 0) {
    $toRead = [Math]::Min($hbuf.Length, $remaining)
    $n = $dstream.Read($hbuf, 0, $toRead)
    if ($n -le 0) { break }
    $sha.TransformBlock($hbuf, 0, $n, $hbuf, 0) | Out-Null
    $hashed += $n; $remaining -= $n
}
$sha.TransformFinalBlock([byte[]]::Empty, 0, 0)
$diskHash = [System.BitConverter]::ToString($sha.Hash).Replace('-','')
$dstream.Close()
Write-Host ""
Write-Ok "Wrote $([math]::Round($written/1MB,1)) MB to \\.\PhysicalDrive$targetNum in $($sw.Elapsed.TotalSeconds)s"
Write-Info "Image SHA-256 : $imgHash"
Write-Info "Disk   SHA-256: $diskHash (first $([math]::Round($hashed/1MB,1)) MB)"
if ($imgHash -eq $diskHash) {
    Write-Ok "SHA-256 match — flash verified."
} else {
    Write-Warn "SHA-256 mismatch — retrying full read..."
    $dstream = [System.IO.File]::Open("\\.\PhysicalDrive$targetNum", [System.IO.FileMode]::Read, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    $buf2 = New-Object byte[] $imgSize
    $dstream.Read($buf2, 0, $buf2.Length); $dstream.Close()
    $diskHash2 = [System.BitConverter]::ToString($sha.ComputeHash($buf2)).Replace('-','')
    if ($imgHash -eq $diskHash2) { Write-Ok "SHA-256 match on full read — flash verified." }
    else { Write-Err "Flash verification failed." }
}
$marker = New-Object byte[] 512
$dstream = [System.IO.File]::Open("\\.\PhysicalDrive$targetNum", [System.IO.FileMode]::Read, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
$dstream.Read($marker, 0, 512); $dstream.Close()
$sig = "0x{0:X2}{1:X2}" -f $marker[510], $marker[511]
Write-Info "MBR signature at offset 510: $sig (expected 0x55AA)"
Write-Ok "Flash complete. Boot the USB stick and select AIOS kernel."
