#!/bin/sh
# build-live-iso.sh - build the bootable AIOS-LIVE ISO via Docker (live/build.sh)
# Linux/macOS equivalent of scripts/build-live-iso.ps1 (for hosts with Docker).
#
# Requires:
#   - Docker engine with linux containers (podman supported via PODMAN=1).
#   - Internet access (cargo deps are fetched when CARGO_NET_OFFLINE=false;
#     to build fully offline, pre-populate ~/.cargo/registry and keep offline).
#
# Output: live/out/aios-live.iso

set -e

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
LIVE="$ROOT/live"
REG="${CARGO_REGISTRY_DIR:-$HOME/.cargo/registry}"
OUT="$LIVE/out"

command -v docker >/dev/null 2>&1 && ENGINE=docker || {
    command -v podman >/dev/null 2>&1 && ENGINE=podman || {
        echo "error: docker or podman not found" >&2
        exit 1
    }
}
if [ "$PODMAN" = "1" ]; then ENGINE=podman; fi

"$ENGINE" info >/dev/null 2>&1 || { echo "error: $ENGINE engine is not running" >&2; exit 1; }

mkdir -p "$OUT" "$REG"

echo "=== pulling/checking rust:alpine base image ==="
"$ENGINE" pull rust:alpine

echo "=== launching live/build.sh (aios static-musl + rootfs + initramfs + GRUB ISO) ==="
"$ENGINE" run --rm -it \
    -e CARGO_NET_OFFLINE="${CARGO_NET_OFFLINE:-false}" \
    -v "$ROOT:/src" \
    -v "$LIVE:/work" \
    -v "$REG:/usr/local/cargo/registry" \
    rust:alpine \
    sh /work/build.sh

ISO="$OUT/aios-live.iso"
if [ -f "$ISO" ]; then
    echo ""
    echo "=== DONE ==="
    echo "ISO: $ISO"
    echo "size: $(du -h "$ISO" | cut -f1)"
    echo "SHA256: $(sha256sum "$ISO" | cut -d' ' -f1)"
    echo "Flash it with Rufus/Ventoy (hybrid BIOS+UEFI)."
else
    echo "error: build finished but $ISO was not produced" >&2
    exit 1
fi