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

# --- wait_block: poll /dev for block devices, up to $1 seconds ---
wait_block() {
  local timeout=${1:-4} i=0
  while [ $i -lt $timeout ]; do
    ls /dev/sd[a-z] /dev/sr0 /dev/nvme0n1 /dev/mmcblk0 2>/dev/null | grep -q . && return 0
    sleep 1; i=$((i+1))
  done
  return 1
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
loadmod "/lib/modules/*/kernel/drivers/virtio/*.ko*"
loadmod "/lib/modules/*/kernel/drivers/block/virtio_blk.ko*"
loadmod "/lib/modules/*/kernel/drivers/nvme/*.ko*"
loadmod "/lib/modules/*/kernel/drivers/ata/*.ko*"
# Poll for block devices instead of sleeping blindly. USB sticks need several
# seconds to enumerate (xhci -> usb-storage -> sd), so allow a generous timeout.
wait_block 15
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
# No sleep here — GPU modules are probe-only, no settle needed before squashfs scan.

loadmod "/lib/modules/*/kernel/drivers/net/ethernet/intel/e1000/e1000.ko* /lib/modules/*/kernel/drivers/net/ethernet/intel/e1000e/e1000e.ko* /lib/modules/*/kernel/drivers/net/ethernet/intel/igb/igb.ko* /lib/modules/*/kernel/drivers/net/virtio_net.ko* /lib/modules/*/kernel/drivers/net/ethernet/realtek/r8169.ko*"
# Network modules loaded — no blocking sleep, DHCP runs in background later.
log "net ifaces after load: $(ls -d /sys/class/net/* 2>/dev/null | xargs -n1 basename 2>/dev/null | tr '\n' ' ' || echo none)"

ROOTDEV=$(grep -o 'root=[^ ]*' /proc/cmdline 2>/dev/null | cut -d= -f2)
SFS=$(grep -o 'aios\.squashfs=[^ ]*' /proc/cmdline 2>/dev/null | cut -d= -f2-)

if [ -n "$ROOTDEV" ] && [ ! -b "$ROOTDEV" ]; then
  ROOTDEV=""
fi

if [ -z "$ROOTDEV" ] && [ -z "$SFS" ]; then
  # The stick may still be enumerating when we get here, so rescan a few times
  # instead of giving up on the first pass.
  scan_root() {
    for dev in /dev/sd[a-z]* /dev/vd[a-z]* /dev/nvme0n* /dev/mmcblk* /dev/sr*; do
      [ -b "$dev" ] || continue
      umount /scan 2>/dev/null
      mount -r "$dev" /scan 2>/dev/null || continue
      for p in boot/aios.squashfs aios.squashfs; do
        if [ -f "/scan/$p" ]; then
          SFS="/scan/$p"
          return 0
        fi
      done
      umount /scan 2>/dev/null
    done
    return 1
  }
  attempt=0
  while [ $attempt -lt 5 ]; do
    scan_root && break
    attempt=$((attempt+1))
    [ $attempt -lt 5 ] && sleep 2
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

# Diagnostics for the serial log.
log "post-bind musl: $(ls /lib/ld-musl* 2>/dev/null | tr '\n' ' ')"
log "post-bind webkit: $(ls /usr/lib/libwebkit2gtk* 2>/dev/null | tr '\n' ' ')"
log "post-bind Xorg: $(command -v Xorg 2>/dev/null || echo none)"
log "post-bind aios: $(ls -la /usr/local/bin/aios /usr/local/bin/aios-gui 2>/dev/null | tr '\n' ' ')"

# Run rcS for basic init (tmpfs mounts, loopback, etc.) — skip its DHCP/GUI
# parts since we handle those below with proper sequencing.
if [ -x /etc/init.d/rcS ]; then
  /bin/sh /etc/init.d/rcS >/dev/null 2>&1 || true
fi

# Start udevd in background — do NOT wait synchronously for udevadm settle.
if command -v udevd >/dev/null 2>&1; then
  udevd --daemon 2>/dev/null || true
  udevadm trigger 2>/dev/null &
fi

# DHCP runs fully in background — network is not required to show the TUI.
# A background script handles fallback + DNS seeding after the UI is up.
(
  sleep 1
  for iface in $(ls -d /sys/class/net/e* /sys/class/net/w* 2>/dev/null | xargs -n1 basename 2>/dev/null); do
    [ "$iface" = "lo" ] && continue
    ip link set "$iface" up 2>/dev/null
    udhcpc -i "$iface" -q -n >/run/aios/udhcpc_${iface}.log 2>&1
    if [ -z "$(ip -o -4 addr show dev $iface 2>/dev/null | grep -v ' 127\.')" ]; then
      # DHCP failed — configure static fallback silently (no DNS ping test).
      ip addr add 10.0.2.15/24 dev "$iface" 2>/dev/null
      ip route add default via 10.0.2.2 dev "$iface" 2>/dev/null
      if ! grep -q '^nameserver' /etc/resolv.conf 2>/dev/null; then
        printf 'nameserver 10.0.2.3\nnameserver 8.8.8.8\nnameserver 1.1.1.1\n' > /etc/resolv.conf
      fi
    fi
    log "net $iface: $(ip -o -4 addr show dev $iface 2>/dev/null | awk '{print $4}')"
  done
) &

log "net ifaces: $(cat /proc/net/dev 2>/dev/null | tail -n +3 | cut -d: -f1 | tr -d ' ' | tr '\n' ' ')"

# Start X server in background, then immediately proceed — we poll for the
# socket below and can start the TUI while X is still initialising.
if command -v Xorg >/dev/null 2>&1 && [ ! -e /tmp/.X11-unix/X0 ]; then
  echo "AIOS: starting X server on :0 (VT7)"
  Xorg :0 vt7 -nolisten tcp -logfile /tmp/xorg.log >/tmp/xorg.out 2>&1 &
fi

# Poll for X socket up to 6 seconds (max 1 s per iteration, no hard sleep 4).
i=0
while [ $i -lt 6 ]; do
  [ -e /tmp/.X11-unix/X0 ] && break
  sleep 1; i=$((i+1))
done
log "X sockets after ${i}s: $(ls /tmp/.X11-unix/ 2>/dev/null | tr '\n' ' ')"
if command -v Xorg >/dev/null 2>&1; then
  log "X EE: $(grep -E '\(EE\)' /tmp/xorg.log 2>/dev/null | tr '\n' ' ' | cut -c1-400)"
fi
# Xorg activates VT7 as soon as it starts. If it then fails there is no X server
# but the console is still switched to the empty VT7, which looks like a freeze.
# Switch back to VT1 so the TUI below is visible.
if [ ! -e /tmp/.X11-unix/X0 ] && command -v chvt >/dev/null 2>&1; then
  chvt 1 2>/dev/null && log "X unavailable — switched console back to VT1"
fi

# Background watchdog: log system state after UI should be up.
(
  sleep 90
  if [ -x /usr/bin/pgrep ]; then
    procs=$(pgrep -af 'WebKit|Xorg|aios' 2>/dev/null | tr '\n' ' ')
  else
    procs=$(ps ax 2>/dev/null | grep -iE 'webkit|Xorg|/aios' | grep -v grep | tr '\n' ' ')
  fi
  log "watchdog procs: $(echo "$procs" | cut -c1-700)"
  log "watchdog X sockets: $(ls /tmp/.X11-unix/ 2>/dev/null | tr '\n' ' ')"
) &

# Start AIOS GUI as the desktop session.
if [ -x /usr/local/bin/aios-gui ] && [ -e /tmp/.X11-unix/X0 ]; then
  # Start window manager in background, no blocking sleep after it.
  if command -v openbox >/dev/null 2>&1; then
    openbox &
  elif command -v jwm >/dev/null 2>&1; then
    jwm &
  elif command -v fluxbox >/dev/null 2>&1; then
    fluxbox &
  elif command -v dwm >/dev/null 2>&1; then
    dwm &
  fi
  export DISPLAY=:0
  exec /usr/local/bin/aios-gui
elif [ -x /usr/local/bin/aios ]; then
  export DISPLAY=:0
  exec /usr/local/bin/aios
fi

exec /bin/sh /etc/inittab 2>/dev/null || exec getty 38400 tty1 linux
