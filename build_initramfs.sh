#!/usr/bin/env bash
#
# build_initramfs.sh — builds the static aios-init binary, packages the real
# AIOS kernel TUI as /system/aios-core, and packs a minimal initramfs that
# boots straight into the full kernel TUI (rescue shell only as a fallback).
#
# Usage:
#   ./build_initramfs.sh                 # plain initramfs (kernel TUI via /system/aios-core)
#   ./build_initramfs.sh --keep-rootfs   # keep the assembled rootfs/ directory after packing
#   ./build_initramfs.sh --no-aios-core  # skip building/staging the aios kernel binary
#   BUSYBOX_PATH=/usr/bin/busybox.static ./build_initramfs.sh
#
# Env:
#   SKIP_AIOS_CORE=1  — same as --no-aios-core
#   BUSYBOX_PATH      — bundle a static busybox as the rescue shell /bin/sh
#
# Output: initramfs.cpio.gz
#
set -euo pipefail

KEEP_ROOTFS=0
STAGE_AIOS_CORE=1

for arg in "$@"; do
  case "$arg" in
    --keep-rootfs) KEEP_ROOTFS=1 ;;
    --no-aios-core) STAGE_AIOS_CORE=0 ;;
    *) echo "WARN: ignoring unknown argument: $arg" >&2 ;;
  esac
done

if [[ "${SKIP_AIOS_CORE:-0}" == "1" ]]; then
  STAGE_AIOS_CORE=0
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TARGET="x86_64-unknown-linux-musl"
ROOTFS="${SCRIPT_DIR}/rootfs"
OUT="${SCRIPT_DIR}/initramfs.cpio.gz"

if ! rustup target list --installed 2>/dev/null | grep -qx "${TARGET}"; then
  echo "WARN: target ${TARGET} not installed — run: rustup target add ${TARGET}" >&2
fi

echo "[aios-init] building static init binary for ${TARGET}"
cd "${SCRIPT_DIR}"
cargo build --release --target "${TARGET}"

INIT_BIN="${SCRIPT_DIR}/target/${TARGET}/release/aios-init"
if [[ ! -x "${INIT_BIN}" ]]; then
  echo "ERROR: init binary not found or not executable: ${INIT_BIN}" >&2
  exit 1
fi
file "${INIT_BIN}" 2>/dev/null || true

AIOS_BIN=""
if [[ "${STAGE_AIOS_CORE}" == "1" ]]; then
  echo "[aios-init] building static aios kernel TUI for ${TARGET}"
  cargo build -p aios --release --target "${TARGET}" --no-default-features
  AIOS_BIN="${SCRIPT_DIR}/target/${TARGET}/release/aios"
  if [[ ! -x "${AIOS_BIN}" ]]; then
    echo "WARN: aios binary not found — /system/aios-core will be absent, boot falls back to the rescue shell" >&2
    AIOS_BIN=""
  else
    file "${AIOS_BIN}" 2>/dev/null || true
  fi
fi

echo "[aios-init] assembling rootfs layout"
# Safety guard: never remove anything outside SCRIPT_DIR.
if [[ "${ROOTFS}" != "${SCRIPT_DIR}"/* ]]; then
  echo "ERROR: refusing to use rootfs outside SCRIPT_DIR: ${ROOTFS}" >&2
  exit 1
fi
rm -rf "${ROOTFS}"
mkdir -p "${ROOTFS}"/{bin,dev,proc,sys,tmp,system}

cp "${INIT_BIN}" "${ROOTFS}/init"
chmod +x "${ROOTFS}/init"

if [[ -n "${AIOS_BIN}" ]]; then
  cp "${AIOS_BIN}" "${ROOTFS}/system/aios-core"
  chmod +x "${ROOTFS}/system/aios-core"
  echo "[aios-init] staged /system/aios-core (kernel TUI)"
fi

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

if [[ "${KEEP_ROOTFS}" == "1" ]]; then
  echo "[aios-init] keeping rootfs at ${ROOTFS}"
else
  rm -rf "${ROOTFS}"
  echo "[aios-init] rootfs cleaned (use --keep-rootfs to retain it)"
fi
