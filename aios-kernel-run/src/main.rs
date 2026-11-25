//! aios-kernel-run: host tool that builds the AIOS bare-metal kernel, wraps the
//! resulting Limine-protocol ELF into a bootable hybrid ISO (BIOS + UEFI) using
//! the Limine tooling and xorriso, and launches it in QEMU.
//!
//! The limine `bios-install` step makes the ISO isohybrid, so a byte copy of it
//! (`aios-kernel-usb.img`) is a bootable USB stick: legacy BIOS boots it through
//! the Limine MBR, UEFI through the ESP partition's `BOOTX64.EFI`.
//!
//! The kernel is built with `--features limine` here: Limine resolves its own
//! requests and the kernel translates them into the shared `BootInfo`. The
//! UEFI-only image (`boot/aios-image`) builds the very same crate without that
//! feature, so `aios-boot` keeps supplying `BootInfo` in `rdi`.
//!
//! All external tool locations can be overridden with environment variables:
//!   AIOS_LIMINE_DIR   directory holding limine-bios-cd.bin / limine-uefi-cd.bin
//!   AIOS_LIMINE_TOOL  path to the `limine` host tool (bios-install)
//!   AIOS_XORRISO      path to `xorriso`
//!   AIOS_QEMU         path to `qemu-system-x86_64`
//!   AIOS_KERNEL_TARGET_DIR, AIOS_ISO_OUT, AIOS_SKIP_QEMU, AIOS_QEMU_UEFI
//!   AIOS_KERNEL_FEATURES  comma-separated extra kernel Cargo features
//!                         (e.g. `limine,ring3-demo` for smoke proofs)
//!   AIOS_FALLBACK_KERNEL  path to the v2.79 rollback ELF. Required for a
//!                         dual-kernel image; defaults to
//!                         `out/aios-kernel-v279.elf` when present.
//!   AIOS_FALLBACK_VERSION label for the rollback menu entry (default `v2.79`)
//!   AIOS_QEMU_USB     boot the USB-stick image (`aios-kernel-usb.img`) as USB
//!                     mass storage instead of the ISO as a CD-ROM

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("aios-kernel-run has no parent dir")
        .to_path_buf()
}

fn kernel_manifest_dir() -> PathBuf {
    repo_root().join("aios-kernel")
}

/// Reads the kernel's single source of truth for its release version.
fn kernel_version() -> String {
    let manifest = fs::read_to_string(kernel_manifest_dir().join("Cargo.toml"))
        .expect("failed to read aios-kernel/Cargo.toml");
    parse_package_version(&manifest).expect("aios-kernel/Cargo.toml has no package version")
}

fn parse_package_version(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(value) = line.strip_prefix("version") {
            let value = value.trim_start();
            if let Some(value) = value.strip_prefix('=') {
                let version = value.trim().trim_matches('"').trim_matches('\'');
                if !version.is_empty() {
                    return Some(version.to_owned());
                }
            }
        }
    }
    None
}

/// `2.84.0` -> `v2.84+`: the menu advertises the release train, not a patch.
fn release_train(semver: &str) -> String {
    let mut parts = semver.split('.');
    let major = parts.next().unwrap_or("0");
    let minor = parts.next().unwrap_or("0");
    format!("v{major}.{minor}+")
}

fn fallback_kernel(out_dir: &Path) -> PathBuf {
    env_path("AIOS_FALLBACK_KERNEL", out_dir.join("aios-kernel-v279.elf"))
}

fn fallback_version() -> String {
    env::var("AIOS_FALLBACK_VERSION").unwrap_or_else(|_| "v2.79".to_owned())
}

fn iso_root() -> PathBuf {
    repo_root()
        .parent()
        .expect("aios-core has no parent dir")
        .join("iso")
}

fn env_path(key: &str, default: PathBuf) -> PathBuf {
    env::var_os(key).map(PathBuf::from).unwrap_or(default)
}

fn limine_dir() -> PathBuf {
    env_path(
        "AIOS_LIMINE_DIR",
        iso_root().join("limine").join("limine-binary"),
    )
}

fn limine_tool() -> PathBuf {
    env_path(
        "AIOS_LIMINE_TOOL",
        limine_dir()
            .join("limine-tool-windows-x86")
            .join("limine.exe"),
    )
}

fn xorriso() -> PathBuf {
    env_path(
        "AIOS_XORRISO",
        iso_root()
            .join("msys")
            .join("usr")
            .join("bin")
            .join("xorriso.exe"),
    )
}

fn qemu_dir() -> PathBuf {
    repo_root()
        .parent()
        .expect("aios-core has no parent dir")
        .join("tools")
        .join("qemu")
}

fn build_kernel(target_dir: &Path) -> PathBuf {
    let manifest = kernel_manifest_dir().join("Cargo.toml");
    let mut features = env::var("AIOS_KERNEL_FEATURES").unwrap_or_default();
    if !features.split(',').any(|f| f.trim() == "limine") {
        if features.is_empty() {
            features.push_str("limine");
        } else {
            features.push_str(",limine");
        }
    }
    let status = Command::new("cargo")
        .current_dir(kernel_manifest_dir())
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .args(["--target", "x86_64-unknown-none", "--release"])
        .args(["--features", &features])
        .env("CARGO_TARGET_DIR", target_dir)
        .status()
        .expect("failed to spawn cargo for the kernel build");
    assert!(status.success(), "kernel build failed");

    target_dir
        .join("x86_64-unknown-none")
        .join("release")
        .join("aios-kernel")
}

/// Windows path -> forward-slash form accepted by native tools.
fn forward(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Windows path -> MSYS2 form (`C:\a\b` -> `/c/a/b`) for the MSYS xorriso build.
fn msys_path(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let drive = s.chars().next().unwrap().to_ascii_lowercase();
        format!("/{}/{}", drive, s[2..].trim_start_matches('/'))
    } else {
        s
    }
}

fn stage(kernel_elf: &Path, fallback_elf: &Path, staging: &Path) -> std::io::Result<()> {
    if staging.exists() {
        fs::remove_dir_all(staging)?;
    }
    fs::create_dir_all(staging.join("boot"))?;
    fs::create_dir_all(staging.join("EFI").join("BOOT"))?;

    fs::copy(kernel_elf, staging.join("boot").join("aios-kernel"))?;
    fs::copy(
        fallback_elf,
        staging.join("boot").join("aios-kernel-fallback"),
    )?;

    let limine = limine_dir();
    fs::copy(
        limine.join("limine-bios-cd.bin"),
        staging.join("boot").join("limine-bios-cd.bin"),
    )?;
    fs::copy(
        limine.join("limine-uefi-cd.bin"),
        staging.join("boot").join("limine-uefi-cd.bin"),
    )?;
    fs::copy(
        limine.join("limine-bios.sys"),
        staging.join("boot").join("limine-bios.sys"),
    )?;
    fs::copy(
        limine.join("BOOTX64.EFI"),
        staging.join("EFI").join("BOOT").join("BOOTX64.EFI"),
    )?;

    let conf = menu_text(&release_train(&kernel_version()), &fallback_version());
    fs::write(staging.join("boot").join("limine.conf"), conf)?;
    Ok(())
}

fn menu_text(current: &str, rollback: &str) -> String {
    format!(
        "timeout: 3\n\
\n\
/AIOS kernel {current} (current, Limine + GOP)\n\
    protocol: limine\n\
    kernel_path: boot():/boot/aios-kernel\n\
\n\
/AIOS kernel {rollback} rollback (Limine + GOP)\n\
    protocol: limine\n\
    kernel_path: boot():/boot/aios-kernel-fallback\n"
    )
}

/// Checks the isohybrid markers produced by xorriso + `limine bios-install`.
fn verify_hybrid_image(path: &Path) -> std::io::Result<()> {
    use std::io::Read;

    let mut file = fs::File::open(path)?;
    let mut head = [0u8; 0x9000];
    if file.read_exact(&mut head).is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "image is too small for an ISO primary volume descriptor",
        ));
    }
    if head[510] != 0x55 || head[511] != 0xAA {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "missing Limine MBR signature 0x55AA",
        ));
    }
    if ![
        &head[0x8001..0x8006],
        &head[0x8801..0x8806],
        &head[0x9001..0x9006],
    ]
    .iter()
    .any(|marker| *marker == b"CD001")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "missing ISO9660 CD001 volume descriptor",
        ));
    }
    Ok(())
}

fn create_iso(staging: &Path, iso: &Path) -> std::io::Result<()> {
    if let Some(parent) = iso.parent() {
        fs::create_dir_all(parent)?;
    }
    if iso.exists() {
        fs::remove_file(iso)?;
    }
    let status = Command::new(xorriso())
        .args([
            "-as",
            "mkisofs",
            "-b",
            "boot/limine-bios-cd.bin",
            "-no-emul-boot",
            "-boot-load-size",
            "4",
            "-boot-info-table",
            "--efi-boot",
            "boot/limine-uefi-cd.bin",
            "--efi-boot-part",
            "--efi-boot-image",
            "--protective-msdos-label",
            "-V",
            "AIOS_KERNEL",
            "-o",
            &msys_path(iso),
            &msys_path(staging),
        ])
        .status()
        .expect("failed to spawn xorriso");
    assert!(status.success(), "xorriso failed to create the ISO");

    let status = Command::new(limine_tool())
        .arg("bios-install")
        .arg(forward(iso))
        .status()
        .expect("failed to spawn the limine tool");
    assert!(status.success(), "limine bios-install failed");
    Ok(())
}

/// Materializes the bootable USB-stick image: the isohybrid ISO is already
/// bootable from a flash drive (Limine MBR for BIOS + ESP for UEFI), so the
/// image is a byte copy of it.
fn create_usb_image(iso: &Path, img: &Path) -> std::io::Result<()> {
    if let Some(parent) = img.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(iso, img)?;
    Ok(())
}

fn find_qemu() -> PathBuf {
    if let Some(explicit) = env::var_os("AIOS_QEMU") {
        return PathBuf::from(explicit);
    }
    let local = qemu_dir().join("qemu-system-x86_64.exe");
    if local.exists() {
        return local;
    }
    if let Ok(found) = Command::new("qemu-system-x86_64").arg("--version").output() {
        if found.status.success() {
            return PathBuf::from("qemu-system-x86_64");
        }
    }
    for c in [
        r"C:\Program Files\qemu\qemu-system-x86_64.exe",
        r"C:\Program Files (x86)\qemu\qemu-system-x86_64.exe",
    ] {
        if Path::new(c).exists() {
            return PathBuf::from(c);
        }
    }
    panic!("qemu-system-x86_64 not found (set AIOS_QEMU)");
}

/// Locates the OVMF code + vars firmware, if QEMU ships it.
fn ovmf() -> Option<(PathBuf, PathBuf)> {
    let share = qemu_dir().join("share");
    let code = share.join("edk2-x86_64-code.fd");
    let vars = share.join("edk2-i386-vars.fd");
    if code.exists() && vars.exists() {
        Some((code, vars))
    } else {
        None
    }
}

fn run_qemu(qemu: &Path, iso: &Path, usb_img: &Path, out_dir: &Path) {
    let mut cmd = Command::new(qemu);

    // Boot media: a USB stick image (qemu-xhci + usb-storage, bootindex=1) when
    // requested, otherwise the ISO as a CD-ROM.
    let want_usb = env::var("AIOS_QEMU_USB").map(|v| v != "0").unwrap_or(false);
    if want_usb {
        println!("boot media: USB stick image {}", usb_img.display());
        cmd.args(["-m", "512M"])
            .args(["-vga", "std"])
            .args(["-serial", "stdio"])
            .args(["-display", "none"])
            .args(["-no-reboot"])
            .args(["-device", "qemu-xhci"])
            .args(["-drive"])
            .arg(format!(
                "if=none,id=aiosusb,format=raw,file={}",
                forward(usb_img)
            ))
            .args(["-device", "usb-storage,drive=aiosusb,bootindex=1"]);
    } else {
        cmd.args(["-cdrom", &forward(iso)])
            .args(["-boot", "d"])
            .args(["-m", "512M"])
            .args(["-vga", "std"])
            .args(["-serial", "stdio"])
            .args(["-display", "none"])
            .args(["-no-reboot"]);
    }

    // UEFI (GOP) by default when OVMF is available; legacy BIOS (VBE) otherwise
    // or when AIOS_QEMU_UEFI=0.
    let want_uefi = env::var("AIOS_QEMU_UEFI").map(|v| v != "0").unwrap_or(true);
    if want_uefi {
        if let Some((code, vars)) = ovmf() {
            let vars_copy = out_dir.join("OVMF_VARS.fd");
            let _ = fs::copy(&vars, &vars_copy);
            println!("firmware: UEFI (OVMF GOP)");
            cmd.args(["-drive"])
                .arg(format!(
                    "if=pflash,format=raw,readonly=on,file={}",
                    forward(&code)
                ))
                .args(["-drive"])
                .arg(format!("if=pflash,format=raw,file={}", forward(&vars_copy)));
        } else {
            println!("firmware: legacy BIOS (OVMF not found)");
        }
    } else {
        println!("firmware: legacy BIOS (requested)");
    }

    // Keyboard: boot-relevant only when the firmware does not provide PS/2
    // emulation (BIOS does; OVMF does not). Defaults on.
    let want_kbd = env::var("AIOS_QEMU_KBD").map(|v| v != "0").unwrap_or(true);
    if want_usb {
        // The controller is already present for the USB stick.
        if want_kbd {
            println!("usb: boot keyboard attached (qemu-xhci from boot media)");
            cmd.args(["-device", "usb-kbd"]);
        } else {
            println!("usb: boot media only (AIOS_QEMU_KBD=0)");
        }
    } else if want_kbd {
        println!("usb: qemu-xhci + usb-kbd attached");
        cmd.args(["-device", "qemu-xhci"])
            .args(["-device", "usb-kbd"]);
    } else {
        println!("usb: none (AIOS_QEMU_USB=0 and AIOS_QEMU_KBD=0)");
    }

    let status = cmd.status().expect("failed to spawn qemu-system-x86_64");
    std::process::exit(status.code().unwrap_or(1));
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = env_path("AIOS_ISO_OUT", root.join("out"));
    fs::create_dir_all(&out_dir).expect("failed to create output dir");

    let version = kernel_version();
    let current = release_train(&version);
    let rollback = fallback_version();
    let fallback = fallback_kernel(&out_dir);
    assert!(
        fallback.is_file(),
        "rollback kernel not found: {}. Supply a real v2.79 ELF via AIOS_FALLBACK_KERNEL",
        fallback.display()
    );
    println!(
        "kernel ELF: current {}, rollback {} ({})",
        current,
        rollback,
        fallback.display()
    );

    let target_dir = env::var_os("AIOS_KERNEL_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target").join("kernel-target"));

    let kernel_elf = build_kernel(&target_dir);
    println!("main kernel ELF: {}", kernel_elf.display());

    let staging = out_dir.join("kernel-iso");
    let iso = out_dir.join("aios-kernel.iso");

    stage(&kernel_elf, &fallback, &staging).expect("failed to stage the ISO tree");
    create_iso(&staging, &iso).expect("failed to create the ISO");
    verify_hybrid_image(&iso).expect("generated ISO failed hybrid boot checks");
    println!("bootable ISO: {}", iso.display());

    let usb_img = out_dir.join("aios-kernel-usb.img");
    create_usb_image(&iso, &usb_img).expect("failed to create the USB image");
    verify_hybrid_image(&usb_img).expect("generated USB image failed hybrid boot checks");
    println!("bootable USB image: {}", usb_img.display());

    if env::var_os("AIOS_SKIP_QEMU").is_some() {
        println!("AIOS_SKIP_QEMU=1 -> stopping after ISO/USB image creation");
        return;
    }

    let qemu = find_qemu();
    println!("QEMU: {}", qemu.display());
    run_qemu(&qemu, &iso, &usb_img, &out_dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_kernel_version_from_the_package_section_only() {
        let manifest = "[package]\nname = \"aios-kernel\"\nversion = \"2.84.0\"\n\n[dependencies]\nversion = \"9.9.9\"\n";
        assert_eq!(parse_package_version(manifest).as_deref(), Some("2.84.0"));
    }

    #[test]
    fn menu_advertises_current_and_rollback_entries() {
        let menu = menu_text(&release_train("2.84.0"), "v2.79");
        assert!(menu.contains("/AIOS kernel v2.84+ (current, Limine + GOP)"));
        assert!(menu.contains("/AIOS kernel v2.79 rollback (Limine + GOP)"));
        assert!(menu.contains("kernel_path: boot():/boot/aios-kernel\n"));
        assert!(menu.contains("kernel_path: boot():/boot/aios-kernel-fallback\n"));
    }
}
