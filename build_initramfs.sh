#!/usr/bin/env bash
#
# build_initramfs.sh — builds the static aios-init binary and packs a
# minimal initramfs that boots into the AIOS block supervisor.
#
# Usage:
#   ./build_initramfs.sh                 # plain initramfs (emergency shell only if no busybox)
#   BUSYBOX_PATH=/usr/bin/busybox.static ./build_initramfs.sh
#
# Output: initramfs.cpio.gz
#
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TARGET="x86_64-unknown-linux-musl"
ROOTFS="${SCRIPT_DIR}/rootfs"
OUT="${SCRIPT_DIR}/initramfs.cpio.gz"

if ! rustup target list --installed 2>/dev/null | grep -qx "${TARGET}"; then
  echo "WARN: target ${TARGET} not installed — run: rustup target add ${TARGET}" >&2
fi

echo "[aios-init] building static binary for ${TARGET}"
cd "${SCRIPT_DIR}"
cargo build --release --target "${TARGET}"

BIN="${SCRIPT_DIR}/target/${TARGET}/release/aios-init"
if [[ ! -x "${BIN}" ]]; then
  echo "ERROR: binary not found or not executable: ${BIN}" >&2
  exit 1
fi
file "${BIN}" 2>/dev/null || true

echo "[aios-init] assembling rootfs layout"
rm -rf "${ROOTFS}"
mkdir -p "${ROOTFS}"/{bin,dev,proc,sys,tmp,system}

cp "${BIN}" "${ROOTFS}/init"
chmod +x "${ROOTFS}/init"

if [[ -n "${BUSYBOX_PATH:-}" ]]; then
  if [[ ! -x "${BUSYBOX_PATH}" ]]; then
    echo "WARN: BUSYBOX_PATH set but not executable: ${BUSYBOX_PATH}" >&2
  else
    cp "${BUSYBOX_PATH}" "${ROOTFS}/bin/busybox"
    chmod +x "${ROOTFS}/bin/busybox"
    ln -sf busybox "${ROOTFS}/bin/sh"
    echo "[aios-init] bundled busybox shell from ${BUSYBOX_PATH}"
  fi
fi

echo "[aios-init] packing initramfs"
(cd "${ROOTFS}" && find . -print0 | cpio --null -ov --format=newc 2>/dev/null | gzip -9 > "${OUT}")

echo "[aios-init] done: ${OUT}"
ls -lh "${OUT}"
