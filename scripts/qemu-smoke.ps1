# QEMU smoke test for the AIOS freestanding kernel.
#
# Builds the BIOS image headlessly and boots it in QEMU, capturing COM1,
# then asserts the Milestone 3/4 proof lines:
#   - ring-3 user tasks enter via the int 0x80 gate
#   - preemptive scheduler context-switches (frame-copy path)
#   - IPC mailboxes carry traffic between pids
#   - a ring-0 kernel worker thread is preempted too
#
# Exit codes: 0 = all checks passed, 1 = checks failed, 2 = skipped (no QEMU).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\qemu-smoke.ps1 [-Seconds 12]

param([int]$Seconds = 12)

$ErrorActionPreference = "Stop"
$env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")
$root = Split-Path -Parent $PSScriptRoot

# --- 1. Build kernel + BIOS image without launching QEMU -------------------
$env:AIOS_SKIP_QEMU = "1"
Push-Location "$root\aios-kernel-run"
cargo run
if ($LASTEXITCODE -ne 0) { Pop-Location; throw "kernel image build failed" }
Pop-Location
Remove-Item Env:AIOS_SKIP_QEMU

$bios = Join-Path $root "aios-kernel-run\bios.img"

# --- 2. Locate QEMU --------------------------------------------------------
$qemu = $null
if (Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue) {
    $qemu = "qemu-system-x86_64"
} else {
    foreach ($c in @(
        "C:\Program Files\qemu\qemu-system-x86_64.exe",
        "C:\Program Files (x86)\qemu\qemu-system-x86_64.exe")) {
        if (Test-Path $c) { $qemu = $c; break }
    }
}
if (-not $qemu) {
    Write-Host "SKIP: qemu-system-x86_64 not installed; image built OK at $bios"
    exit 2
}

# --- 3. Boot headless, capture COM1 ----------------------------------------
$log = Join-Path $env:TEMP "aios-qemu-smoke.log"
Remove-Item $log -ErrorAction SilentlyContinue
$proc = Start-Process -FilePath $qemu `
    -ArgumentList @("-drive","format=raw,file=$bios","-display","none","-serial","file:$log","-no-reboot") `
    -PassThru
Start-Sleep -Seconds $Seconds
if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }

# --- 4. Assert proof lines -------------------------------------------------
$serial = Get-Content $log -Raw -ErrorAction SilentlyContinue
if (-not $serial) { Write-Host "FAIL: no serial output captured at $log"; exit 1 }

$checks = @(
    @{ Name = "ring3 entry via int 0x80"; Pattern = '\[ring3\] pid \d+ entered ring 3' },
    @{ Name = "IPC mailbox traffic";      Pattern = '\[stats\][^\r\n]*sent=[1-9]' },
    @{ Name = "scheduler switching";      Pattern = '\[stats\] switches=\d+' },
    @{ Name = "kernel worker preempted";  Pattern = '\[ktask\] alive' }
)

$failed = 0
foreach ($c in $checks) {
    if ($serial -match $c.Pattern) { Write-Host ("PASS: " + $c.Name) }
    else { Write-Host ("FAIL: " + $c.Name); $failed++ }
}

Write-Host "--- full serial log: $log"
exit $(if ($failed -eq 0) { 0 } else { 1 })
