use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn kernel_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("aios-kernel-run has no parent dir")
        .join("aios-kernel")
}

fn build_kernel(target_dir: &Path) -> PathBuf {
    let manifest = kernel_manifest_dir().join("Cargo.toml");
    let status = Command::new("cargo")
        .current_dir(kernel_manifest_dir())
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .args(["--target", "x86_64-unknown-none", "--release"])
        .env("CARGO_TARGET_DIR", target_dir)
        .status()
        .expect("failed to spawn cargo for the kernel build");
    assert!(status.success(), "kernel build failed");

    target_dir
        .join("x86_64-unknown-none")
        .join("release")
        .join("aios-kernel")
}

fn create_bios_image(kernel_elf: &Path, out: &Path) {
    bootloader::BiosBoot::new(kernel_elf)
        .create_disk_image(out)
        .expect("failed to create BIOS disk image");
}

fn find_qemu() -> PathBuf {
    if let Ok(found) = Command::new("qemu-system-x86_64").arg("--version").output() {
        if found.status.success() {
            return PathBuf::from("qemu-system-x86_64");
        }
    }
    let candidates = [
        r"C:\Program Files\qemu\qemu-system-x86_64.exe",
        r"C:\Program Files (x86)\qemu\qemu-system-x86_64.exe",
    ];
    for c in candidates {
        if Path::new(c).exists() {
            return PathBuf::from(c);
        }
    }
    panic!("qemu-system-x86_64 not found on PATH nor in C:\\Program Files\\qemu");
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = env::var_os("AIOS_KERNEL_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target").join("kernel-target"));

    let kernel_elf = build_kernel(&target_dir);
    println!("kernel ELF: {}", kernel_elf.display());

    let bios_path = env::var_os("AIOS_BIOS_IMAGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("bios.img"));
    create_bios_image(&kernel_elf, &bios_path);
    println!("BIOS disk image: {}", bios_path.display());

    let qemu = find_qemu();
    println!("QEMU: {}", qemu.display());

    let status = Command::new(&qemu)
        .args([
            "-drive",
            &format!("format=raw,file={}", bios_path.display()),
        ])
        .args(["-serial", "stdio"])
        .args(["-display", "none"])
        .args(["-no-reboot"])
        .status()
        .expect("failed to spawn qemu-system-x86_64");
    let code = status.code().unwrap_or(1);
    std::process::exit(code);
}
