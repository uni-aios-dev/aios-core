//! aios-storectl — applies live updates to the AIOS persistent boot store.
//!
//! The tool edits the tree the loader reads (`<esp>/aios/store`), so it can
//! target a mounted ESP, a staged image tree or any directory copy:
//!
//! ```text
//! aios-storectl verify   --esp <DIR>
//! aios-storectl update   --esp <DIR> --add <module>=<FILE> [--add ...]
//!                        [--fail-after payloads|staging|active|manifest|retired]
//! aios-storectl rollback --esp <DIR> [--fail-after ...]
//! aios-storectl init     --esp <DIR> --add <module>=<FILE> [--sequence N]
//! ```
//!
//! `verify` performs exactly the loader's checks and prints the slot that would
//! boot, `update` copies payloads into the inactive slot and commits in the
//! order documented by [`aios_bootstore`], and `--fail-after` stops between two
//! steps to reproduce a power loss. Exit status is non-zero when the store has
//! no bootable slot.

use aios_storectl::{
    commit, init, rollback, verify, DirStore, Payload, Step, Verification, MODULE_BLOCK,
    MODULE_CONFIG,
};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

const USAGE: &str = "usage:
  aios-storectl verify   --esp <DIR>
  aios-storectl update   --esp <DIR> --add <module>=<FILE> [--add ...] [--fail-after STEP]
  aios-storectl rollback --esp <DIR> [--fail-after STEP]
  aios-storectl init     --esp <DIR> --add <module>=<FILE> [--add ...] [--sequence N]

STEP: payloads, staging, active, manifest, retired";

fn main() {
    if let Err(error) = run() {
        eprintln!("aios-storectl: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or(USAGE)?;
    if command == "--help" || command == "-h" {
        println!("{USAGE}");
        return Ok(());
    }

    let mut esp: Option<PathBuf> = None;
    let mut adds: Vec<(String, PathBuf)> = Vec::new();
    let mut fail_after: Option<Step> = None;
    let mut sequence = 1u64;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--esp" => esp = Some(PathBuf::from(args.next().ok_or("--esp needs a path")?)),
            "--add" => {
                let spec = args.next().ok_or("--add needs <module>=<FILE>")?;
                let (name, path) = spec.split_once('=').ok_or("--add needs <module>=<FILE>")?;
                if name.is_empty() || path.is_empty() {
                    return Err("--add needs <module>=<FILE>".into());
                }
                adds.push((name.to_string(), PathBuf::from(path)));
            }
            "--fail-after" => {
                let value = args.next().ok_or("--fail-after needs a step")?;
                fail_after =
                    Some(Step::parse(&value).ok_or_else(|| format!("unknown step: {value}"))?);
            }
            "--sequence" => {
                sequence = args
                    .next()
                    .ok_or("--sequence needs a number")?
                    .parse()
                    .map_err(|_| "--sequence must be a number")?;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}").into()),
        }
    }

    let store = DirStore::new(esp.ok_or("--esp is required")?);
    match command.as_str() {
        "verify" => {
            let verification = verify(&store)?;
            report(&verification);
            if verification.boot_slot.is_none() {
                return Err("no bootable slot in the store".into());
            }
        }
        "update" => {
            let payloads = read_payloads(&adds)?;
            let result = commit(&store, &payloads, fail_after)?;
            println!(
                "aios-storectl: update -> slot {}, sequence {}, manifest copy {} ({} payload(s))",
                result.target_slot,
                result.sequence,
                result.manifest_copy,
                result.entries.len()
            );
            match (result.interrupted, fail_after) {
                (Some(step), _) => println!(
                    "aios-storectl: stopped after '{}' (simulated interruption)",
                    step.as_str()
                ),
                (None, _) => println!("aios-storectl: committed"),
            }
        }
        "rollback" => {
            let result = rollback(&store, fail_after)?;
            println!(
                "aios-storectl: rollback -> slot {}, sequence {}, manifest copy {} ({} payload(s))",
                result.target_slot,
                result.sequence,
                result.manifest_copy,
                result.entries.len()
            );
            match (result.interrupted, fail_after) {
                (Some(step), _) => println!(
                    "aios-storectl: stopped after '{}' (simulated interruption)",
                    step.as_str()
                ),
                (None, _) => println!("aios-storectl: rolled back"),
            }
        }
        "init" => {
            let payloads = read_payloads(&adds)?;
            let result = init(&store, &payloads, sequence)?;
            println!(
                "aios-storectl: initialised store sequence {} with {} payload(s) in slot 0",
                result.sequence,
                result.entries.len()
            );
        }
        other => return Err(format!("unknown command: {other}\n\n{USAGE}").into()),
    }
    Ok(())
}

fn read_payloads(adds: &[(String, PathBuf)]) -> Result<Vec<Payload>, Box<dyn Error>> {
    if adds.is_empty() {
        return Err("at least one --add <module>=<FILE> is required".into());
    }
    let mut payloads = Vec::with_capacity(adds.len());
    for (name, path) in adds {
        let kind = if name.starts_with("config/") {
            MODULE_CONFIG as u8
        } else {
            MODULE_BLOCK as u8
        };
        let data = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        println!(
            "aios-storectl: added {name} from {} ({} bytes)",
            path.display(),
            data.len()
        );
        payloads.push(Payload {
            name: name.clone(),
            kind,
            data,
        });
    }
    Ok(payloads)
}

fn report(verification: &Verification) {
    println!(
        "aios-storectl: manifest sequence {} (copy {}), active slot {}, previous slot {}",
        verification.sequence,
        verification.copy,
        verification.active_slot,
        verification.previous_slot
    );
    for (slot, reason) in &verification.rejected {
        println!("aios-storectl: slot {slot} unusable: {reason}");
    }
    match verification.boot_slot {
        Some(slot) => println!(
            "aios-storectl: loader would boot slot {slot} ({} payload(s))",
            verification.boot_entries
        ),
        None => println!("aios-storectl: no bootable slot"),
    }
}
