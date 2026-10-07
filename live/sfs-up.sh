#!/bin/sh
# AIOS userspace bring-up for the aios-init (PID 1) live boot.
# Loads storage/loop modules, mounts the AIOS root (squashfs on the USB stick
# or a disk given via `root=`), bind-mounts the full Alpine userspace (X.org,
# GUI, tools), starts the network, udev and the X server on VT7.
# On failure the system stays in TUI-only mode (aios-core keeps running).
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export TERM=linux

log() { echo "AIOS: $*"; echo "AIOS: $*" >/dev/ttyS0 2>/dev/null; }

# The initramfs ships a statically linked busbox but no applet links (no /sbin,
# no /usr/sbin in the initramfs). Install the full applet set into /bin so that
# mount/insmod/mkdir/grep/... resolve through PATH.
/bin/busybox --install -s /bin 2>/dev/null
export PATH=/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin

echo "AIOS: userspace bring-up"

mount -t tmpfs tmpfs /run 2>/dev/null
mkdir -p /scan /mnt/sfs /mnt/root /tmp/.aios /run/aios

loadmod() {
  # Prefer modprobe (resolves dependencies via modules.dep); fall back to
  # plain insmod for modules busbox modprobe cannot resolve by name.
  for m in $1; do
    [ -f "$m" ] || continue
    base=$(basename "$m")
    base=${base%.ko.gz}; base=${base%.ko}
    if ! modprobe "$base" 2>/dev/null; then
      insmod "$m" 2>/dev/null || log "loadmod FAILED: $base"
    fi
  done
}

loadmod "/lib/modules/*/kernel/drivers/block/loop.ko*"
loadmod "/lib/modules/*/kernel/fs/squashfs/squashfs.ko*"
loadmod "/lib/modules/*/kernel/fs/fat/fat.ko*"
loadmod "/lib/modules/*/kernel/fs/fat/vfat.ko*"
loadmod "/lib/modules/*/kernel/fs/isofs/isofs.ko*"
loadmod "/lib/modules/*/kernel/fs/udf/udf.ko*"
loadmod "/lib/modules/*/kernel/fs/ext4/ext4.ko*"
loadmod "/lib/modules/*/kernel/fs/nls/nls_cp437.ko*"
loadmod "/lib/modules/*/kernel/fs/nls/nls_utf8.ko*"
# SCSI core + low-level controllers BEFORE the disk/CD drivers that depend on them.
loadmod "/lib/modules/*/kernel/drivers/scsi/scsi_common.ko*"
loadmod "/lib/modules/*/kernel/drivers/scsi/scsi_mod.ko*"
loadmod "/lib/modules/*/kernel/drivers/scsi/sr_mod.ko*"
loadmod "/lib/modules/*/kernel/drivers/scsi/sg.ko*"
loadmod "/lib/modules/*/kernel/drivers/scsi/sd_mod.ko*"
loadmod "/lib/modules/*/kernel/drivers/cdrom/cdrom.ko*"
loadmod "/lib/modules/*/kernel/fs/jbd2/jbd2.ko*"
loadmod "/lib/modules/*/kernel/fs/mbcache.ko*"
loadmod "/lib/modules/*/kernel/fs/unicode/unicode.ko*"
loadmod "/lib/modules/*/kernel/crypto/crc32c-cryptoapi.ko*"
loadmod "/lib/modules/*/kernel/drivers/ata/libata.ko*"
loadmod "/lib/modules/*/kernel/drivers/ata/ata_piix.ko*"
loadmod "/lib/modules/*/kernel/drivers/ata/ata_generic.ko*"
loadmod "/lib/modules/*/kernel/drivers/ata/libahci.ko*"
loadmod "/lib/modules/*/kernel/drivers/ata/ahci.ko*"
loadmod "/lib/modules/*/kernel/drivers/ide/ide-core.ko*"
loadmod "/lib/modules/*/kernel/drivers/ide/ide-pci-generic.ko*"
loadmod "/lib/modules/*/kernel/drivers/usb/storage/usb-storage.ko*"
loadmod "/lib/modules/*/kernel/drivers/usb/host/*xhci*.ko*"
loadmod "/lib/modules/*/kernel/drivers/nvme/*.ko*"
loadmod "/lib/modules/*/kernel/drivers/ata/*.ko*"
sleep 2
log "block devices: $(ls /dev/sd* /dev/sr* /dev/vd* /dev/nvme* 2>/dev/null | tr '\n' ' ')"

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

loadmod "/lib/modules/*/kernel/drivers/net/ethernet/intel/e1000/e1000.ko* /lib/modules/*/kernel/drivers/net/ethernet/intel/e1000e/e1000e.ko* /lib/modules/*/kernel/drivers/net/ethernet/intel/igb/igb.ko* /lib/modules/*/kernel/drivers/net/virtio_net.ko* /lib/modules/*/kernel/drivers/net/ethernet/realtek/r8169.ko*"
sleep 2
log "net ifaces after load: $(ls -d /sys/class/net/* 2>/dev/null | xargs -n1 basename 2>/dev/null | tr '\n' ' ' || echo none)"

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
  log "no root device/squashfs matched scan of /dev/sd* /dev/vd* /dev/nvme* /dev/mmcblk* /dev/sr*"
  : > /run/aios-tui-only
  exit 1
fi
if [ -z "$(ls -A "$U" 2>/dev/null)" ]; then
  echo "AIOS: userspace root mount failed — TUI-only mode"
  log "mount of $U failed (empty)"
  : > /run/aios-tui-only
  exit 1
fi
log "userspace root ready: $U"

# Bind the Alpine userland dirs. /bin is deliberately NOT rebound: the
# initramfs already ships a working static busybox (with full applet links
# installed above), and rebinding it to the squashfs /bin breaks the running
# script's PATH resolution. The dirs that matter for the dynamic aios-core
# (musl loader, webkit/gtk libs, X.org, fonts) all live in usr/lib/sbin/etc.
for d in sbin usr lib etc root var boot; do
  [ -d "$U/$d" ] || continue
  mkdir -p "/$d"
  mount --bind "$U/$d" "/$d" 2>/dev/null && echo "AIOS: bound $U/$d -> /$d"
done

mkdir -p /dev/pts /dev/shm /var/log /var/tmp
mount -t devpts devpts /dev/pts 2>/dev/null
mount -t tmpfs tmpfs /dev/shm 2>/dev/null
mount -t tmpfs tmpfs /var/log 2>/dev/null
mount -t tmpfs tmpfs /var/tmp 2>/dev/null

# /etc came from the read-only squashfs, so rcS/udhcpc's DHCP deconfig script
# cannot write DNS settings there. Bind a writable temp file over
# /etc/resolv.conf so DHCP leases and the static fallback can configure
# nameservers (the web tab will not resolve any hostname without them).
echo "" > /run/aios/resolv.conf 2>/dev/null
mount --bind /run/aios/resolv.conf /etc/resolv.conf 2>/dev/null
log "resolv after bind: [$(grep -h '^nameserver' /etc/resolv.conf 2>/dev/null | tr '\n' ' ')]"

# Diagnostics for the serial log: confirm musl loader, webkit and X are on
# the rebound paths so the dynamic /system/aios-core can exec.
log "post-bind musl: $(ls /lib/ld-musl* 2>/dev/null | tr '\n' ' ')"
log "post-bind webkit: $(ls /usr/lib/libwebkit2gtk* 2>/dev/null | tr '\n' ' ')"
log "post-bind Xorg: $(command -v Xorg 2>/dev/null || echo none)"
log "post-bind aios: $(ls -la /usr/local/bin/aios /usr/local/bin/aios-gui 2>/dev/null | tr '\n' ' ')"

if [ -x /etc/init.d/rcS ]; then
  /bin/sh /etc/init.d/rcS >/dev/null 2>&1 || true
fi

log "net ifaces: $(cat /proc/net/dev 2>/dev/null | tail -n +3 | cut -d: -f1 | tr -d ' ' | tr '\n' ' ')"
log "net ip: $(ip -o -4 addr show 2>/dev/null | grep -v ' 127\.' | awk '{print $2, $4}' | tr '\n' ' ' | cut -c1-300)"
log "net gw: $(ip route 2>/dev/null | grep default | awk '{print $3}' | tr '\n' ' ')"
log "net dns: $(cat /etc/resolv.conf 2>/dev/null | grep nameserver | awk '{print $2}' | tr '\n' ' ')"

if command -v udevd >/dev/null 2>&1; then
  udevd --daemon 2>/dev/null || true
  sleep 1
  udevadm trigger 2>/dev/null || true
  udevadm settle 2>/dev/null || true
  sleep 2
fi

# Re-run DHCP now that udev has settled the NIC (rcS's udhcpc may have raced
# the carrier up); then report the address on the serial log.
for iface in $(ls -d /sys/class/net/e* /sys/class/net/w* 2>/dev/null | xargs -n1 basename 2>/dev/null); do
  [ "$iface" = "lo" ] && continue
  ip link set "$iface" up 2>/dev/null
  log "udhcpc bin: $(command -v udhcpc || echo MISSING), probe: $(ls -la /sbin/udhcpc 2>/dev/null | tr -s ' ' | tr '\n' ' ')"
  udhcpc -i "$iface" -q -n >/run/aios/udhcpc.log 2>&1
  rc=$?
  log "udhcpc exit=$rc: $(cut -c1-300 /run/aios/udhcpc.log 2>/dev/null | tr '\n' ' ')"
done
log "net ip after udhcpc: $(ip -o -4 addr show 2>/dev/null | grep -v ' 127\.' | awk '{print $2, $4}' | tr '\n' ' ' | cut -c1-300)"
if [ -z "$(ip -o -4 addr show 2>/dev/null | grep -v ' 127\.')" ]; then
  # Static fallback (QEMU user-net / typical NAT): prove the NIC + AR route,
  # then seed nameservers (the resolv.conf bind above makes them stick).
  for iface in $(ls -d /sys/class/net/e* 2>/dev/null | xargs -n1 basename 2>/dev/null); do
    ip addr add 10.0.2.15/24 dev "$iface" 2>/dev/null
    ip route add default via 10.0.2.2 dev "$iface" 2>/dev/null
    chkp=$(ping -c 1 -W 2 10.0.2.2 2>&1 | grep -cE '1 received|bytes from' )
    log "static-net test $iface 10.0.2.2: ping_ok=$chkp"
  done
  if ! grep -q '^nameserver' /etc/resolv.conf 2>/dev/null; then
    printf 'nameserver 10.0.2.3\nnameserver 8.8.8.8\nnameserver 1.1.1.1\n' > /etc/resolv.conf
  fi
  log "resolv: $(grep '^nameserver' /etc/resolv.conf 2>/dev/null | tr '\n' ' ')"
  dnst=$(ping -c 1 -W 4 example.com 2>&1 | grep -m1 -oE 'PING [^ ]+ \([0-9.]+\)')
  log "static-net dns example.com: ${dnst:-FAIL}"
fi

if command -v Xorg >/dev/null 2>&1 && [ ! -e /tmp/.X11-unix/X0 ]; then
  echo "AIOS: starting X server on :0 (VT7)"
  Xorg :0 vt7 -nolisten tcp -logfile /tmp/xorg.log >/tmp/xorg.out 2>&1 &
  sleep 4
  log "X sockets: $(ls /tmp/.X11-unix/ 2>/dev/null | tr '\n' ' ')"
  log "X log: $(grep -hoE '\((EE|WW|NI|II)\)|\berror|\bfatal|no screen found|cannot run in framebuffer|No devices detected' /tmp/xorg.log 2>/dev/null | sort -u | tr '\n' ' ')"
  log "X vt: $(cat /sys/class/tty/tty0/active 2>/dev/null)"
  log "X fb: $(ls /dev/fb0 /dev/dri/card0 2>/dev/null | tr '\n' ' ')"
  log "X EE: $(grep -E '\(EE\)' /tmp/xorg.log 2>/dev/null | tr '\n' ' ' | cut -c1-400)"
  log "X mode: $(DISPLAY=:0 xrandr --current 2>/dev/null | grep -E '\*|\bconnected' | tr '\n' ' ' | cut -c1-400)"
  if [ "${AIOS_BROWSER_AUTO:-0}" = "1" ]; then
    log "browser auto-open requested"
  fi
fi

# Background watchdog: once the TUI dashboard is up (and the user may have
# opened the native browser), dump the OS-level picture to the serial log so a
# headless QEMU capture can confirm WebKit/GTK are actually running.
(
  sleep 150
  if [ -x /usr/bin/pgrep ]; then
    procs=$(pgrep -af 'WebKit|Xorg|aios' 2>/dev/null | tr '\n' ' ')
  else
    procs=$(ps ax 2>/dev/null | grep -iE 'webkit|Xorg|/aios' | grep -v grep | tr '\n' ' ')
  fi
  log "watchdog procs: $(echo "$procs" | cut -c1-700)"
  log "watchdog X sockets: $(ls /tmp/.X11-unix/ 2>/dev/null | tr '\n' ' ')"
  log "watchdog Xmode: $(DISPLAY=:0 xrandr --current 2>/dev/null | grep -E '\*' | tr '\n' ' ' | cut -c1-300)"
) &

echo "AIOS: userspace ready (DISPLAY=:0)"
log "userspace ready (DISPLAY=:0)"
: > /run/aios-ready
exit 0