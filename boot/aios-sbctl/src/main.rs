//! Command-line front end for the AIOS Secure Boot signing tool.

use aios_sbctl::{
    create_enrollment_auth, generate_keys, hex, sign_pe_file, sign_pe_file_to, verify_pe_file,
    KeyMaterial,
};
use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aios-sbctl: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    match command.as_str() {
        "keygen" => {
            let dir = value(&mut args, "--dir").ok_or("--dir is required")?;
            let keys = generate_keys(&PathBuf::from(dir))?;
            println!("PK:  {}", keys.pk_crt.display());
            println!("KEK: {}", keys.kek_crt.display());
            println!("DB:  {}", keys.db_crt.display());
            Ok(())
        }
        "sign" => {
            let image = value(&mut args, "--image").ok_or("--image is required")?;
            let keys_dir = value(&mut args, "--keys").ok_or("--keys is required")?;
            let out = value(&mut args, "--out");
            let keys = KeyMaterial::paths(&PathBuf::from(keys_dir));
            let signature = match out {
                Some(out) => sign_pe_file_to(&PathBuf::from(image), &PathBuf::from(out), &keys)?,
                None => sign_pe_file(&PathBuf::from(image), &keys)?,
            };
            println!("PE SHA-256: {}", hex(&signature.pe_hash));
            println!("PKCS#7 bytes: {}", signature.pkcs7.len());
            Ok(())
        }
        "verify" => {
            let image = value(&mut args, "--image").ok_or("--image is required")?;
            let keys_dir = value(&mut args, "--keys").ok_or("--keys is required")?;
            let keys = KeyMaterial::paths(&PathBuf::from(keys_dir));
            let signature = verify_pe_file(&PathBuf::from(image), &keys)?;
            println!("signature OK");
            println!("PE SHA-256: {}", hex(&signature.pe_hash));
            Ok(())
        }
        "enroll" => {
            let keys_dir = value(&mut args, "--keys").ok_or("--keys is required")?;
            let out = value(&mut args, "--out").ok_or("--out is required")?;
            let keys = KeyMaterial::paths(&PathBuf::from(keys_dir));
            let files = create_enrollment_auth(&keys, &PathBuf::from(out))?;
            for file in files {
                println!("{}", file.display());
            }
            Ok(())
        }
        _ => Err(usage().into()),
    }
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Option<String> {
    let rest = args.by_ref();
    while let Some(arg) = rest.next() {
        if arg == flag {
            return rest.next();
        }
    }
    None
}

fn usage() -> String {
    "usage: aios-sbctl keygen --dir DIR | sign --image EFI [--out EFI] --keys DIR | verify --image EFI --keys DIR | enroll --keys DIR --out DIR".to_string()
}
