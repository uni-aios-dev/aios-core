use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio, exit};

use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

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
    if !map.contains_key("ll") { map.insert("ll".to_string(), "ls -la".to_string()); }
    if !map.contains_key("la") { map.insert("la".to_string(), "ls -la".to_string()); }
    if !map.contains_key("gs") { map.insert("gs".to_string(), "git status".to_string()); }
    if !map.contains_key("gd") { map.insert("gd".to_string(), "git diff".to_string()); }
    if !map.contains_key("gl") { map.insert("gl".to_string(), "git log --oneline -10".to_string()); }
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
    let mut parts = line.splitn(2, char::is_whitespace);
    let first = match parts.next() {
        Some(x) => x,
        None => return line.to_string(),
    };
    if let Some(a) = aliases.get(first) {
        match parts.next() {
            Some(rest) => format!("{} {}", a, rest.trim()),
            None => a.clone(),
        }
    } else {
        line.to_string()
    }
}

fn run_command(line: &str, aliases: &mut HashMap<String, String>, _rl: &mut DefaultEditor) {
    let expanded = expand_aliases(line, aliases);
    let mut parts = expanded.split_whitespace();
    let cmd = match parts.next() {
        Some(c) => c.to_string(),
        None => return,
    };
    match cmd.as_str() {
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
                if let Ok(old) = env::var("OLDPWD") { PathBuf::from(old) } else { return; }
            } else {
                PathBuf::from(target)
            };
            if let Ok(cur) = env::current_dir() { env::set_var("OLDPWD", cur); }
            if let Err(e) = env::set_current_dir(&path) { eprintln!("cd: {}: {}", path.display(), e); }
        }
        "help" | "?" => {
            println!("\x1b[1;36mAIOS Shell - Extended\x1b[0m");
            println!("Built-in: exit,clear,pwd,cd,env,export,unset,history,alias,unalias");
            println!("Files: ls,cat,head,tail,grep,find,wc,cp,mv,rm,mkdir,touch,chmod");
            println!("System: ps,free,df,du,uname,uptime,whoami,id,date,dmesg,kill");
            println!("Net: ping,wget,curl,nc,ss,ip,ifconfig");
            println!("Dev: git,make,cargo,rustc,python,node");
            println!("AIOS: gui,tui,aios,logs,hw,services");
            println!("Tools: which,echo,type,exec,source");
        }
        "env" => {
            let mut envs: Vec<_> = env::vars().collect();
            envs.sort_by(|a,b| a.0.cmp(&b.0));
            for (k,v) in envs { println!("{}={}", k,v); }
        }
        "export" => {
            if let Some(a) = parts.next() {
                if let Some((k,v)) = a.split_once('=') { env::set_var(k,v); }
                else if let Ok(v)=env::var(a) { println!("{}={}", a,v); }
            } else { run_command("env", aliases, _rl); }
        }
        "unset" => { if let Some(k)=parts.next() { env::remove_var(k); } }
        "history" => {
            if let Some(h)=history_path() {
                if let Ok(c)=fs::read_to_string(&h) {
                    for (i,l) in c.lines().enumerate() { if !l.is_empty(){ println!("{}: {}",i+1,l); } }
                }
            }
        }
        "alias" => {
            let rest: Vec<String>=parts.map(|x|x.to_string()).collect();
            if rest.is_empty() {
                for (k,v) in aliases.iter() { println!("alias {}='{}'", k,v); }
            } else {
                for a in rest {
                    if let Some((k,v))=a.split_once('=') {
                        let v=v.trim_matches('\'').trim_matches('"').to_string();
                        aliases.insert(k.to_string(), v);
                    }
                }
                save_aliases(aliases);
            }
        }
        "unalias" => {
            if let Some(k)=parts.next() { aliases.remove(k); save_aliases(aliases); }
        }
        "gui" => { let _ = Command::new("aios-gui").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn(); println!("Starting aios-gui..."); }
        "tui" => { let _ = Command::new("aios").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn(); println!("Starting aios..."); }
        "aios" => {
            let args: Vec<String>=parts.map(|x|x.to_string()).collect();
            let mut c=Command::new("aios"); c.args(&args); let _=c.status();
        }
        "logs" => {
            for f in ["/var/log/syslog","/var/log/messages","/var/log/dmesg","/tmp/xorg.log","/var/log/xorg.log"] {
                if Path::new(f).exists() {
                    println!("\x1b[1;33m--- {} ---\x1b[0m", f);
                    if let Ok(c)=fs::read_to_string(f) {
                        let lines: Vec<&str>=c.lines().collect();
                        let s=if lines.len()>40{lines.len()-40}else{0};
                        for l in &lines[s..]{println!("{}",l);}
                    }
                }
            }
        }
        "hw" | "hardware" => {
            println!("\x1b[1;36m=== Hardware ===\x1b[0m");
            let _=Command::new("uname").args(["-a"]).status();
            println!("\n\x1b[1;36m=== CPU ===\x1b[0m");
            let _=Command::new("cat").args(["/proc/cpuinfo"]).status();
            println!("\n\x1b[1;36m=== Memory ===\x1b[0m");
            let _=Command::new("free").args(["-h"]).status();
            println!("\n\x1b[1;36m=== Disks ===\x1b[0m");
            let _=Command::new("df").args(["-h"]).status();
            println!("\n\x1b[1;36m=== PCI ===\x1b[0m");
            let _=Command::new("lspci").status();
            println!("\n\x1b[1;36m=== USB ===\x1b[0m");
            let _=Command::new("lsusb").status();
        }
        "services" => { let _=Command::new("ps").args(["aux"]).status(); }
        "which" => { if let Some(n)=parts.next(){ let _=Command::new("which").arg(n).status(); } }
        "type" => {
            if let Some(n)=parts.next() {
                if aliases.contains_key(n){ println!("{} is aliased to '{}'",n,aliases.get(n).unwrap()); continue; }
                let builtins=["exit","clear","pwd","cd","help","env","export","unset","history","alias","unalias","gui","tui","aios","logs","dmesg","hw","services","which","exec","source","echo","type","ps","free","df","uname","uptime","whoami","date"];
                if builtins.contains(&n){ println!("{} is a shell builtin",n); continue; }
                let _=Command::new("which").arg(n).status();
            }
        }
        "exec" => {
            let rest: Vec<String>=parts.map(|x|x.to_string()).collect();
            if rest.is_empty(){eprintln!("exec: missing");return;}
            let mut c=Command::new(&rest[0]); if rest.len()>1{c.args(&rest[1..]);} let _=c.status();
        }
        "source" | "." => {
            if let Some(f)=parts.next(){
                if let Ok(content)=fs::read_to_string(f){
                    for line in content.lines(){
                        let l=line.trim();
                        if l.is_empty()||l.starts_with('#'){continue;}
                        run_command(l,aliases,_rl);
                    }
                }
            }
        }
        "echo" => {
            let rest: Vec<String>=parts.map(|x|{
                if x.starts_with('$'){
                    let v=&x[1..];
                    if let Some((k,d))=v.split_once('-'){ env::var(k).unwrap_or_else(|_|d.to_string()) }
                    else if let Some((k,d))=v.split_once(':'){ env::var(k).unwrap_or_else(|_|d.to_string()) }
                    else { env::var(v).unwrap_or_default() }
                }else{x.to_string()}
            }).collect();
            println!("{}", rest.join(" "));
        }
        "ps"|"free"|"df"|"du"|"uname"|"uptime"|"whoami"|"id"|"date"|"ls"|"cat"|"head"|"tail"|"grep"|"find"|"wc"|"cp"|"mv"|"rm"|"mkdir"|"touch"|"chmod"|"chown"|"ln"|"tar"|"gzip"|"gunzip"|"zip"|"unzip"|"curl"|"wget"|"ping"|"ip"|"ss"|"netstat"|"kill"|"killall"|"top"|"htop"|"lsof"|"strace"|"file"|"hexdump"|"xxd"|"md5sum"|"sha256sum"|"git"|"make"|"cargo"|"rustc"|"gcc"|"g++"|"python"|"python3"|"node"|"npm"|"vi"|"vim"|"nano"|"less"|"more" => {
            let mut c=Command::new(&cmd);
            let args: Vec<String>=parts.map(|x|x.to_string()).collect();
            c.args(&args);
            c.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
            let _=c.status();
        }
        _ => {
            // try direct then sh -c
            let mut p2 = expanded.split_whitespace();
            if let Some(first)=p2.next(){
                let args: Vec<String>=p2.map(|x|x.to_string()).collect();
                let mut c=Command::new(first);
                c.args(&args);
                c.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
                if c.status().is_err(){
                    let mut c2=Command::new("sh");
                    c2.args(["-c",&expanded]);
                    c2.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
                    let _=c2.status();
                }
                return;
            }
        }
    }
}

fn load_shellrc() {
    if let Some(p)=config_path() {
        if let Ok(content)=fs::read_to_string(p) {
            let mut aliases=load_aliases();
            let mut rl=DefaultEditor::new().unwrap();
            for line in content.lines() {
                let l=line.trim();
                if l.is_empty()||l.starts_with('#'){continue;}
                if l.starts_with("alias ") {
                    let rest=l.strip_prefix("alias ").unwrap_or("");
                    if let Some((k,v))=rest.split_once('=') {
                        aliases.insert(k.trim().to_string(), v.trim().trim_matches('\'').trim_matches('"').to_string());
                    }
                } else {
                    run_command(l,&mut aliases,&mut rl);
                }
            }
            save_aliases(&aliases);
        }
    }
}

fn main() {
    load_shellrc();
    println!("\x1b[1;36m╔═══════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[1;36m║           AIOS Shell v2.1                 ║\x1b[0m");
    println!("\x1b[1;36m║      Extended CLI with Full Tooling       ║\x1b[0m");
    println!("\x1b[1;36m╚═══════════════════════════════════════════╝\x1b[0m");
    println!("\x1b[2mhelp, gui, tui, hw, logs, ps, net, dev tools\x1b[0m\n");
    let mut rl = DefaultEditor::new().expect("rl");
    if let Some(h)=history_path(){ let _=rl.load_history(&h); }
    let mut aliases = load_aliases();
    loop {
        let pwd = env::current_dir().unwrap_or_else(|_|PathBuf::from("/"));
        let user = env::var("USER").unwrap_or_else(|_|env::var("LOGNAME").unwrap_or_else(|_|"root".to_string()));
        let host = env::var("HOSTNAME").unwrap_or_else(|_|"aios".to_string());
        let prompt = format!("\x1b[1;32m{}@{}\x1b[0m:\x1b[1;34m{}\x1b[0m$ ", user, host, pwd.display());
        match rl.readline(&prompt) {
            Ok(line) => {
                let line=line.trim();
                if line.is_empty(){continue;}
                let _=rl.add_history_entry(line);
                if let Some(h)=history_path(){ let _=rl.save_history(&h); }
                for cmd in line.split([';','&']){
                    let cmd=cmd.trim();
                    if cmd.is_empty()||cmd=="&"{continue;}
                    run_command(cmd,&mut aliases,&mut rl);
                }
            }
            Err(ReadlineError::Interrupted)=>{println!("^C");continue;}
            Err(ReadlineError::Eof)=>break;
            Err(e)=>{eprintln!("err: {:?}",e);break;}
        }
    }
    println!("Goodbye!");
}
