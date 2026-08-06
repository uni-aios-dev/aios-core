#!/bin/sh
set -e
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export CARGO_TARGET_DIR=/tmp/target
export CARGO_NET_OFFLINE=true
W=/tmp

echo "=== [0] toolchain ==="
apk update
apk add --no-cache \
  rust cargo musl-dev gcc g++ pkgconfig openssl-dev \
  squashfs-tools cpio xz gzip \
  busybox-static \
  grub grub-bios grub-efi xorriso mtools dosfstools \
  util-linux-misc \
  ca-certificates

command -v grub-mkrescue >/dev/null 2>&1 || apk add --no-cache grub-bios

echo "=== [1] building aios (static musl, no webview) ==="
cd /src
cargo build -p aios --release --no-default-features
cp "$CARGO_TARGET_DIR/release/aios" "$W/aios-bin"
ls -la "$W/aios-bin"
file "$W/aios-bin" 2>/dev/null || true

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
  util-linux-misc util-linux || echo "NOTE: apk trigger errors ignored (grub-probe in chroot)"
umount "$W/rootfs/dev" 2>/dev/null || true
umount "$W/rootfs/proc" 2>/dev/null || true

mkdir -p "$W/rootfs/usr/local/bin" "$W/rootfs/etc/init.d" "$W/rootfs/root"
cp "$W/aios-bin" "$W/rootfs/usr/local/bin/aios"
cp "/work/aios-install" "$W/rootfs/usr/local/bin/aios-install"
cp "/work/aios-launch" "$W/rootfs/usr/local/bin/aios-launch"
chmod +x "$W/rootfs/usr/local/bin/aios" "$W/rootfs/usr/local/bin/aios-install" "$W/rootfs/usr/local/bin/aios-launch"
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
EOF

cat > "$W/rootfs/etc/motd" <<'EOF'
AIOS Live — Type 'aios-install' to install AIOS to a disk.
EOF

echo "=== [3] squashfs ==="
mksquashfs "$W/rootfs" "$W/iso/boot/aios.squashfs" -noappend -comp xz

echo "=== [4] initramfs ==="
mkdir -p "$W/initramfs/bin" "$W/initramfs/dev" "$W/initramfs/proc" "$W/initramfs/sys" "$W/initramfs/lib/modules"
cp /bin/busybox.static "$W/initramfs/bin/busybox"
"$W/initramfs/bin/busybox" --install -s "$W/initramfs/bin"
cd "$W"
cp -a "$W/rootfs/lib/modules/." "$W/initramfs/lib/modules/"
cp "/work/init.rs" "$W/initramfs/init"
chmod +x "$W/initramfs/init"
cd "$W/initramfs"
find . | cpio -o -H newc 2>/dev/null | gzip -9 > "$W/iso/boot/initramfs.gz"

echo "=== [5] iso ==="
cp "/work/grub.cfg" "$W/iso/boot/grub/grub.cfg"
cp "$W/rootfs/boot/vmlinuz-lts" "$W/iso/boot/vmlinuz"
grub-mkrescue -o "$W/out/aios-live.iso" "$W/iso" -- -volid AIOS-LIVE 2>&1 | tail -5

echo "=== [6] copying to /work ==="
mkdir -p /work/out
cp "$W/out/aios-live.iso" /work/out/aios-live.iso
ls -la /work/out/ /work/out/aios-live.iso
sha256sum /work/out/aios-live.iso
