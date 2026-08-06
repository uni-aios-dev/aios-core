#!/bin/sh
export PATH=/bin:/sbin:/usr/bin:/usr/sbin
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null || mdev -s
mkdir -p /dev/pts /dev/shm /mnt/root /mnt/sfs /scan /tmp
mount -t devpts devpts /dev/pts 2>/dev/null
mount -t tmpfs tmpfs /dev/shm 2>/dev/null

for m in \
  /lib/modules/*/kernel/drivers/block/loop.ko* \
  /lib/modules/*/kernel/fs/squashfs/squashfs.ko* \
  /lib/modules/*/kernel/fs/fat/fat.ko* \
  /lib/modules/*/kernel/fs/fat/vfat.ko* \
  /lib/modules/*/kernel/fs/iso9660/iso9660.ko* \
  /lib/modules/*/kernel/fs/ext4/ext4.ko* \
  /lib/modules/*/kernel/fs/nls/nls_cp437.ko* \
  /lib/modules/*/kernel/fs/nls/nls_utf8.ko* \
  /lib/modules/*/kernel/drivers/usb/storage/usb-storage.ko* \
  /lib/modules/*/kernel/drivers/usb/host/*xhci*.ko* \
  /lib/modules/*/kernel/drivers/scsi/sd_mod.ko* \
  /lib/modules/*/kernel/drivers/scsi/*.ko* \
  /lib/modules/*/kernel/drivers/ata/*.ko* \
  /lib/modules/*/kernel/drivers/nvme/*.ko* ; do
  [ -f "$m" ] && insmod "$m" 2>/dev/null
done

sleep 2

ROOTDEV=$(grep -o 'root=[^ ]*' /proc/cmdline 2>/dev/null | cut -d= -f2)
SFS=$(grep -o 'aios\.squashfs=[^ ]*' /proc/cmdline 2>/dev/null | cut -d= -f2-)

if [ -n "$ROOTDEV" ]; then
  mount -r "$ROOTDEV" /mnt/root 2>/dev/null || mount "$ROOTDEV" /mnt/root 2>/dev/null || { echo "AIOS: cannot mount $ROOTDEV"; exec /bin/sh; }
  exec switch_root /mnt/root /sbin/init
fi

if [ -z "$SFS" ]; then
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

if [ -z "$SFS" ]; then
  echo "AIOS: aios.squashfs not found. Dropping to shell."
  exec /bin/sh
fi

echo "AIOS: mounting $SFS"
mount -o loop,ro "$SFS" /mnt/sfs 2>/dev/null || { echo "AIOS: squashfs mount failed"; exec /bin/sh; }
exec switch_root /mnt/sfs /sbin/init
