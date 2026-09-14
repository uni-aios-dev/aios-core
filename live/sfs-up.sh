#!/bin/sh
# AIOS userspace bring-up for the aios-init (PID 1) live boot.
# Loads storage/loop modules, mounts the AIOS root (squashfs on the USB stick
# or a disk given via `root=`), bind-mounts the full Alpine userspace (X.org,
# GUI, tools), starts the network, udev and the X server on VT7.
# On failure the system stays in TUI-only mode (aios-core keeps running).
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export TERM=linux

echo "AIOS: userspace bring-up"

mount -t tmpfs tmpfs /run 2>/dev/null
mkdir -p /scan /mnt/sfs /mnt/root /tmp/.aios /run/aios

loadmod() {
  for m in $1; do
    [ -f "$m" ] && insmod "$m" 2>/dev/null
  done
}

loadmod "/lib/modules/*/kernel/drivers/block/loop.ko*"
loadmod "/lib/modules/*/kernel/fs/squashfs/squashfs.ko*"
loadmod "/lib/modules/*/kernel/fs/fat/fat.ko*"
loadmod "/lib/modules/*/kernel/fs/fat/vfat.ko*"
loadmod "/lib/modules/*/kernel/fs/iso9660/iso9660.ko*"
loadmod "/lib/modules/*/kernel/fs/ext4/ext4.ko*"
loadmod "/lib/modules/*/kernel/fs/nls/nls_cp437.ko*"
loadmod "/lib/modules/*/kernel/fs/nls/nls_utf8.ko*"
loadmod "/lib/modules/*/kernel/drivers/usb/storage/usb-storage.ko*"
loadmod "/lib/modules/*/kernel/drivers/usb/host/*xhci*.ko*"
loadmod "/lib/modules/*/kernel/drivers/scsi/sd_mod.ko*"
loadmod "/lib/modules/*/kernel/drivers/scsi/*.ko*"
loadmod "/lib/modules/*/kernel/drivers/ata/*.ko*"
loadmod "/lib/modules/*/kernel/drivers/nvme/*.ko*"

# GPU stack in dependency order (best-effort): core DRM first, then drivers,
# then fbdev fallbacks for X.org.
loadmod "/lib/modules/*/kernel/drivers/gpu/drm/drm.ko*"
loadmod "/lib/modules/*/kernel/drivers/gpu/drm/drm_kms_helper.ko*"
loadmod "/lib/modules/*/kernel/drivers/gpu/drm/drm_buddy.ko* /lib/modules/*/kernel/drivers/gpu/drm/drm_exec.ko* /lib/modules/*/kernel/drivers/gpu/drm/drm_gpuvm.ko* /lib/modules/*/kernel/drivers/gpu/drm/ttm/ttm.ko*"
loadmod "/lib/modules/*/kernel/drivers/gpu/drm/i915/i915.ko*"
loadmod "/lib/modules/*/kernel/drivers/gpu/drm/amd/amdgpu/amdgpu.ko*"
loadmod "/lib/modules/*/kernel/drivers/gpu/drm/radeon/radeon.ko*"
loadmod "/lib/modules/*/kernel/drivers/gpu/drm/nouveau/nouveau.ko*"
loadmod "/lib/modules/*/kernel/drivers/gpu/drm/vmwgfx/vmwgfx.ko*"
loadmod "/lib/modules/*/kernel/drivers/video/fbdev/efifb.ko* /lib/modules/*/kernel/drivers/video/fbdev/vesafb.ko* /lib/modules/*/kernel/drivers/video/fbdev/simplefb.ko*"

sleep 2

ROOTDEV=$(grep -o 'root=[^ ]*' /proc/cmdline 2>/dev/null | cut -d= -f2)
SFS=$(grep -o 'aios\.squashfs=[^ ]*' /proc/cmdline 2>/dev/null | cut -d= -f2-)

if [ -n "$ROOTDEV" ] && [ ! -b "$ROOTDEV" ]; then
  ROOTDEV=""
fi

if [ -z "$ROOTDEV" ] && [ -z "$SFS" ]; then
  for dev in /dev/sd[a-z]* /dev/vd[a-z]* /dev/nvme0n* /dev/mmcblk* /dev/sr*; do
    [ -b "$dev" ] || continue
    umount /scan 2>/dev/null
    mount -r "$dev" /scan 2>/dev/null || continue
    for p in boot/aios.squashfs aios.squashfs; do
      if [ -f "/scan/$p" ]; then
        SFS="/scan/$p"
        break 2
      fi
    done
  done
fi

U=""
if [ -n "$ROOTDEV" ]; then
  U=/mnt/root
  mount -r "$ROOTDEV" "$U" 2>/dev/null || mount "$ROOTDEV" "$U" 2>/dev/null
elif [ -n "$SFS" ]; then
  echo "AIOS: mounting $SFS"
  U=/mnt/sfs
  mount -o loop,ro "$SFS" "$U" 2>/dev/null
fi

if [ -z "$U" ]; then
  echo "AIOS: no userspace root found — TUI-only mode"
  : > /run/aios-tui-only
  exit 1
fi
if [ -z "$(ls -A "$U" 2>/dev/null)" ]; then
  echo "AIOS: userspace root mount failed — TUI-only mode"
  : > /run/aios-tui-only
  exit 1
fi

for d in bin sbin usr lib etc root var boot; do
  [ -d "$U/$d" ] || continue
  mkdir -p "/$d"
  mount --bind "$U/$d" "/$d" 2>/dev/null && echo "AIOS: bound $U/$d -> /$d"
done

mkdir -p /dev/pts /dev/shm /var/log /var/tmp
mount -t devpts devpts /dev/pts 2>/dev/null
mount -t tmpfs tmpfs /dev/shm 2>/dev/null
mount -t tmpfs tmpfs /var/log 2>/dev/null
mount -t tmpfs tmpfs /var/tmp 2>/dev/null

if [ -x /etc/init.d/rcS ]; then
  /bin/sh /etc/init.d/rcS >/dev/null 2>&1 || true
fi

if command -v udevd >/dev/null 2>&1; then
  udevd --daemon 2>/dev/null || true
  sleep 1
  udevadm trigger 2>/dev/null || true
  udevadm settle 2>/dev/null || true
fi

if command -v Xorg >/dev/null 2>&1 && [ ! -e /tmp/.X11-unix/X0 ]; then
  echo "AIOS: starting X server on :0 (VT7)"
  /usr/bin/Xorg :0 vt7 -nolisten tcp >/dev/null 2>&1 &
  sleep 2
fi

echo "AIOS: userspace ready (DISPLAY=:0)"
: > /run/aios-ready
exit 0