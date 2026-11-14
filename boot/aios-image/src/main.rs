//! aios-image — builds the AIOS boot image and optionally boots it in QEMU.
//!
//! The image is a plain GPT disk with one FAT32 EFI System Partition:
//!
//! ```text
//! LBA 0        protective MBR
//! LBA 1        GPT header
//! LBA 2..33    partition entries
//! LBA 2048     ESP (FAT32)
//! total - 33   backup entries
//! total - 1    backup header
//! ```
//!
//! The ESP contains `/EFI/BOOT/BOOTX64.EFI` (the loader), `/aios/kernel.elf`,
//! `/aios/boot.conf` and the module tree from `boot/assets`. Both the GPT and
//! the FAT32 volume are written by this crate; no external tool is involved.
//!
//! Usage:
//!
//! ```text
//! cargo run --manifest-path boot/aios-image/Cargo.toml -- [options]
//!
//!   --out <path>     write the image here (default: boot/out/aios-esp.img)
//!   --no-run         build the image only
//!   --headless       run QEMU without a display, serial on stdio
//!   --timeout <sec>  stop QEMU after N seconds
//! ```
//!
//! Environment: `AIOS_QEMU`, `AIOS_IMAGE_OUT`.

mod crc32;
mod fat32;
mod gpt;

use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Bytes per sector of the disk image.
const SECTOR_SIZE: u64 = 512;
/// The ESP starts at 1 MiB, the usual alignment for GPT partitions.
const ESP_FIRST_LBA: u64 = 2048;
/// ESP size in sectors: 320 MiB.
const ESP_SECTORS: u64 = 320 * 1024 * 1024 / SECTOR_SIZE;
/// Sectors reserved at the end for the backup GPT (32 entries + 1 header).
const TAIL_SECTORS: u64 = 33;
/// Total size of the image in sectors.
const TOTAL_SECTORS: u64 = ESP_FIRST_LBA + ESP_SECTORS + TAIL_SECTORS;

fn main() {
    if let Err(error) = run() {
        eprintln!("aios-image: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let crate_dir = manifest_dir();
    let boot_dir = crate_dir
        .parent()
        .ok_or("boot/aios-image has no parent directory")?
        .to_path_buf();
    let core_dir = boot_dir
        .parent()
        .ok_or("boot/ has no parent directory")?
        .to_path_buf();
    let options = Options::parse(&boot_dir)?;
    let loader = build_loader(&boot_dir)?;
    let kernel = build_kernel(&core_dir)?;
    create_image(&options.out, &loader, &kernel, &boot_dir.join("assets"))?;
    verify_image(&options.out)?;
    println!(
        "aios-image: {} ({:.0} MiB)",
        options.out.display(),
        file_size(&options.out)? as f64 / (1024.0 * 1024.0)
    );

    if options.run {
        run_qemu(&options, &core_dir)?;
    }
    Ok(())
}

/// Command-line options.
struct Options {
    /// Path of the image to write.
    out: PathBuf,
    /// Whether QEMU should be started after a successful build.
    run: bool,
    /// Start QEMU without a display and with serial on stdio.
    headless: bool,
    /// Kill QEMU after this many seconds.
    timeout: Option<u64>,
}

impl Options {
    fn parse(boot_dir: &Path) -> Result<Self, Box<dyn Error>> {
        let mut out = env::var_os("AIOS_IMAGE_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| boot_dir.join("out").join("aios-esp.img"));
        let mut run = env::var("AIOS_NO_RUN").map(|v| v == "0").unwrap_or(true);
        let mut headless = env::var("AIOS_HEADLESS").map(|v| v != "0").unwrap_or(false);
        let mut timeout = env::var("AIOS_QEMU_TIMEOUT")
            .ok()
            .and_then(|value| value.parse().ok());

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--out" => {
                    out = PathBuf::from(args.next().ok_or("--out needs a path")?);
                }
                "--no-run" => run = false,
                "--headless" => headless = true,
                "--timeout" => {
                    timeout = Some(
                        args.next()
                            .ok_or("--timeout needs a number")?
                            .parse()
                            .map_err(|_| "--timeout must be a number of seconds")?,
                    );
                }
                "--help" | "-h" => {
                    println!(
                        "usage: aios-image [--out PATH] [--no-run] [--headless] [--timeout SECS]"
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other}").into()),
            }
        }
        Ok(Self {
            out,
            run,
            headless,
            timeout,
        })
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn build_loader(boot_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let crate_dir = boot_dir.join("aios-boot");
    println!("aios-image: building aios-boot (x86_64-unknown-uefi, release)");
    let status = Command::new(cargo())
        .current_dir(&crate_dir)
        .args(["build", "--release", "--target", "x86_64-unknown-uefi"])
        .status()?;
    if !status.success() {
        return Err("aios-boot build failed".into());
    }
    let path = crate_dir
        .join("target")
        .join("x86_64-unknown-uefi")
        .join("release")
        .join("aios-boot.efi");
    require_file(&path)?;
    Ok(path)
}

fn build_kernel(core_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let crate_dir = core_dir.join("aios-kernel");
    println!("aios-image: building aios-kernel (x86_64-unknown-none, release)");
    let status = Command::new(cargo())
        .current_dir(&crate_dir)
        .args(["build", "--release", "--target", "x86_64-unknown-none"])
        .status()?;
    if !status.success() {
        return Err("aios-kernel build failed".into());
    }
    let path = crate_dir
        .join("target")
        .join("x86_64-unknown-none")
        .join("release")
        .join("aios-kernel");
    require_file(&path)?;
    Ok(path)
}

fn create_image(
    out: &Path,
    loader: &Path,
    kernel: &Path,
    assets: &Path,
) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    println!("aios-image: writing {}", out.display());
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(out)?;
    file.set_len(TOTAL_SECTORS * SECTOR_SIZE)?;

    gpt::write(
        &mut file,
        TOTAL_SECTORS,
        ESP_FIRST_LBA,
        ESP_FIRST_LBA + ESP_SECTORS - 1,
    )?;

    let mut fat = fat32::Fat32::format(file, ESP_FIRST_LBA * SECTOR_SIZE, ESP_SECTORS)?;
    println!(
        "aios-image: ESP FAT32, {} MiB at LBA {}",
        fat.volume_bytes() / (1024 * 1024),
        ESP_FIRST_LBA
    );
    let efi = fat.create_dir(fat32::ROOT_CLUSTER, "efi")?;
    let boot = fat.create_dir(efi, "boot")?;
    fat.write_file(boot, "BOOTX64.EFI", &fs::read(loader)?)?;

    let aios = fat.create_dir(fat32::ROOT_CLUSTER, "aios")?;
    fat.write_file(aios, "kernel.elf", &fs::read(kernel)?)?;
    copy_tree(&mut fat, aios, &assets.join("aios"), "")?;
    fat.finish()?;
    Ok(())
}

/// Recursively copies the contents of `path` into the ESP directory `dir`.
///
/// `prefix` is the path already consumed inside `/aios`, used only for the
/// progress lines.
fn copy_tree(
    fat: &mut fat32::Fat32<File>,
    dir: u32,
    path: &Path,
    prefix: &str,
) -> Result<(), Box<dyn Error>> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = format!("{prefix}/{name}");
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let child = fat.create_dir(dir, &name)?;
            copy_tree(fat, child, &entry.path(), &relative)?;
        } else if file_type.is_file() {
            let data = fs::read(entry.path())?;
            fat.write_file(dir, &name, &data)?;
            println!("aios-image:   /aios{relative} ({} bytes)", data.len());
        }
    }
    Ok(())
}

/// Reads back the image and checks the GPT, the FAT32 boot sector and the
/// first directory entries, so a broken writer fails here instead of in QEMU.
fn verify_image(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut sector = [0u8; 512];
    file.read_exact(&mut sector)?;
    if sector[510..512] != [0x55, 0xAA] || sector[446 + 4] != 0xEE {
        return Err("protective MBR is missing".into());
    }
    read_sector(&mut file, 1, &mut sector)?;
    if &sector[0..8] != b"EFI PART" {
        return Err("GPT header signature is missing".into());
    }
    read_sector(&mut file, ESP_FIRST_LBA, &mut sector)?;
    if &sector[82..90] != b"FAT32   " || &sector[3..11] != b"AIOSESP " {
        return Err("FAT32 boot sector is missing".into());
    }
    let geometry = fat32::geometry(ESP_SECTORS)?;
    let root_lba = ESP_FIRST_LBA
        + u64::from(geometry.reserved_sectors)
        + u64::from(geometry.num_fats) * u64::from(geometry.fat_sectors);
    read_sector(&mut file, root_lba, &mut sector)?;
    let names = directory_names(&sector);
    println!("aios-image: verified GPT + FAT32, root entries: {names:?}");
    if !names.iter().any(|name| name == "EFI") || !names.iter().any(|name| name == "AIOS") {
        return Err("ESP root is missing the EFI or AIOS directory".into());
    }

    let efi_cluster = directory_cluster(&sector, "EFI").ok_or("ESP has no EFI directory entry")?;
    let boot_sector = read_cluster(&mut file, &geometry, efi_cluster)?;
    let boot_cluster =
        directory_cluster(&boot_sector, "BOOT").ok_or("EFI has no BOOT directory entry")?;
    let boot_dir = read_cluster(&mut file, &geometry, boot_cluster)?;
    if !directory_names(&boot_dir)
        .iter()
        .any(|name| name == "BOOTX64.EFI")
    {
        return Err("EFI/BOOT does not contain BOOTX64.EFI".into());
    }

    let aios_cluster =
        directory_cluster(&sector, "AIOS").ok_or("ESP has no AIOS directory entry")?;
    let aios = read_cluster(&mut file, &geometry, aios_cluster)?;
    let aios_names = directory_names(&aios);
    if !aios_names.iter().any(|name| name == "KERNEL.ELF") {
        return Err("/aios does not contain kernel.elf".into());
    }
    let config_cluster =
        directory_cluster(&aios, "CONFIG").ok_or("/aios has no config directory")?;
    let config = read_cluster(&mut file, &geometry, config_cluster)?;
    if !directory_names(&config)
        .iter()
        .any(|name| name.starts_with("SYSTEM"))
    {
        return Err("/aios/config does not contain system.conf".into());
    }
    let blocks_cluster =
        directory_cluster(&aios, "BLOCKS").ok_or("/aios has no blocks directory")?;
    let blocks = read_cluster(&mut file, &geometry, blocks_cluster)?;
    if !directory_names(&blocks)
        .iter()
        .any(|name| name.starts_with("EMPTY"))
    {
        return Err("/aios/blocks does not contain empty.wasm".into());
    }
    println!("aios-image: verified kernel + modules (/aios{aios_names:?})");
    Ok(())
}

/// 8.3 names of the directory entries in one cluster, long-name entries
/// skipped. Names keep their FAT form (`KERNEL.ELF`), with the extension
/// separated by a dot when present.
fn directory_names(cluster: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    for entry in cluster.chunks(32) {
        if entry[0] == 0 {
            break;
        }
        if entry[11] == 0x0F || entry[0] == 0xE5 || entry[0] == b'.' {
            continue;
        }
        let base = String::from_utf8_lossy(&entry[0..8]).trim_end().to_string();
        let extension = String::from_utf8_lossy(&entry[8..11])
            .trim_end()
            .to_string();
        names.push(if extension.is_empty() {
            base
        } else {
            format!("{base}.{extension}")
        });
    }
    names
}

/// Cluster number of the directory entry called `name`, if present.
fn directory_cluster(cluster: &[u8], name: &str) -> Option<u32> {
    for entry in cluster.chunks(32) {
        if entry[0] == 0 {
            break;
        }
        if entry[11] == 0x0F || entry[0] == 0xE5 {
            continue;
        }
        let base = String::from_utf8_lossy(&entry[0..8]);
        let base = base.trim_end();
        if base == name {
            let high = u32::from(u16::from_le_bytes([entry[20], entry[21]])) << 16;
            let low = u32::from(u16::from_le_bytes([entry[26], entry[27]]));
            return Some(high | low);
        }
    }
    None
}

/// Reads the 4 KiB cluster `cluster` of the ESP.
fn read_cluster(
    file: &mut File,
    geometry: &fat32::Geometry,
    cluster: u32,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let sectors = u64::from(geometry.reserved_sectors)
        + u64::from(geometry.num_fats) * u64::from(geometry.fat_sectors)
        + u64::from(cluster - fat32::ROOT_CLUSTER) * u64::from(geometry.sectors_per_cluster);
    let mut data =
        vec![0u8; (fat32::SECTOR_SIZE * u64::from(geometry.sectors_per_cluster)) as usize];
    file.seek(SeekFrom::Start((ESP_FIRST_LBA + sectors) * SECTOR_SIZE))?;
    file.read_exact(&mut data)?;
    Ok(data)
}

fn read_sector(file: &mut File, lba: u64, buffer: &mut [u8; 512]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(lba * SECTOR_SIZE))?;
    file.read_exact(buffer)
}

fn run_qemu(options: &Options, core_dir: &Path) -> Result<(), Box<dyn Error>> {
    let qemu = find_qemu(core_dir)?;
    let (code, vars) = find_ovmf(core_dir)?;
    let out_dir = options
        .out
        .parent()
        .ok_or("image path has no parent directory")?;
    let vars_copy = out_dir.join("OVMF_VARS.fd");
    fs::copy(&vars, &vars_copy)?;

    let mut command = Command::new(&qemu);
    command.args(["-machine", "q35", "-m", "512M", "-vga", "std", "-no-reboot"]);
    command.arg("-drive").arg(format!(
        "if=pflash,format=raw,readonly=on,file={}",
        forward(&code)
    ));
    command
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", forward(&vars_copy)));
    command.arg("-drive").arg(format!(
        "if=none,id=aiosdisk,format=raw,file={}",
        forward(&options.out)
    ));
    command.args(["-device", "virtio-blk-pci,drive=aiosdisk,bootindex=1"]);
    command.args(["-device", "qemu-xhci", "-device", "usb-kbd"]);
    let serial_log = out_dir.join("serial.log");
    if options.headless {
        // A file keeps the byte stream intact: stdio mixes the kernel log with
        // QEMU's own output and ANSI escapes.
        command
            .arg("-serial")
            .arg(format!("file:{}", forward(&serial_log)));
        command.args(["-display", "none"]);
    } else {
        command.args(["-serial", "stdio"]);
        command.args(["-display", "gtk"]);
    }

    println!("aios-image: starting {}", qemu.display());
    let mut child = command.spawn()?;
    match options.timeout {
        None => {
            let status = child.wait()?;
            println!("aios-image: qemu exited with {status}");
        }
        Some(seconds) => {
            let deadline = Instant::now() + Duration::from_secs(seconds);
            loop {
                if let Some(status) = child.try_wait()? {
                    println!("aios-image: qemu exited with {status}");
                    break;
                }
                if Instant::now() >= deadline {
                    child.kill()?;
                    child.wait()?;
                    println!("aios-image: qemu stopped after {seconds}s");
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    if options.headless {
        print_serial_log(&serial_log)?;
    }
    Ok(())
}

/// Prints the serial log written by the headless run, so a caller sees the
/// kernel trace without a display.
fn print_serial_log(path: &Path) -> Result<(), Box<dyn Error>> {
    let log = fs::read(path)?;
    println!(
        "aios-image: serial log {} ({} bytes)",
        path.display(),
        log.len()
    );
    println!("{}", String::from_utf8_lossy(&log));
    Ok(())
}

fn find_qemu(core_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(explicit) = env::var_os("AIOS_QEMU") {
        return Ok(PathBuf::from(explicit));
    }
    let local = repo_root(core_dir)
        .join("tools")
        .join("qemu")
        .join("qemu-system-x86_64.exe");
    if local.exists() {
        return Ok(local);
    }
    for candidate in [
        r"C:\Program Files\qemu\qemu-system-x86_64.exe",
        r"C:\Program Files (x86)\qemu\qemu-system-x86_64.exe",
    ] {
        if Path::new(candidate).exists() {
            return Ok(PathBuf::from(candidate));
        }
    }
    Err("qemu-system-x86_64 not found (set AIOS_QEMU)".into())
}

fn find_ovmf(core_dir: &Path) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let share = repo_root(core_dir).join("tools").join("qemu").join("share");
    let code = share.join("edk2-x86_64-code.fd");
    let vars = share.join("edk2-i386-vars.fd");
    if code.exists() && vars.exists() {
        return Ok((code, vars));
    }
    Err("OVMF firmware not found in tools/qemu/share".into())
}

fn repo_root(core_dir: &Path) -> PathBuf {
    core_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| core_dir.to_path_buf())
}

fn cargo() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn require_file(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("expected build output at {}", path.display()).into())
    }
}

fn file_size(path: &Path) -> std::io::Result<u64> {
    Ok(fs::metadata(path)?.len())
}

fn forward(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
