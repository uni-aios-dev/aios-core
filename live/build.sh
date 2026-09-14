#!/bin/sh
set -e
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export CARGO_TARGET_DIR=/tmp/target
export CARGO_NET_OFFLINE="${CARGO_NET_OFFLINE:-true}"
W=/tmp

echo "=== [0] toolchain ==="
apk update
apk add --no-cache \
  rust cargo musl-dev gcc g++ pkgconfig openssl-dev \
  squashfs-tools cpio xz gzip \
  busybox-static \
  grub grub-bios grub-efi xorriso mtools dosfstools \
  util-linux-misc \
  ca-certificates \
  libxcb-dev libxkbcommon-dev libxi-dev libxrandr-dev libxcursor-dev \
  libxinerama-dev libx11-dev libglvnd-dev mesa-dev libwayland-dev \
  fontconfig-dev libxft-dev libxrender-dev eudev-dev

command -v grub-mkrescue >/dev/null 2>&1 || apk add --no-cache grub-bios

echo "=== [1] building aios + aios-gui (musl, no webview engine) ==="
cd /src
cargo build -p aios --release --no-default-features
cargo build -p aios-gui --release --no-default-features
cp "$CARGO_TARGET_DIR/release/aios" "$W/aios-bin"
cp "$CARGO_TARGET_DIR/release/aios-gui" "$W/aios-gui-bin"
ls -la "$W/aios-bin" "$W/aios-gui-bin"
file "$W/aios-bin" 2>/dev/null || true
file "$W/aios-gui-bin" 2>/dev/null || true

echo "=== [2] building rootfs ==="
rm -rf "$W/rootfs" "$W/iso" "$W/initramfs" "$W/out"
mkdir -p "$W/rootfs" "$W/iso/boot/grub" "$W/out"

MINI=$(wget -qO- https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/ 2>/dev/null | grep -oE 'alpine-minirootfs-[0-9.]+-x86_64\.tar\.gz' | sort -uV | tail -1)
echo "download minirootfs: $MINI"
wget -q -O "/tmp/$MINI" "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/$MINI"
tar xzf "/tmp/$MINI" -C "$W/rootfs"
cp /etc/resolv.conf "$W/rootfs/etc/resolv.conf" 2>/dev/null || true
mount --bind /dev "$W/rootfs/dev" 2>/dev/null || true
mount --bind /proc "$W/rootfs/proc" 2>/dev/null || true
chroot "$W/rootfs" /sbin/apk add --no-cache \
  linux-lts \
  grub grub-bios grub-efi \
  e2fsprogs dosfstools \
  util-linux-misc util-linux \
  xorg-server xauth xrandr \
  xf86-input-evdev xf86-input-libinput libinput \
  xf86-video-fbdev xf86-video-vesa \
  mesa mesa-dri-gallium libglvnd \
  libxcb libx11 libxi libxrandr libxcursor libxinerama libxext \
  libxkbcommon fontconfig ttf-dejavu \
  eudev || echo "NOTE: apk trigger errors ignored (grub-probe in chroot)"
umount "$W/rootfs/dev" 2>/dev/null || true
umount "$W/rootfs/proc" 2>/dev/null || true

mkdir -p "$W/rootfs/usr/local/bin" "$W/rootfs/etc/init.d" "$W/rootfs/root" "$W/rootfs/boot"
cp "$W/aios-bin" "$W/rootfs/usr/local/bin/aios"
cp "$W/aios-gui-bin" "$W/rootfs/usr/local/bin/aios-gui"
cp "/work/aios-install" "$W/rootfs/usr/local/bin/aios-install"
cp "/work/aios-launch" "$W/rootfs/usr/local/bin/aios-launch"
chmod +x "$W/rootfs/usr/local/bin/aios" "$W/rootfs/usr/local/bin/aios-gui" "$W/rootfs/usr/local/bin/aios-install" "$W/rootfs/usr/local/bin/aios-launch"
cp "/work/inittab" "$W/rootfs/etc/inittab"
cp "/work/rcS" "$W/rootfs/etc/init.d/rcS"
chmod +x "$W/rootfs/etc/init.d/rcS"

printf 'root:x:0:0:root:/root:/bin/sh\n' > "$W/rootfs/etc/passwd"
printf 'root:x:0:\n' > "$W/rootfs/etc/group"

cat > "$W/rootfs/etc/fstab" <<'EOF'
tmpfs	/tmp	tmpfs	defaults,noatime,mode=1777	0 0
tmpfs	/var/tmp	tmpfs	defaults,noatime,mode=1777	0 0
tmpfs	/var/log	tmpfs	defaults,noatime,mode=1777	0 0
tmpfs	/run	tmpfs	defaults,noatime,mode=0755	0 0
EOF

cat > "$W/rootfs/etc/profile" <<'EOF'
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export TERM=linux
export DISPLAY=:0
export AIOS_DATA_DIR=/tmp/.aios
EOF

cat > "$W/rootfs/etc/motd" <<'EOF'
AIOS Live — Type 'aios-install' to install AIOS to a disk.
EOF

echo "=== [3] initramfs (built before squashfs so boot files can be injected) ==="
mkdir -p "$W/initramfs/bin" "$W/initramfs/dev" "$W/initramfs/proc" "$W/initramfs/sys" "$W/initramfs/tmp" "$W/initramfs/system" "$W/initramfs/lib/modules"
cp -a "$W/rootfs/lib/modules/." "$W/initramfs/lib/modules/"

if [ "${USE_BUSYBOX_INIT:-0}" = "1" ]; then
  echo "=== [3a] busybox init mode (legacy): squashfs root + switch_root ==="
  cp /bin/busybox.static "$W/initramfs/bin/busybox"
  "$W/initramfs/bin/busybox" --install -s "$W/initramfs/bin"
  cd "$W"
  cp "/work/init.rs" "$W/initramfs/init"
  chmod +x "$W/initramfs/init"
else
  echo "=== [3a] aios-init mode (default): kernel TUI as PID 1 ==="
  cd /src/aios-init
  cargo build --release
  cp "$CARGO_TARGET_DIR/release/aios-init" "$W/initramfs/init"
  chmod +x "$W/initramfs/init"
  cp "$W/aios-bin" "$W/initramfs/system/aios-core"
  chmod +x "$W/initramfs/system/aios-core"
  cp /bin/busybox.static "$W/initramfs/bin/busybox"
  chmod +x "$W/initramfs/bin/busybox"
  ln -sf busybox "$W/initramfs/bin/sh"
  cp "/work/sfs-up.sh" "$W/initramfs/sfs-up.sh"
  chmod +x "$W/initramfs/sfs-up.sh"
fi

cd "$W/initramfs"
find . | cpio -o -H newc 2>/dev/null | gzip -9 > "$W/iso/boot/initramfs.gz"

echo "=== [4] injecting boot files into rootfs (installed-disk boot) ==="
cp "$W/iso/boot/initramfs.gz" "$W/rootfs/boot/initramfs.gz"
cp "$W/rootfs/boot/vmlinuz-lts" "$W/rootfs/boot/vmlinuz"

echo "=== [5] squashfs ==="
mksquashfs "$W/rootfs" "$W/iso/boot/aios.squashfs" -noappend -comp xz

echo "=== [6] iso ==="
if [ "${USE_BUSYBOX_INIT:-0}" = "1" ]; then
  cp "/work/grub.cfg" "$W/iso/boot/grub/grub.cfg"
else
  cat > "$W/iso/boot/grub/grub.cfg" <<'EOF'
set timeout=10
set default=0

menuentry "AIOS (aios-init kernel TUI)" {
  linux /boot/vmlinuz init=/init console=tty0
  initrd /boot/initramfs.gz
}

menuentry "AIOS (verbose)" {
  linux /boot/vmlinuz init=/init console=tty0
  initrd /boot/initramfs.gz
}
EOF
fi
cp "$W/rootfs/boot/vmlinuz-lts" "$W/iso/boot/vmlinuz"
grub-mkrescue -o "$W/out/aios-live.iso" "$W/iso" -- -volid AIOS-LIVE 2>&1 | tail -5

echo "=== [7] copying to /work ==="
mkdir -p /work/out
cp "$W/out/aios-live.iso" /work/out/aios-live.iso
ls -la /work/out/ /work/out/aios-live.iso
sha256sum /work/out/aios-live.iso