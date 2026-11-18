//! aios-install — validates and writes the AIOS UEFI disk image.
//!
//! ```text
//! aios-install --list
//! aios-install --image <IMG> --device <PATH>            # validate and print the plan
//! aios-install --image <IMG> --device <PATH> --write --yes
//! ```
//!
//! `<PATH>` is either a raw file (for tests and file-backed disks) or a Windows
//! physical disk such as `\\.\PhysicalDrive2`. Without `--write` nothing is
//! modified; a write additionally requires `--yes`. Unmount every volume on the
//! target before writing.

use aios_install::{execute, list_devices, plan, verify, TargetKind};
use std::error::Error;
use std::path::PathBuf;

const USAGE: &str = "usage:
  aios-install --list
  aios-install --image <IMG> --device <PATH> [--write --yes]

<PATH> is a raw file or a Windows \\\\.\\PhysicalDriveN path.
Without --write the tool only validates and prints the plan;
--write requires --yes. Unmount target volumes first.";

fn main() {
    if let Err(error) = run() {
        eprintln!("aios-install: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut image: Option<PathBuf> = None;
    let mut device: Option<PathBuf> = None;
    let mut list = false;
    let mut write = false;
    let mut yes = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--image" => image = Some(PathBuf::from(args.next().ok_or("--image needs a path")?)),
            "--device" => device = Some(PathBuf::from(args.next().ok_or("--device needs a path")?)),
            "--list" => list = true,
            "--write" => write = true,
            "--yes" => yes = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}").into()),
        }
    }

    if list {
        let devices = list_devices();
        if devices.is_empty() {
            println!("aios-install: no physical disks found (use a raw file on this host)");
        }
        for found in devices {
            println!(
                "aios-install: {} ({:.0} MiB, protective MBR: {})",
                found.path,
                found.bytes as f64 / (1024.0 * 1024.0),
                found.protective_mbr
            );
        }
        return Ok(());
    }

    let image = image.unwrap_or_else(default_image);
    let device = device.ok_or(format!("--device is required\n\n{USAGE}"))?;
    let planned = plan(&image, &device)?;
    println!(
        "aios-install: image {} ({:.0} MiB, CRC32 {:08X})",
        planned.image.display(),
        planned.info.bytes as f64 / (1024.0 * 1024.0),
        planned.image_crc
    );
    println!(
        "aios-install: GPT backup LBA {}, ESP LBA {}..{}, backup CRC checked",
        planned.info.backup_lba, planned.info.esp_first_lba, planned.info.esp_last_lba
    );
    println!(
        "aios-install: target {} ({}, {:.0} MiB)",
        planned.target.display(),
        match planned.kind {
            TargetKind::File => "raw file",
            TargetKind::PhysicalDevice => "physical disk",
        },
        planned.target_bytes as f64 / (1024.0 * 1024.0)
    );

    if !write {
        println!("aios-install: dry run — nothing was written (add --write --yes)");
        return Ok(());
    }
    if !yes {
        return Err("--write requires --yes; this destroys all data on the target".into());
    }

    let mut last_percent = 0;
    let report = execute(&planned, |done, total| {
        let percent = done * 100 / total;
        if percent >= last_percent + 10 || done == total {
            println!(
                "aios-install: wrote {:.0} / {:.0} MiB ({}%)",
                done as f64 / (1024.0 * 1024.0),
                total as f64 / (1024.0 * 1024.0),
                percent
            );
            last_percent = percent;
        }
    })?;
    let checked = verify(&planned)?;
    if checked != report {
        return Err("post-write verification disagrees with the copy report".into());
    }
    println!(
        "aios-install: wrote {} bytes to {}, verified {} sector(s), CRC32 {:08X}",
        report.written,
        planned.target.display(),
        report.verified_sectors,
        report.crc
    );
    Ok(())
}

fn default_image() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("out")
        .join("aios-esp.img")
}
