use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, exit};

use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

fn history_path() -> Option<PathBuf> {
    if let Ok(d) = env::var("AIOS_DATA_DIR") {
        let mut p = PathBuf::from(d);
        p.push("shell_history");
        Some(p)
    } else if let Some(mut p) = dirs::data_dir() {
        p.push("aios");
        fs::create_dir_all(&p).ok();
        p.push("shell_history");
        Some(p)
    } else {
        None
    }
}

fn main() {
    println!("\x1b[1;36mAIOS Shell\x1b[0m - type 'help' for commands, 'exit' to quit");
    println!("Type any command to run it. Built-in: help, clear, cd, pwd, env, history, exec, gui, tui");
    let mut rl = match DefaultEditor::new() {
        Ok(e) => e,
        Err(_) => {
            eprintln!("Failed to initialize shell");
            exit(1);
        }
    };
    if let Some(h) = history_path() {
        let _ = rl.load_history(&h);
    }
    loop {
        let prompt = match env::current_dir() {
            Ok(p) => format!("\x1b[1;32mai\x1b[0m:\x1b[1;34m{}\x1b[0m> ", p.display()),
            Err(_) => "\x1b[1;32mai\x1b[0m> ".to_string(),
        };
        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                if let Some(h) = history_path() {
                    let _ = rl.save_history(&h);
                }
                let mut parts = line.split_whitespace();
                let cmd = match parts.next() {
                    Some(c) => c,
                    None => continue,
                };
                match cmd {
                    "exit" | "quit" => break,
                    "clear" | "cls" => {
                        print!("\x1b[2J\x1b[H");
                        let _ = io::stdout().flush();
                    }
                    "pwd" => match env::current_dir() {
                        Ok(p) => println!("{}", p.display()),
                        Err(e) => eprintln!("pwd: {}", e),
                    },
                    "cd" => {
                        let target = parts.next().unwrap_or("~");
                        let path = if target == "~" {
                            dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
                        } else {
                            PathBuf::from(target)
                        };
                        if let Err(e) = env::set_current_dir(&path) {
                            eprintln!("cd: {}: {}", path.display(), e);
                        }
                    }
                    "help" => {
                        println!("AIOS Shell - Built-in commands:");
                        println!("  help     - Show this help");
                        println!("  clear    - Clear screen");
                        println!("  pwd      - Print working directory");
                        println!("  cd <dir> - Change directory");
                        println!("  env      - Show environment");
                        println!("  history  - Show command history");
                        println!("  exec <cmd> [args] - Execute command");
                        println!("  gui      - Start AIOS GUI");
                        println!("  tui      - Start AIOS TUI");
                        println!("  exit     - Exit shell");
                        println!("\nAny other command is executed via shell.");
                    }
                    "env" => {
                        let mut envs: Vec<_> = env::vars().collect();
                        envs.sort_by(|a, b| a.0.cmp(&b.0));
                        for (k, v) in envs {
                            println!("{}={}", k, v);
                        }
                    }
                    "history" => {
                        if let Some(h) = history_path() {
                            if let Ok(content) = fs::read_to_string(&h) {
                                for (i, l) in content.lines().enumerate() {
                                    if !l.is_empty() {
                                        println!("{}: {}", i+1, l);
                                    }
                                }
                            }
                        }
                    }
                    "gui" => {
                        let _ = Command::new("aios-gui").spawn();
                    }
                    "tui" => {
                        let _ = Command::new("aios").spawn();
                    }
                    "exec" => {
                        let rest: Vec<String> = parts.map(|x| x.to_string()).collect();
                        if rest.is_empty() {
                            eprintln!("exec: missing command");
                            continue;
                        }
                        let mut c = Command::new(&rest[0]);
                        if rest.len() > 1 {
                            c.args(&rest[1..]);
                        }
                        let _ = c.status();
                    }
                    _ => {
                        // Try to run as shell command
                        if cfg!(windows) {
                            let _ = Command::new("cmd").args(["/C", line]).status();
                        } else {
                            let _ = Command::new("sh").args(["-c", line]).status();
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
                continue;
            }
            Err(ReadlineError::Eof) => break,
            Err(err) => {
                eprintln!("Error: {:?}", err);
                break;
            }
        }
    }
    println!("Goodbye!");
}
