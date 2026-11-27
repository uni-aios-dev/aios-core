use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{exit, Command, Stdio};
use std::sync::{Arc, Mutex};

use rustyline::error::ReadlineError;
use rustyline::history::FileHistory;
use rustyline::{Config, EditMode, Editor};

use aios_console::control::{JobControl, ShellJobInfo};
use aios_console::host::HostBackend;
use aios_console::legacy::{
    shell_alias_set_text, shell_alias_text, shell_cd_error_text, shell_echo_text, shell_env_text,
    shell_env_value_text, shell_export_set_text, shell_history_text, shell_job_error_text,
    shell_jobs_text, shell_pwd_text, shell_source_missing_text, shell_source_recursion_text,
    SHELL_HELP,
};
#[cfg(unix)]
use aios_console::legacy::{shell_bg_text, shell_fg_text};
use aios_console::output::OutputControl;

#[derive(Clone)]
struct Job {
    id: usize,
    cmd: String,
    pid: u32,
}

/// Shared job table owned by the shell and injected into `HostBackend`.
///
/// The same table serves the bare `jobs`/`fg`/`bg` builtins and the canonical
/// `console jobs`/`fg <id>`/`bg <id>` commands, so both paths observe the same
/// live background processes.
#[derive(Clone, Default)]
struct JobTable {
    inner: Arc<Mutex<Vec<Job>>>,
}

impl JobTable {
    fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> Vec<Job> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn entries(&self) -> Vec<(usize, String, u32)> {
        self.snapshot()
            .into_iter()
            .map(|job| (job.id, job.cmd, job.pid))
            .collect()
    }

    fn push(&self, job: Job) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(job);
    }

    #[cfg(unix)]
    fn clear(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    #[cfg(unix)]
    fn last(&self) -> Option<Job> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last()
            .cloned()
    }

    fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    #[cfg(unix)]
    fn retain(&self, predicate: impl FnMut(&Job) -> bool) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(predicate);
    }

    #[cfg(unix)]
    fn remove(&self, id: usize) -> bool {
        let mut jobs = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match jobs.iter().position(|job| job.id == id) {
            Some(index) => {
                jobs.remove(index);
                true
            }
            None => false,
        }
    }
}

impl JobControl for JobTable {
    fn jobs(&self) -> Vec<ShellJobInfo> {
        self.snapshot()
            .into_iter()
            .map(|job| ShellJobInfo {
                id: job.id,
                cmd: job.cmd,
                pid: job.pid,
            })
            .collect()
    }

    fn fg(&self, id: usize) -> Result<ShellJobInfo, String> {
        #[cfg(unix)]
        {
            let job = self.snapshot().into_iter().find(|job| job.id == id);
            let Some(job) = job else {
                return Err(format!("no job {id}"));
            };
            let _ = unsafe { libc::kill(job.pid as i32, libc::SIGCONT) };
            self.remove(id);
            Ok(ShellJobInfo {
                id: job.id,
                cmd: job.cmd,
                pid: job.pid,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = id;
            Err("not supported on this platform".to_string())
        }
    }

    fn bg(&self, id: usize) -> Result<ShellJobInfo, String> {
        #[cfg(unix)]
        {
            let job = self.snapshot().into_iter().find(|job| job.id == id);
            let Some(job) = job else {
                return Err(format!("no job {id}"));
            };
            let _ = unsafe { libc::kill(job.pid as i32, libc::SIGCONT) };
            Ok(ShellJobInfo {
                id: job.id,
                cmd: job.cmd,
                pid: job.pid,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = id;
            Err("not supported on this platform".to_string())
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ShellOutcome {
    Lines(Vec<String>),
    Stderr(Vec<String>),
    Clear,
}

fn history_path() -> Option<PathBuf> {
    if let Ok(d) = env::var("AIOS_DATA_DIR") {
        let mut p = PathBuf::from(d);
        let _ = fs::create_dir_all(&p);
        p.push("shell_history");
        Some(p)
    } else if let Some(mut p) = dirs::data_dir() {
        p.push("aios");
        let _ = fs::create_dir_all(&p);
        p.push("shell_history");
        Some(p)
    } else if let Some(mut p) = dirs::home_dir() {
        p.push(".aios_shell_history");
        Some(p)
    } else {
        None
    }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(d) = env::var("AIOS_DATA_DIR") {
        let mut p = PathBuf::from(d);
        let _ = fs::create_dir_all(&p);
        p.push("shellrc");
        Some(p)
    } else if let Some(mut p) = dirs::config_dir() {
        p.push("aios");
        let _ = fs::create_dir_all(&p);
        p.push("shellrc");
        Some(p)
    } else {
        None
    }
}

fn aliases_path() -> Option<PathBuf> {
    if let Ok(d) = env::var("AIOS_DATA_DIR") {
        let mut p = PathBuf::from(d);
        let _ = fs::create_dir_all(&p);
        p.push("aliases.json");
        Some(p)
    } else {
        None
    }
}

fn load_aliases() -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(p) = aliases_path() {
        if let Ok(s) = fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<HashMap<String, String>>(&s) {
                map = v;
            }
        }
    }
    if !map.contains_key("ll") {
        map.insert("ll".to_string(), "ls -la".to_string());
    }
    if !map.contains_key("la") {
        map.insert("la".to_string(), "ls -la".to_string());
    }
    if !map.contains_key("l") {
        map.insert("l".to_string(), "ls -CF".to_string());
    }
    if !map.contains_key("gs") {
        map.insert("gs".to_string(), "git status".to_string());
    }
    if !map.contains_key("gd") {
        map.insert("gd".to_string(), "git diff".to_string());
    }
    if !map.contains_key("gl") {
        map.insert("gl".to_string(), "git log --oneline -10".to_string());
    }
    if !map.contains_key("grep") {
        map.insert("grep".to_string(), "grep --color=auto".to_string());
    }
    map
}

fn save_aliases(map: &HashMap<String, String>) {
    if let Some(p) = aliases_path() {
        if let Ok(s) = serde_json::to_string_pretty(map) {
            let _ = fs::write(p, s);
        }
    }
}

fn expand_aliases(line: &str, aliases: &HashMap<String, String>) -> String {
    let mut result = line.to_string();
    let mut depth = 0;
    let max_depth = 10;
    loop {
        if depth >= max_depth {
            break;
        }
        let mut parts = result.splitn(2, char::is_whitespace);
        let first = match parts.next() {
            Some(x) => x.to_string(),
            None => break,
        };
        if let Some(a) = aliases.get(&first) {
            // prevent self-reference
            if a.split_whitespace().next() == Some(first.as_str()) && depth > 0 {
                break;
            }
            match parts.next() {
                Some(rest) => {
                    result = format!("{} {}", a, rest.trim());
                }
                None => {
                    // check if alias expands to itself
                    if a.split_whitespace().next() == Some(first.as_str()) {
                        break;
                    }
                    result = a.clone();
                }
            }
            depth += 1;
        } else {
            break;
        }
    }
    result
}

fn is_gui_app(name: &str) -> bool {
    matches!(
        name,
        "aios-gui"
            | "firefox"
            | "chromium"
            | "xterm"
            | "rxvt"
            | "aterm"
            | "mlterm"
            | "gimp"
            | "thunar"
            | "pcmanfm"
            | "nautilus"
            | "geany"
            | "mousepad"
            | "leafpad"
    )
}

fn run_console_command(console: &mut HostBackend, rest: &str) {
    let input = if rest.is_empty() { "help" } else { rest };
    let output = console.execute_line(input);
    for line in &output.lines {
        println!("{line}");
    }
    match output.control {
        OutputControl::Clear => {
            print!("\x1b[2J\x1b[H");
            let _ = io::stdout().flush();
        }
        OutputControl::Exit(code) => exit(i32::from(code)),
        OutputControl::None => {}
    }
}

fn emit_lines(lines: &[String]) {
    for line in lines {
        println!("{line}");
    }
}

fn apply_clear() {
    print!("\x1b[2J\x1b[H");
    let _ = io::stdout().flush();
}

fn emit_stderr(lines: &[String]) {
    for line in lines {
        eprintln!("{line}");
    }
}

fn collect_env() -> Vec<(String, String)> {
    let mut envs: Vec<_> = env::vars().collect();
    envs.sort_by(|a, b| a.0.cmp(&b.0));
    envs
}

thread_local! { static SOURCING: RefCell<HashSet<String>> = RefCell::new(HashSet::new()); }

fn source_mark(key: String) -> bool {
    SOURCING.with(|s| !s.borrow_mut().insert(key))
}

fn source_unmark(key: &str) {
    SOURCING.with(|s| {
        s.borrow_mut().remove(key);
    });
}

fn prune_jobs(jobs: &JobTable) {
    #[cfg(unix)]
    jobs.retain(|job| unsafe { libc::kill(job.pid as i32, 0) == 0 });
    #[cfg(not(unix))]
    let _ = jobs;
}

fn expand_posix_vars<'a>(parts: impl Iterator<Item = &'a str>) -> Vec<String> {
    parts
        .map(|x| {
            if let Some(value) = x.strip_prefix('$') {
                if let Some((key, default)) = value.split_once('-') {
                    env::var(key).unwrap_or_else(|_| default.to_string())
                } else if let Some((key, default)) = value.split_once(':') {
                    env::var(key).unwrap_or_else(|_| default.to_string())
                } else {
                    env::var(value).unwrap_or_default()
                }
            } else {
                x.to_string()
            }
        })
        .collect()
}

fn read_shell_history_lines() -> Vec<String> {
    history_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|content| content.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(test)]
fn legacy_help_lines() -> Vec<String> {
    vec![
        "\x1b[1;36mтХФтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХЧ\x1b[0m".to_string(),
        "\x1b[1;36mтХС                    AIOS Shell v2.2                        тХС\x1b[0m".to_string(),
        "\x1b[1;36mтХЪтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХЭ\x1b[0m".to_string(),
        "\n\x1b[1;33m[BUILTIN]\x1b[0m exit,quit,clear,pwd,cd,env,export,unset,history,alias,unalias,source,type,echo,exec,jobs,fg,bg".to_string(),
        "\x1b[1;33m[FILES]\x1b[0m ls,cat,head,tail,less,grep,find,wc,cp,mv,rm,mkdir,touch,chmod,chown,ln,stat,file".to_string(),
        "\x1b[1;33m[SYSTEM]\x1b[0m ps,top,htop,free,df,du,uname,uptime,whoami,id,date,dmesg,kernel,lsmod,modinfo".to_string(),
        "\x1b[1;33m[NET]\x1b[0m ping,curl,wget,nc,ss,netstat,ip,ifconfig,route,arp,nslookup,dig,traceroute".to_string(),
        "\x1b[1;33m[DEBUG]\x1b[0m strace,lsof,lscpu,lsblk,lsusb,lspci,dmidecode,hdparm,iostat,vmstat".to_string(),
        "\x1b[1;33m[DEV]\x1b[0m git,make,cmake,gcc,g++,rustc,cargo,python,python3,node,npm,pip".to_string(),
        "\x1b[1;33m[ARCHIVE]\x1b[0m tar,gzip,gunzip,bzip2,xz,zip,unzip".to_string(),
        "\x1b[1;33m[AIOS]\x1b[0m gui,tui,aios,logs,hw,services,aios-shell-reload".to_string(),
        "\x1b[1;33m[CONSOLE]\x1b[0m console <command> (console help lists unified commands)".to_string(),
        "\x1b[1;33m[EDITORS]\x1b[0m vi,vim,nano,emacs,micro".to_string(),
        "\n\x1b[2mRun any program directly. Use & for background. TAB for completion.\x1b[0m".to_string(),
    ]
}

#[cfg(test)]
fn legacy_pwd_lines() -> Option<Vec<String>> {
    env::current_dir()
        .ok()
        .map(|path| vec![path.display().to_string()])
}

#[cfg(test)]
fn legacy_echo_lines<'a>(parts: impl Iterator<Item = &'a str>) -> Vec<String> {
    let rest: Vec<String> = parts
        .map(|x| {
            if x.starts_with('$') {
                let v = x.strip_prefix('$').unwrap_or(x);
                if let Some((k, d)) = v.split_once('-') {
                    env::var(k).unwrap_or_else(|_| d.to_string())
                } else if let Some((k, d)) = v.split_once(':') {
                    env::var(k).unwrap_or_else(|_| d.to_string())
                } else {
                    env::var(v).unwrap_or_default()
                }
            } else {
                x.to_string()
            }
        })
        .collect();
    vec![rest.join(" ")]
}

#[cfg(test)]
fn legacy_history_lines() -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(h) = history_path() {
        if let Ok(c) = fs::read_to_string(&h) {
            for (i, l) in c.lines().enumerate() {
                if !l.is_empty() {
                    lines.push(format!("{:5}: {}", i + 1, l));
                }
            }
        }
    }
    lines
}

#[cfg(test)]
fn legacy_alias_lines(aliases: &HashMap<String, String>) -> Vec<String> {
    let mut keys: Vec<_> = aliases.keys().collect();
    keys.sort();
    keys.into_iter()
        .map(|key| format!("alias {}='{}'", key, aliases.get(key).unwrap()))
        .collect()
}

#[cfg(test)]
fn legacy_alias_set_lines(rest: Vec<String>, aliases: &mut HashMap<String, String>) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in rest {
        if let Some((key, value)) = entry.split_once('=') {
            let value = value.trim_matches('\'').trim_matches('"').to_string();
            aliases.insert(key.to_string(), value.clone());
            lines.push(format!("alias {}='{}'", key, value));
        }
    }
    save_aliases(aliases);
    lines
}

#[cfg(test)]
fn legacy_env_lines() -> Vec<String> {
    let mut envs: Vec<_> = env::vars().collect();
    envs.sort_by(|a, b| a.0.cmp(&b.0));
    envs.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
}

#[cfg(test)]
fn legacy_cd_error_lines(path: &str, error: &str) -> Vec<String> {
    vec![format!("cd: {path}: {error}")]
}

#[cfg(test)]
fn legacy_export_set_lines(key: &str, value: &str) -> Vec<String> {
    vec![format!("export {key}={value}")]
}

#[cfg(test)]
fn legacy_env_value_lines(key: &str, value: &str) -> Vec<String> {
    vec![format!("{key}={value}")]
}

#[cfg(test)]
fn legacy_source_recursion_lines(path: &str) -> Vec<String> {
    vec![format!("source: {path}: recursion detected")]
}

#[cfg(test)]
fn legacy_source_missing_lines(path: &str) -> Vec<String> {
    vec![format!("source: {path}: No such file")]
}

fn execute_shell_adapter(
    expanded: &str,
    aliases: &mut HashMap<String, String>,
    jobs: &JobTable,
    job_counter: &mut usize,
    console: &mut HostBackend,
) -> Option<ShellOutcome> {
    let mut parts = expanded.split_whitespace();
    let cmd = parts.next()?.trim_end_matches('&');
    match cmd {
        "help" | "?" => Some(ShellOutcome::Lines(
            SHELL_HELP.iter().map(|line| line.to_string()).collect(),
        )),
        "clear" | "cls" => {
            let output = console.execute_line("clear");
            debug_assert!(output.lines.is_empty());
            Some(ShellOutcome::Clear)
        }
        "pwd" => env::current_dir()
            .ok()
            .map(|path| ShellOutcome::Lines(shell_pwd_text(&path.display().to_string()))),
        "echo" => Some(ShellOutcome::Lines(shell_echo_text(&expand_posix_vars(
            parts,
        )))),
        "history" => Some(ShellOutcome::Lines(shell_history_text(
            &read_shell_history_lines(),
        ))),
        "alias" => {
            let rest: Vec<String> = parts.map(|part| part.to_string()).collect();
            if rest.is_empty() {
                let entries: Vec<(&str, &str)> = aliases
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect();
                Some(ShellOutcome::Lines(shell_alias_text(&entries)))
            } else {
                let mut lines = Vec::new();
                for entry in rest {
                    if let Some((key, value)) = entry.split_once('=') {
                        let value = value.trim_matches('\'').trim_matches('"').to_string();
                        aliases.insert(key.to_string(), value.clone());
                        lines.extend(shell_alias_set_text(key, &value));
                    }
                }
                save_aliases(aliases);
                Some(ShellOutcome::Lines(lines))
            }
        }
        "unalias" => {
            if let Some(key) = parts.next() {
                aliases.remove(key);
                save_aliases(aliases);
            }
            Some(ShellOutcome::Lines(Vec::new()))
        }
        "cd" => {
            let target = parts.next().unwrap_or("~");
            let path = if target == "~" {
                dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
            } else if target == "-" {
                match env::var("OLDPWD") {
                    Ok(old) => PathBuf::from(old),
                    Err(_) => return Some(ShellOutcome::Lines(Vec::new())),
                }
            } else {
                PathBuf::from(target)
            };
            if let Ok(cur) = env::current_dir() {
                env::set_var("OLDPWD", cur);
            }
            match env::set_current_dir(&path) {
                Ok(()) => Some(ShellOutcome::Lines(Vec::new())),
                Err(e) => Some(ShellOutcome::Stderr(shell_cd_error_text(
                    &path.display().to_string(),
                    &e.to_string(),
                ))),
            }
        }
        "env" => Some(ShellOutcome::Lines(shell_env_text(&collect_env()))),
        "export" => {
            if let Some(arg) = parts.next() {
                if let Some((key, value)) = arg.split_once('=') {
                    env::set_var(key, value);
                    Some(ShellOutcome::Lines(shell_export_set_text(key, value)))
                } else {
                    match env::var(arg) {
                        Ok(value) => Some(ShellOutcome::Lines(shell_env_value_text(arg, &value))),
                        Err(_) => Some(ShellOutcome::Lines(Vec::new())),
                    }
                }
            } else {
                Some(ShellOutcome::Lines(shell_env_text(&collect_env())))
            }
        }
        "unset" => {
            if let Some(key) = parts.next() {
                env::remove_var(key);
            }
            Some(ShellOutcome::Lines(Vec::new()))
        }
        "source" | "." => {
            let Some(file) = parts.next() else {
                return Some(ShellOutcome::Lines(Vec::new()));
            };
            let key = file.to_string();
            if source_mark(key.clone()) {
                return Some(ShellOutcome::Stderr(shell_source_recursion_text(file)));
            }
            let outcome = if let Ok(content) = fs::read_to_string(file) {
                for line in content.lines() {
                    let l = line.trim();
                    if l.is_empty() || l.starts_with('#') || l == "exit" || l == "quit" {
                        continue;
                    }
                    run_command(l, aliases, jobs, job_counter, console);
                }
                Some(ShellOutcome::Lines(Vec::new()))
            } else {
                Some(ShellOutcome::Stderr(shell_source_missing_text(file)))
            };
            source_unmark(&key);
            outcome
        }
        "jobs" => {
            prune_jobs(jobs);
            let snapshot = jobs.entries();
            let entries: Vec<_> = snapshot
                .iter()
                .map(|(id, cmd, pid)| (*id, cmd.as_str(), *pid))
                .collect();
            Some(ShellOutcome::Lines(shell_jobs_text(&entries)))
        }
        "fg" => {
            #[cfg(unix)]
            {
                if let Some(job) = jobs.last() {
                    let _ = unsafe { libc::kill(job.pid as i32, libc::SIGCONT) };
                    jobs.clear();
                    Some(ShellOutcome::Lines(shell_fg_text(job.id, &job.cmd)))
                } else {
                    Some(ShellOutcome::Lines(shell_job_error_text(
                        "fg",
                        "no current job",
                    )))
                }
            }
            #[cfg(not(unix))]
            {
                Some(ShellOutcome::Lines(shell_job_error_text(
                    "fg",
                    "not supported on this platform",
                )))
            }
        }
        "bg" => {
            #[cfg(unix)]
            {
                if let Some(job) = jobs.last() {
                    let _ = unsafe { libc::kill(job.pid as i32, libc::SIGCONT) };
                    Some(ShellOutcome::Lines(shell_bg_text(job.id, &job.cmd)))
                } else {
                    Some(ShellOutcome::Lines(shell_job_error_text(
                        "bg",
                        "no current job",
                    )))
                }
            }
            #[cfg(not(unix))]
            {
                Some(ShellOutcome::Lines(shell_job_error_text(
                    "bg",
                    "not supported on this platform",
                )))
            }
        }
        _ => None,
    }
}

fn run_command(
    line: &str,
    aliases: &mut HashMap<String, String>,
    jobs: &JobTable,
    job_counter: &mut usize,
    console: &mut HostBackend,
) {
    prune_jobs(jobs);
    if let Some(rest) = aios_console::frontend::console_prefix(line) {
        run_console_command(console, rest);
        return;
    }
    let expanded = expand_aliases(line, aliases);
    if let Some(outcome) = execute_shell_adapter(&expanded, aliases, jobs, job_counter, console) {
        match outcome {
            ShellOutcome::Lines(lines) => emit_lines(&lines),
            ShellOutcome::Stderr(lines) => emit_stderr(&lines),
            ShellOutcome::Clear => apply_clear(),
        }
        return;
    }
    run_command_legacy(line, aliases, jobs, job_counter, console)
}

fn run_command_legacy(
    line: &str,
    aliases: &mut HashMap<String, String>,
    jobs: &JobTable,
    job_counter: &mut usize,
    console: &mut HostBackend,
) {
    let expanded = expand_aliases(line, aliases);
    let mut parts = expanded.split_whitespace();
    let cmd = match parts.next() {
        Some(c) => c.to_string(),
        None => return,
    };

    // Background execution
    let bg = expanded.ends_with(" &");
    let cmd_clean = if bg {
        cmd.trim_end_matches('&').to_string()
    } else {
        cmd.clone()
    };

    match cmd_clean.as_str() {
        "exit" | "quit" | "q" => exit(0),
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
            } else if target == "-" {
                if let Ok(old) = env::var("OLDPWD") {
                    PathBuf::from(old)
                } else {
                    return;
                }
            } else {
                PathBuf::from(target)
            };
            if let Ok(cur) = env::current_dir() {
                env::set_var("OLDPWD", cur);
            }
            if let Err(e) = env::set_current_dir(&path) {
                eprintln!("cd: {}: {}", path.display(), e);
            }
        }
        "help" | "?" => {
            println!("\x1b[1;36mтХФтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХЧ\x1b[0m");
            println!("\x1b[1;36mтХС                    AIOS Shell v2.2                        тХС\x1b[0m");
            println!("\x1b[1;36mтХЪтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХЭ\x1b[0m");
            println!("\n\x1b[1;33m[BUILTIN]\x1b[0m exit,quit,clear,pwd,cd,env,export,unset,history,alias,unalias,source,type,echo,exec,jobs,fg,bg");
            println!("\x1b[1;33m[FILES]\x1b[0m ls,cat,head,tail,less,grep,find,wc,cp,mv,rm,mkdir,touch,chmod,chown,ln,stat,file");
            println!("\x1b[1;33m[SYSTEM]\x1b[0m ps,top,htop,free,df,du,uname,uptime,whoami,id,date,dmesg,kernel,lsmod,modinfo");
            println!("\x1b[1;33m[NET]\x1b[0m ping,curl,wget,nc,ss,netstat,ip,ifconfig,route,arp,nslookup,dig,traceroute");
            println!("\x1b[1;33m[DEBUG]\x1b[0m strace,lsof,lscpu,lsblk,lsusb,lspci,dmidecode,hdparm,iostat,vmstat");
            println!("\x1b[1;33m[DEV]\x1b[0m git,make,cmake,gcc,g++,rustc,cargo,python,python3,node,npm,pip");
            println!("\x1b[1;33m[ARCHIVE]\x1b[0m tar,gzip,gunzip,bzip2,xz,zip,unzip");
            println!("\x1b[1;33m[AIOS]\x1b[0m gui,tui,aios,logs,hw,services,aios-shell-reload");
            println!("\x1b[1;33m[CONSOLE]\x1b[0m console <command> (console help lists unified commands)");
            println!("\x1b[1;33m[EDITORS]\x1b[0m vi,vim,nano,emacs,micro");
            println!("\n\x1b[2mRun any program directly. Use & for background. TAB for completion.\x1b[0m");
        }
        "env" => {
            let mut envs: Vec<_> = env::vars().collect();
            envs.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in envs {
                println!("{}={}", k, v);
            }
        }
        "export" => {
            if let Some(a) = parts.next() {
                if let Some((k, v)) = a.split_once('=') {
                    env::set_var(k, v);
                    println!("export {}={}", k, v);
                } else if let Ok(v) = env::var(a) {
                    println!("{}={}", a, v);
                }
            } else {
                run_command("env", aliases, jobs, job_counter, console);
            }
        }
        "unset" => {
            if let Some(k) = parts.next() {
                env::remove_var(k);
            }
        }
        "history" => {
            if let Some(h) = history_path() {
                if let Ok(c) = fs::read_to_string(&h) {
                    for (i, l) in c.lines().enumerate() {
                        if !l.is_empty() {
                            println!("{:5}: {}", i + 1, l);
                        }
                    }
                }
            }
        }
        "alias" => {
            let rest: Vec<String> = parts.map(|x| x.to_string()).collect();
            if rest.is_empty() {
                let mut keys: Vec<_> = aliases.keys().collect();
                keys.sort();
                for k in keys {
                    println!("alias {}='{}'", k, aliases.get(k).unwrap());
                }
            } else {
                for a in rest {
                    if let Some((k, v)) = a.split_once('=') {
                        let v = v.trim_matches('\'').trim_matches('"').to_string();
                        aliases.insert(k.to_string(), v);
                        println!("alias {}='{}'", k, aliases.get(k).unwrap());
                    }
                }
                save_aliases(aliases);
            }
        }
        "unalias" => {
            if let Some(k) = parts.next() {
                aliases.remove(k);
                save_aliases(aliases);
            }
        }
        "jobs" => {
            prune_jobs(jobs);
            if jobs.is_empty() {
                println!("No jobs");
            } else {
                for j in jobs.entries() {
                    println!("[{}]+ Running {} (pid {})", j.0, j.1, j.2);
                }
            }
        }
        "fg" => {
            #[cfg(unix)]
            {
                if let Some(j) = jobs.last() {
                    let _ = unsafe { libc::kill(j.pid as i32, libc::SIGCONT) };
                    println!("[{}]+ Running        {}", j.id, j.cmd);
                    jobs.clear();
                } else {
                    println!("fg: no current job");
                }
            }
            #[cfg(not(unix))]
            {
                println!("fg: not supported on this platform");
            }
        }
        "bg" => {
            #[cfg(unix)]
            {
                if let Some(j) = jobs.last() {
                    let _ = unsafe { libc::kill(j.pid as i32, libc::SIGCONT) };
                    println!("[{}]+ Running        &{}", j.id, j.cmd);
                } else {
                    println!("bg: no current job");
                }
            }
            #[cfg(not(unix))]
            {
                println!("bg: not supported on this platform");
            }
        }
        "gui" => {
            if let Ok(child) = Command::new("aios-gui")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .env(
                    "DISPLAY",
                    env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string()),
                )
                .spawn()
            {
                println!("Started aios-gui [PID {}]", child.id());
                if bg {
                    *job_counter += 1;
                    jobs.push(Job {
                        id: *job_counter,
                        cmd: "aios-gui".to_string(),
                        pid: child.id(),
                    });
                }
            }
        }
        "tui" => {
            if let Ok(_child) = Command::new("aios")
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
            {
                println!("Starting aios...");
            }
        }
        "aios" => {
            let args: Vec<String> = parts.map(|x| x.to_string()).collect();
            let mut c = Command::new("aios");
            c.args(&args)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            let _ = c.status();
        }
        "aios-shell-reload" => {
            println!("Reloading shellrc...");
            load_shellrc_simple(aliases, jobs, console);
        }
        "logs" => {
            for f in [
                "/var/log/syslog",
                "/var/log/messages",
                "/var/log/dmesg",
                "/tmp/xorg.log",
                "/var/log/xorg.log",
                "/var/log/auth.log",
            ] {
                if Path::new(f).exists() {
                    println!("\x1b[1;33mтХРтХРтХР {} тХРтХРтХР\x1b[0m", f);
                    if let Ok(mut file) = File::open(f) {
                        let mut content = String::new();
                        let _ = file.read_to_string(&mut content);
                        let lines: Vec<&str> = content.lines().collect();
                        let s = if lines.len() > 60 {
                            lines.len() - 60
                        } else {
                            0
                        };
                        for l in &lines[s..] {
                            println!("{}", l);
                        }
                    }
                    println!();
                }
            }
        }
        "hw" | "hardware" | "info" => {
            println!("\x1b[1;36mтХРтХРтХР System Information тХРтХРтХР\x1b[0m");
            let _ = Command::new("uname")
                .args(["-a"])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            println!("\n\x1b[1;36mтХРтХРтХР CPU тХРтХРтХР\x1b[0m");
            let _ = Command::new("sh")
                .args([
                    "-c",
                    "cat /proc/cpuinfo | grep -E 'model name|processor|cores' | head -20",
                ])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            println!("\n\x1b[1;36mтХРтХРтХР Memory тХРтХРтХР\x1b[0m");
            let _ = Command::new("free")
                .args(["-h"])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            println!("\n\x1b[1;36mтХРтХРтХР Disks тХРтХРтХР\x1b[0m");
            let _ = Command::new("lsblk")
                .args(["-o", "NAME,SIZE,TYPE,MOUNTPOINT"])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            println!("\n\x1b[1;36mтХРтХРтХР PCI Devices тХРтХРтХР\x1b[0m");
            let _ = Command::new("lspci")
                .args(["-nnk"])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            println!("\n\x1b[1;36mтХРтХРтХР USB Devices тХРтХРтХР\x1b[0m");
            let _ = Command::new("lsusb")
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
        }
        "services" => {
            println!("\x1b[1;36mтХРтХРтХР Running Processes тХРтХРтХР\x1b[0m");
            let _ = Command::new("ps")
                .args(["auxf"])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
        }
        "kernel" => {
            println!("\x1b[1;36mтХРтХРтХР Kernel Info тХРтХРтХР\x1b[0m");
            let _ = Command::new("uname")
                .args(["-r", "-v"])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            println!("\n\x1b[1;36mтХРтХРтХР Loaded Modules тХРтХРтХР\x1b[0m");
            let _ = Command::new("lsmod")
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
        }
        "which" => {
            if let Some(n) = parts.next() {
                let _ = Command::new("which")
                    .arg(n)
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status();
            }
        }
        "type" => {
            if let Some(n) = parts.next() {
                if aliases.contains_key(n) {
                    println!("{} is aliased to '{}'", n, aliases.get(n).unwrap());
                } else {
                    let builtins = [
                        "exit", "clear", "pwd", "cd", "help", "env", "export", "unset", "history",
                        "alias", "unalias", "gui", "tui", "aios", "logs", "hw", "services",
                        "kernel", "which", "exec", "source", "echo", "type", "ps", "free", "df",
                        "uname", "uptime", "whoami", "date", "ls", "cat", "grep", "find", "jobs",
                        "fg", "bg",
                    ];
                    if builtins.contains(&n) {
                        println!("{} is a shell builtin", n);
                    } else {
                        let _ = Command::new("which")
                            .arg(n)
                            .stdin(Stdio::inherit())
                            .stdout(Stdio::inherit())
                            .stderr(Stdio::inherit())
                            .status();
                    }
                }
            }
        }
        "exec" => {
            let rest: Vec<String> = parts.map(|x| x.to_string()).collect();
            if rest.is_empty() {
                eprintln!("exec: missing");
                return;
            }
            let mut c = Command::new(&rest[0]);
            if rest.len() > 1 {
                c.args(&rest[1..]);
            }
            c.stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            let _ = c.status();
        }
        "source" | "." => {
            if let Some(f) = parts.next() {
                thread_local! { static SOURCING: RefCell<HashSet<String>> = RefCell::new(HashSet::new()); }
                SOURCING.with(|s| {
                    let mut set = s.borrow_mut();
                    let key = f.to_string();
                    if set.contains(&key) {
                        eprintln!("source: {}: recursion detected", f);
                        return;
                    }
                    set.insert(key.clone());
                    if let Ok(content) = fs::read_to_string(f) {
                        for line in content.lines() {
                            let l = line.trim();
                            if l.is_empty() || l.starts_with('#') {
                                continue;
                            }
                            if l == "exit" || l == "quit" {
                                continue;
                            }
                            run_command(l, aliases, jobs, job_counter, &mut *console);
                        }
                    } else {
                        eprintln!("source: {}: No such file", f);
                    }
                    set.remove(&key);
                });
            }
        }
        "echo" => {
            let rest: Vec<String> = parts
                .map(|x| {
                    if x.starts_with('$') {
                        let v = x.strip_prefix('$').unwrap_or(x);
                        if let Some((k, d)) = v.split_once('-') {
                            env::var(k).unwrap_or_else(|_| d.to_string())
                        } else if let Some((k, d)) = v.split_once(':') {
                            env::var(k).unwrap_or_else(|_| d.to_string())
                        } else {
                            env::var(v).unwrap_or_default()
                        }
                    } else {
                        x.to_string()
                    }
                })
                .collect();
            println!("{}", rest.join(" "));
        }
        "ps" | "free" | "df" | "du" | "uname" | "uptime" | "whoami" | "id" | "date" | "ls"
        | "cat" | "head" | "tail" | "grep" | "find" | "wc" | "cp" | "mv" | "rm" | "mkdir"
        | "touch" | "chmod" | "chown" | "ln" | "stat" | "file" | "tar" | "gzip" | "gunzip"
        | "bzip2" | "xz" | "zip" | "unzip" | "curl" | "wget" | "ping" | "ip" | "ss" | "netstat"
        | "route" | "arp" | "nslookup" | "dig" | "traceroute" | "nc" | "kill" | "killall"
        | "top" | "htop" | "lsof" | "strace" | "lscpu" | "lsblk" | "lsusb" | "lspci"
        | "dmidecode" | "hdparm" | "iostat" | "vmstat" | "dmesg" | "git" | "make" | "cmake"
        | "gcc" | "g++" | "rustc" | "cargo" | "python" | "python3" | "node" | "npm" | "pip"
        | "pip3" | "vi" | "vim" | "nano" | "emacs" | "micro" | "less" | "more" | "hexdump"
        | "xxd" | "md5sum" | "sha256sum" | "mount" | "umount" | "modprobe" | "insmod" | "rmmod"
        | "lsmod" => {
            let mut c = Command::new(&cmd_clean);
            let args: Vec<String> = parts.map(|x| x.to_string()).collect();
            c.args(&args);
            c.stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            if bg {
                if let Ok(child) = c.spawn() {
                    *job_counter += 1;
                    jobs.push(Job {
                        id: *job_counter,
                        cmd: expanded.trim_end_matches(" &").to_string(),
                        pid: child.id(),
                    });
                    println!(
                        "[{}] {} {}",
                        *job_counter,
                        child.id(),
                        expanded.trim_end_matches(" &")
                    );
                }
            } else {
                let _ = c.status();
            }
        }
        _ => {
            // Try to run as GUI app in X if in X session
            if env::var("DISPLAY").is_ok() && is_gui_app(&cmd_clean) {
                let mut c = Command::new(&cmd_clean);
                let args: Vec<String> = parts.map(|x| x.to_string()).collect();
                c.args(&args);
                c.stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                if let Ok(child) = c.spawn() {
                    println!("Launched {} [PID {}]", cmd_clean, child.id());
                    if bg {
                        *job_counter += 1;
                        jobs.push(Job {
                            id: *job_counter,
                            cmd: expanded.clone(),
                            pid: child.id(),
                        });
                    }
                }
                return;
            }
            // Try direct exec
            let mut p2 = expanded.split_whitespace();
            if let Some(first) = p2.next() {
                let first_clean = first.trim_end_matches('&');
                let args: Vec<String> = p2.map(|x| x.to_string()).filter(|x| x != "&").collect();
                let mut c = Command::new(first_clean);
                c.args(&args);
                c.stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());
                if bg {
                    if let Ok(child) = c.spawn() {
                        *job_counter += 1;
                        jobs.push(Job {
                            id: *job_counter,
                            cmd: expanded.trim_end_matches(" &").to_string(),
                            pid: child.id(),
                        });
                        println!(
                            "[{}] {} {}",
                            *job_counter,
                            child.id(),
                            expanded.trim_end_matches(" &")
                        );
                    }
                    return;
                }
                if c.status().is_err() {
                    let mut c2 = Command::new("sh");
                    c2.args(["-c", &expanded]);
                    c2.stdin(Stdio::inherit())
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit());
                    let _ = c2.status();
                }
            }
        }
    }
}

fn load_shellrc_simple(
    aliases: &mut HashMap<String, String>,
    jobs: &JobTable,
    console: &mut HostBackend,
) {
    if let Some(p) = config_path() {
        if let Ok(content) = fs::read_to_string(p) {
            let mut jc = 0;
            for line in content.lines() {
                let l = line.trim();
                if l.is_empty() || l.starts_with('#') {
                    continue;
                }
                if l.starts_with("alias ") {
                    let rest = l.strip_prefix("alias ").unwrap_or("");
                    if let Some((k, v)) = rest.split_once('=') {
                        aliases.insert(
                            k.trim().to_string(),
                            v.trim().trim_matches('\'').trim_matches('"').to_string(),
                        );
                    }
                } else if l == "exit" || l == "quit" {
                    continue; // don't exit on shellrc load
                } else {
                    run_command(l, aliases, jobs, &mut jc, console);
                }
            }
            save_aliases(aliases);
        }
    }
}

fn main() {
    let mut aliases = load_aliases();
    let mut console = HostBackend::new();
    let jobs = JobTable::new();
    console.set_job_control(Arc::new(jobs.clone()));
    load_shellrc_simple(&mut aliases, &jobs, &mut console);

    println!("\x1b[1;36mтХФтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХЧ\x1b[0m");
    println!("\x1b[1;36mтХС                AIOS Shell v2.2                    тХС\x1b[0m");
    println!("\x1b[1;36mтХС        Full-Featured Command Interface            тХС\x1b[0m");
    println!("\x1b[1;36mтХЪтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХЭ\x1b[0m");
    println!("\x1b[2mType 'help' for all commands | TAB completion enabled\x1b[0m\n");

    let config = Config::builder()
        .history_ignore_space(true)
        .completion_type(rustyline::CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .build();

    let mut rl: Editor<(), FileHistory> =
        Editor::with_config(config).expect("Failed to init readline");
    if let Some(h) = history_path() {
        let _ = rl.load_history(&h);
    }

    let mut job_counter: usize = 0;

    loop {
        let pwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let user = env::var("USER")
            .unwrap_or_else(|_| env::var("LOGNAME").unwrap_or_else(|_| "root".to_string()));
        let host = env::var("HOSTNAME").unwrap_or_else(|_| "aios".to_string());
        let short_pwd = if pwd.to_string_lossy() == "/" {
            "/".to_string()
        } else {
            pwd.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("/")
                .to_string()
        };
        let prompt = format!(
            "\x1b[1;32m{}@{}\x1b[0m:\x1b[1;34m{}\x1b[0m$ ",
            user,
            host,
            if pwd.to_string_lossy().len() > 20 {
                format!(".../{}", short_pwd)
            } else {
                pwd.display().to_string()
            }
        );
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
                for cmd in line.split([';']) {
                    let cmd = cmd.trim();
                    if cmd.is_empty() {
                        continue;
                    }
                    // Handle &&
                    if cmd.contains("&&") {
                        for sub in cmd.split("&&") {
                            let sub = sub.trim();
                            if sub.is_empty() {
                                continue;
                            }
                            run_command(sub, &mut aliases, &jobs, &mut job_counter, &mut console);
                        }
                    } else {
                        run_command(cmd, &mut aliases, &jobs, &mut job_counter, &mut console);
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
            }
            Err(ReadlineError::Eof) => {
                println!("logout");
                break;
            }
            Err(e) => {
                eprintln!("Error: {:?}", e);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_console::registry::{resolve, ResolveError};

    #[test]
    fn posix_alias_expansion_maps_to_canonical_text_commands() {
        let mut aliases = HashMap::new();
        aliases.insert("ll".to_string(), "ls -la".to_string());
        aliases.insert("g".to_string(), "git".to_string());

        assert_eq!(expand_aliases("ll", &aliases), "ls -la");
        assert_eq!(expand_aliases("ll /tmp", &aliases), "ls -la /tmp");
        assert_eq!(expand_aliases("g status", &aliases), "git status");
        assert!(is_gui_app("firefox"));
        assert!(!is_gui_app("cargo"));

        let vfs = resolve("vfs ls").unwrap();
        assert_eq!(vfs.spec.id, "vfs");
        assert_eq!(vfs.subcommand, Some("ls"));

        let exec = resolve("exec ls -la").unwrap();
        assert_eq!(exec.spec.id, "exec");
        assert_eq!(exec.first_arg(), Some("ls"));

        assert!(matches!(resolve("gui"), Err(ResolveError::Deferred { .. })));
        assert!(matches!(resolve("tui"), Err(ResolveError::Deferred { .. })));
    }

    #[test]
    fn console_prefix_is_case_insensitive_and_keeps_arguments() {
        assert_eq!(
            aios_console::frontend::console_prefix("console ps"),
            Some("ps")
        );
        assert_eq!(
            aios_console::frontend::console_prefix("CONSOLE   ps aux"),
            Some("ps aux")
        );
        assert_eq!(
            aios_console::frontend::console_prefix("  console  block list "),
            Some("block list")
        );
        assert_eq!(aios_console::frontend::console_prefix("console"), Some(""));
        assert_eq!(aios_console::frontend::console_prefix("ps"), None);
        assert_eq!(aios_console::frontend::console_prefix("consolett ps"), None);
    }

    #[test]
    fn console_command_routes_through_the_unified_backend() {
        let mut console = HostBackend::new();
        let output = console.execute_line("echo MiXeD Case");
        assert_eq!(output.lines, ["MiXeD Case"]);

        let help = console.execute_line("help");
        assert!(help.lines.iter().any(|line| line.contains("vfs")));
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static DATA_DIR_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    struct DataDirGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        dir: PathBuf,
        previous: Option<std::ffi::OsString>,
    }

    impl DataDirGuard {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let previous = env::var_os("AIOS_DATA_DIR");
            let seq = DATA_DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                env::temp_dir().join(format!("aios-shell-parity-{}-{seq}", std::process::id()));
            let _ = fs::create_dir_all(&dir);
            env::set_var("AIOS_DATA_DIR", &dir);
            Self {
                _lock: lock,
                dir,
                previous,
            }
        }
    }

    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var("AIOS_DATA_DIR", value),
                None => env::remove_var("AIOS_DATA_DIR"),
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn bare_shell_static_adapters_match_legacy_text() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut aliases = HashMap::new();
        let mut console = HostBackend::new();
        let jobs = JobTable::new();
        let mut job_counter = 0;

        assert_eq!(
            execute_shell_adapter("help", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Lines(legacy_help_lines()))
        );
        assert_eq!(
            execute_shell_adapter("?", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Lines(legacy_help_lines()))
        );

        let pwd = legacy_pwd_lines().unwrap();
        assert_eq!(
            execute_shell_adapter("pwd", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Lines(pwd))
        );

        for line in ["echo", "echo hello", "echo MiXeD Case"] {
            let mut parts = line.split_whitespace();
            parts.next();
            assert_eq!(
                execute_shell_adapter(line, &mut aliases, &jobs, &mut job_counter, &mut console),
                Some(ShellOutcome::Lines(legacy_echo_lines(parts))),
                "input: {line}"
            );
        }

        assert_eq!(
            execute_shell_adapter("clear", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Clear)
        );
        assert_eq!(
            execute_shell_adapter("cls", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Clear)
        );
    }

    #[test]
    fn bare_shell_alias_adapters_match_legacy_text() {
        let _guard = DataDirGuard::new();
        let mut console = HostBackend::new();
        let mut aliases = HashMap::new();
        let jobs = JobTable::new();
        let mut job_counter = 0;
        aliases.insert("gs".to_string(), "git status".to_string());

        assert_eq!(
            execute_shell_adapter("alias", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Lines(legacy_alias_lines(&aliases)))
        );

        let mut expected = aliases.clone();
        let set_args = vec!["ll=ls".to_string(), "g=git".to_string()];
        assert_eq!(
            execute_shell_adapter(
                "alias ll=ls g=git",
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(legacy_alias_set_lines(
                set_args,
                &mut expected
            )))
        );
        assert_eq!(aliases, expected);

        assert_eq!(
            execute_shell_adapter(
                "unalias gs",
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(Vec::new()))
        );
        assert!(!aliases.contains_key("gs"));
    }

    #[test]
    fn bare_shell_history_adapter_matches_legacy_text() {
        let _guard = DataDirGuard::new();
        let mut aliases = HashMap::new();
        let mut console = HostBackend::new();
        let jobs = JobTable::new();
        let mut job_counter = 0;
        let history_file = history_path().unwrap();
        fs::write(&history_file, "first\n\nthird\n").unwrap();

        let expected = vec!["    1: first".to_string(), "    3: third".to_string()];
        assert_eq!(
            execute_shell_adapter(
                "history",
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(expected))
        );
        assert_eq!(
            execute_shell_adapter(
                "history",
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(legacy_history_lines()))
        );
    }

    #[test]
    fn unmigrated_bare_shell_commands_fall_through() {
        let mut aliases = HashMap::new();
        let mut console = HostBackend::new();
        let jobs = JobTable::new();
        let mut job_counter = 0;
        for line in ["ls", "gui", "tui", "exit", "aios-shell-reload"] {
            assert!(
                execute_shell_adapter(line, &mut aliases, &jobs, &mut job_counter, &mut console)
                    .is_none(),
                "input: {line}"
            );
        }
    }

    #[test]
    fn canonical_console_jobs_share_the_shell_job_table() {
        let mut console = HostBackend::new();
        let jobs = JobTable::new();
        console.set_job_control(Arc::new(jobs.clone()));

        assert_eq!(console.execute_line("jobs").lines, ["No jobs"]);

        let pid = std::process::id();
        jobs.push(Job {
            id: 7,
            cmd: "sleep 2".to_string(),
            pid,
        });
        assert_eq!(
            console.execute_line("jobs").lines,
            [format!("[7]+ Running sleep 2 (pid {pid})")]
        );
        assert!(console.execute_line("fg 99").lines[0].starts_with("fg:"));
        assert!(console.execute_line("bg 99").lines[0].starts_with("bg:"));
    }

    #[test]
    fn bare_shell_job_adapters_match_legacy_text_and_effects() {
        let mut aliases = HashMap::new();
        let mut console = HostBackend::new();
        let jobs = JobTable::new();
        let mut job_counter = 0;

        assert_eq!(
            execute_shell_adapter("jobs", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Lines(vec!["No jobs".to_string()]))
        );

        let pid = std::process::id();
        jobs.push(Job {
            id: 1,
            cmd: "sleep 1".to_string(),
            pid,
        });
        assert_eq!(
            execute_shell_adapter("jobs", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Lines(vec![format!(
                "[1]+ Running sleep 1 (pid {pid})"
            )]))
        );

        #[cfg(unix)]
        {
            assert_eq!(
                execute_shell_adapter("fg", &mut aliases, &jobs, &mut job_counter, &mut console),
                Some(ShellOutcome::Lines(vec![
                    "[1]+ Running        sleep 1".to_string()
                ]))
            );
            assert!(jobs.is_empty());

            jobs.push(Job {
                id: 1,
                cmd: "sleep 1".to_string(),
                pid,
            });
            assert_eq!(
                execute_shell_adapter("bg", &mut aliases, &jobs, &mut job_counter, &mut console),
                Some(ShellOutcome::Lines(vec![
                    "[1]+ Running        &sleep 1".to_string()
                ]))
            );
            assert_eq!(jobs.len(), 1);
        }

        #[cfg(not(unix))]
        {
            assert_eq!(
                execute_shell_adapter("fg", &mut aliases, &jobs, &mut job_counter, &mut console),
                Some(ShellOutcome::Lines(vec![
                    "fg: not supported on this platform".to_string()
                ]))
            );
            assert_eq!(
                execute_shell_adapter("bg", &mut aliases, &jobs, &mut job_counter, &mut console),
                Some(ShellOutcome::Lines(vec![
                    "bg: not supported on this platform".to_string()
                ]))
            );
            assert_eq!(jobs.len(), 1);
        }
    }

    struct VarRestore {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl VarRestore {
        fn new(key: &'static str) -> Self {
            Self {
                key,
                previous: env::var_os(key),
            }
        }
    }

    impl Drop for VarRestore {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    struct CwdGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        dir: PathBuf,
        oldpwd: Option<std::ffi::OsString>,
    }

    impl CwdGuard {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            Self {
                _lock: lock,
                dir: env::current_dir().unwrap(),
                oldpwd: env::var_os("OLDPWD"),
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = env::set_current_dir(&self.dir);
            match &self.oldpwd {
                Some(value) => env::set_var("OLDPWD", value),
                None => env::remove_var("OLDPWD"),
            }
        }
    }

    #[test]
    fn bare_shell_env_adapters_match_legacy_text() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _var = VarRestore::new("AIOS_SHELL_PARITY_KEY");
        env::set_var("AIOS_SHELL_PARITY_KEY", "parity-value");

        let mut aliases = HashMap::new();
        let mut console = HostBackend::new();
        let jobs = JobTable::new();
        let mut job_counter = 0;

        assert_eq!(
            execute_shell_adapter("env", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Lines(legacy_env_lines()))
        );
        assert_eq!(
            execute_shell_adapter(
                "export AIOS_SHELL_PARITY_KEY",
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(legacy_env_value_lines(
                "AIOS_SHELL_PARITY_KEY",
                "parity-value"
            )))
        );
        assert_eq!(
            execute_shell_adapter(
                "export AIOS_SHELL_PARITY_KEY=changed",
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(legacy_export_set_lines(
                "AIOS_SHELL_PARITY_KEY",
                "changed"
            )))
        );
        assert_eq!(env::var("AIOS_SHELL_PARITY_KEY").unwrap(), "changed");
        assert_eq!(
            execute_shell_adapter(
                "export",
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(legacy_env_lines()))
        );
        assert_eq!(
            execute_shell_adapter(
                "unset AIOS_SHELL_PARITY_KEY",
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(Vec::new()))
        );
        assert!(env::var("AIOS_SHELL_PARITY_KEY").is_err());
    }

    #[test]
    fn bare_shell_cd_adapter_matches_legacy_side_effects() {
        let _guard = CwdGuard::new();
        let dir = env::temp_dir().join(format!("aios-shell-cd-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let original = env::current_dir().unwrap();

        let mut aliases = HashMap::new();
        let mut console = HostBackend::new();
        let jobs = JobTable::new();
        let mut job_counter = 0;

        let target = dir.display().to_string();
        assert_eq!(
            execute_shell_adapter(
                &format!("cd {target}"),
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(Vec::new()))
        );
        assert_eq!(env::current_dir().unwrap(), dir);
        assert_eq!(env::var("OLDPWD").unwrap(), original.display().to_string());

        assert_eq!(
            execute_shell_adapter("cd -", &mut aliases, &jobs, &mut job_counter, &mut console),
            Some(ShellOutcome::Lines(Vec::new()))
        );
        assert_eq!(env::current_dir().unwrap(), original);

        let missing = dir.join("definitely-missing");
        let error = env::set_current_dir(&missing).unwrap_err().to_string();
        assert_eq!(
            execute_shell_adapter(
                &format!("cd {}", missing.display()),
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Stderr(legacy_cd_error_lines(
                &missing.display().to_string(),
                &error
            )))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bare_shell_source_adapter_matches_legacy_text_and_side_effects() {
        let _guard = DataDirGuard::new();
        let _var = VarRestore::new("AIOS_SOURCE_TEST");
        env::remove_var("AIOS_SOURCE_TEST");

        let mut aliases = HashMap::new();
        let mut console = HostBackend::new();
        let jobs = JobTable::new();
        let mut job_counter = 0;

        let missing = history_path().unwrap().with_file_name("no-such-shellrc");
        assert_eq!(
            execute_shell_adapter(
                &format!("source {}", missing.display()),
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Stderr(legacy_source_missing_lines(
                &missing.display().to_string()
            )))
        );

        let rc = history_path().unwrap().with_file_name("source-test.rc");
        fs::write(&rc, "# comment\nexport AIOS_SOURCE_TEST=ok\n\nexit\n").unwrap();
        assert_eq!(
            execute_shell_adapter(
                &format!("source {}", rc.display()),
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(Vec::new()))
        );
        assert_eq!(env::var("AIOS_SOURCE_TEST").unwrap(), "ok");

        let recursive = history_path()
            .unwrap()
            .with_file_name("source-recursive.rc");
        fs::write(&recursive, format!("source {}\n", recursive.display())).unwrap();
        assert_eq!(
            execute_shell_adapter(
                &format!("source {}", recursive.display()),
                &mut aliases,
                &jobs,
                &mut job_counter,
                &mut console
            ),
            Some(ShellOutcome::Lines(Vec::new()))
        );
        assert_eq!(
            legacy_source_recursion_lines(&recursive.display().to_string()),
            [format!(
                "source: {}: recursion detected",
                recursive.display()
            )]
        );
    }
}
