//! `aios-easylang` — compiles an EasyLang file into an AIOS WASM block.
//!
//! ```text
//! aios-easylang hello.easy
//! aios-easylang hello.easy -o blocks/hello.wasm
//! aios-easylang hello.easy --check
//! ```

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut check = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "usage: aios-easylang <input.easy> [-o <output.wasm>] [--check]\n\n\
                     Without -o the output is the input path with a .wasm extension.\n\
                     --check compiles the program and prints its size without writing."
                );
                return Ok(ExitCode::SUCCESS);
            }
            "-o" | "--output" => {
                let Some(path) = args.next() else {
                    return Err(String::from("error: -o needs a path"));
                };
                output = Some(PathBuf::from(path));
            }
            "--check" => check = true,
            other if other.starts_with('-') => {
                return Err(format!("error: unknown option '{other}'"));
            }
            other => {
                if input.is_some() {
                    return Err(String::from("error: only one input file is accepted"));
                }
                input = Some(PathBuf::from(other));
            }
        }
    }

    let input = input.ok_or_else(|| String::from("error: no input file"))?;
    let source = fs::read_to_string(&input)
        .map_err(|error| format!("error: cannot read {}: {error}", input.display()))?;
    let bytes =
        aios_easylang::compile(&source).map_err(|error| format!("{}: {error}", input.display()))?;

    if check {
        println!("{}: ok ({} bytes)", input.display(), bytes.len());
        return Ok(ExitCode::SUCCESS);
    }

    let output = output.unwrap_or_else(|| input.with_extension("wasm"));
    fs::write(&output, &bytes)
        .map_err(|error| format!("error: cannot write {}: {error}", output.display()))?;
    println!(
        "{} -> {} ({} bytes)",
        input.display(),
        output.display(),
        bytes.len()
    );
    Ok(ExitCode::SUCCESS)
}
