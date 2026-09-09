# build-live-iso.ps1 - build the bootable AIOS-LIVE ISO via Docker (live/build.sh).
#
# Requires:
#   - Docker Desktop engine running (linux containers). If the engine won't
#     start, see scripts/fix-wsl2.ps1 (VT-x must be enabled in the UEFI/BIOS).
#   - A cargo registry cache on the host (%USERPROFILE%\.cargo\registry)
#     because live/build.sh runs with CARGO_NET_OFFLINE=true.
#
# Output: live/out/aios-live.iso  (gitignored)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$live = Join-Path $root "live"
$reg = Join-Path $env:USERPROFILE ".cargo\registry"
$out = Join-Path $live "out"

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    throw "docker not found on PATH"
}

docker info --format '{{.ServerVersion}}' | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "docker engine is not running - start Docker Desktop first"
}

New-Item -ItemType Directory -Force -Path $out | Out-Null

"=== pulling/checking rust:alpine base image ==="
docker pull rust:alpine | Out-Null

"=== launching live/build.sh (aios static-musl + rootfs + initramfs + GRUB ISO) ==="
docker run --rm -it `
    -v "${root}:/src" `
    -v "${live}:/work" `
    -v "${reg}:/usr/local/cargo/registry" `
    rust:alpine `
    sh /work/build.sh

if ($LASTEXITCODE -ne 0) {
    throw "live/build.sh exited with error $LASTEXITCODE"
}

$iso = Join-Path $out "aios-live.iso"
if (Test-Path $iso) {
    $h = (Get-FileHash $iso -Algorithm SHA256).Hash
    $size = [math]::Round((Get-Item $iso).Length / 1MB, 1)
    ""
    "=== DONE ==="
    "ISO: $iso"
    "size: $size MB"
    "SHA256: $h"
    "Flash it with Rufus/Ventoy (hybrid BIOS+UEFI)."
}
else {
    throw "build finished but $iso was not produced"
}