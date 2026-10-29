use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio, exit};

use rustyline::completion::FilenameCompleter;
use rustyline::error::ReadlineError;
use rustyline::history::FileHistory;
use rustyline::{Config, EditMode, Editor};

#[derive(Clone)]
struct Job {
    id: usize,
    cmd: String,
    pid: u32,
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
    if !map.contains_key("ll") { map.insert("ll".to_string(), "ls -la".to_string()); }
    if !map.contains_key("la") { map.insert("la".to_string(), "ls -la".to_string()); }
    if !map.contains_key("l") { map.insert("l".to_string(), "ls -CF".to_string()); }
    if !map.contains_key("gs") { map.insert("gs".to_string(), "git status".to_string()); }
    if !map.contains_key("gd") { map.insert("gd".to_string(), "git diff".to_string()); }
    if !map.contains_key("gl") { map.insert("gl".to_string(), "git log --oneline -10".to_string()); }
    if !map.contains_key("grep") { map.insert("grep".to_string(), "grep --color=auto".to_string()); }
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

fn is_gui_app(name: &str) -> bool {
    matches!(name, "aios-gui"|"firefox"|"chromium"|"xterm"|"rxvt"|"aterm"|"mlterm"|"gimp"|"thunar"|"pcmanfm"|"nautilus"|"geany"|"mousepad"|"leafpad")
}

fn run_command(line: &str, aliases: &mut HashMap<String, String>, jobs: &mut Vec<Job>, job_counter: &mut usize) {
    let expanded = expand_aliases(line, aliases);
    let mut parts = expanded.split_whitespace();
    let cmd = match parts.next() {
        Some(c) => c.to_string(),
        None => return,
    };
    
    // Background execution
    let bg = expanded.ends_with(" &") || cmd.ends_with("&");
    let cmd_clean = if bg { cmd.trim_end_matches('&').to_string() } else { cmd.clone() };
    
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
                if let Ok(old) = env::var("OLDPWD") { PathBuf::from(old) } else { return; }
            } else {
                PathBuf::from(target)
            };
            if let Ok(cur) = env::current_dir() { env::set_var("OLDPWD", cur); }
            if let Err(e) = env::set_current_dir(&path) { eprintln!("cd: {}: {}", path.display(), e); }
        }
        "help" | "?" => {
            println!("\x1b[1;36m╔════════════════════════════════════════════════════════════╗\x1b[0m");
            println!("\x1b[1;36m║                    AIOS Shell v2.2                        ║\x1b[0m");
            println!("\x1b[1;36m╚════════════════════════════════════════════════════════════╝\x1b[0m");
            println!("\n\x1b[1;33m[BUILTIN]\x1b[0m exit,quit,clear,pwd,cd,env,export,unset,history,alias,unalias,source,type,echo,exec,jobs,fg,bg");
            println!("\x1b[1;33m[FILES]\x1b[0m ls,cat,head,tail,less,grep,find,wc,cp,mv,rm,mkdir,touch,chmod,chown,ln,stat,file");
            println!("\x1b[1;33m[SYSTEM]\x1b[0m ps,top,htop,free,df,du,uname,uptime,whoami,id,date,dmesg,kernel,lsmod,modinfo");
            println!("\x1b[1;33m[NET]\x1b[0m ping,curl,wget,nc,ss,netstat,ip,ifconfig,route,arp,nslookup,dig,traceroute");
            println!("\x1b[1;33m[DEBUG]\x1b[0m strace,lsof,lscpu,lsblk,lsusb,lspci,dmidecode,hdparm,iostat,vmstat");
            println!("\x1b[1;33m[DEV]\x1b[0m git,make,cmake,gcc,g++,rustc,cargo,python,python3,node,npm,pip");
            println!("\x1b[1;33m[ARCHIVE]\x1b[0m tar,gzip,gunzip,bzip2,xz,zip,unzip");
            println!("\x1b[1;33m[AIOS]\x1b[0m gui,tui,aios,logs,hw,services,aios-shell-reload");
            println!("\x1b[1;33m[EDITORS]\x1b[0m vi,vim,nano,emacs,micro");
            println!("\n\x1b[2mRun any program directly. Use & for background. TAB for completion.\x1b[0m");
        }
        "env" => {
            let mut envs: Vec<_> = env::vars().collect();
            envs.sort_by(|a,b| a.0.cmp(&b.0));
            for (k,v) in envs { println!("{}={}", k,v); }
        }
        "export" => {
            if let Some(a) = parts.next() {
                if let Some((k,v)) = a.split_once('=') { env::set_var(k,v); println!("export {}={}",k,v); }
                else if let Ok(v)=env::var(a) { println!("{}={}", a,v); }
            } else { run_command("env", aliases, jobs, job_counter); }
        }
        "unset" => { if let Some(k)=parts.next() { env::remove_var(k); } }
        "history" => {
            if let Some(h)=history_path() {
                if let Ok(c)=fs::read_to_string(&h) {
                    for (i,l) in c.lines().enumerate() { if !l.is_empty(){ println!("{:5}: {}",i+1,l); } }
                }
            }
        }
        "alias" => {
            let rest: Vec<String>=parts.map(|x|x.to_string()).collect();
            if rest.is_empty() {
                let mut keys: Vec<_>=aliases.keys().collect(); keys.sort();
                for k in keys { println!("alias {}='{}'", k, aliases.get(k).unwrap()); }
            } else {
                for a in rest {
                    if let Some((k,v))=a.split_once('=') {
                        let v=v.trim_matches('\'').trim_matches('"').to_string();
                        aliases.insert(k.to_string(), v);
                        println!("alias {}='{}'", k, aliases.get(k).unwrap());
                    }
                }
                save_aliases(aliases);
            }
        }
        "unalias" => {
            if let Some(k)=parts.next() { aliases.remove(k); save_aliases(aliases); }
        }
        "jobs" => {
            if jobs.is_empty() { println!("No jobs"); } else {
                for j in jobs.iter() { println!("[{}]+ Running        {}", j.id, j.cmd); }
            }
        }
        "fg" | "bg" => { println!("{}: job control not fully implemented", cmd_clean); }
        "gui" => { 
            if let Ok(child) = Command::new("aios-gui").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).env("DISPLAY", env::var("DISPLAY").unwrap_or_else(|_|":0".to_string())).spawn() {
                println!("Started aios-gui [PID {}]", child.id());
                if bg { *job_counter+=1; jobs.push(Job{id:*job_counter, cmd:"aios-gui".to_string(), pid:child.id()}); }
            }
        }
        "tui" => { 
            if let Ok(_child) = Command::new("aios").stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).spawn() {
                println!("Starting aios...");
            }
        }
        "aios" => {
            let args: Vec<String>=parts.map(|x|x.to_string()).collect();
            let mut c=Command::new("aios"); c.args(&args).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
            let _=c.status();
        }
        "aios-shell-reload" => { println!("Reloading shellrc..."); load_shellrc_simple(aliases); }
        "logs" => {
            for f in ["/var/log/syslog","/var/log/messages","/var/log/dmesg","/tmp/xorg.log","/var/log/xorg.log","/var/log/auth.log"] {
                if Path::new(f).exists() {
                    println!("\x1b[1;33m═══ {} ═══\x1b[0m", f);
                    if let Ok(mut file)=File::open(f) {
                        let mut content=String::new();
                        let _=file.read_to_string(&mut content);
                        let lines: Vec<&str>=content.lines().collect();
                        let s=if lines.len()>60{lines.len()-60}else{0};
                        for l in &lines[s..]{println!("{}",l);}
                    }
                    println!();
                }
            }
        }
        "hw" | "hardware" | "info" => {
            println!("\x1b[1;36m═══ System Information ═══\x1b[0m");
            let _=Command::new("uname").args(["-a"]).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
            println!("\n\x1b[1;36m═══ CPU ═══\x1b[0m");
            let _=Command::new("sh").args(["-c","cat /proc/cpuinfo | grep -E 'model name|processor|cores' | head -20"]).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
            println!("\n\x1b[1;36m═══ Memory ═══\x1b[0m");
            let _=Command::new("free").args(["-h"]).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
            println!("\n\x1b[1;36m═══ Disks ═══\x1b[0m");
            let _=Command::new("lsblk").args(["-o","NAME,SIZE,TYPE,MOUNTPOINT"]).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
            println!("\n\x1b[1;36m═══ PCI Devices ═══\x1b[0m");
            let _=Command::new("lspci").args(["-nnk"]).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
            println!("\n\x1b[1;36m═══ USB Devices ═══\x1b[0m");
            let _=Command::new("lsusb").stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
        }
        "services" => {
            println!("\x1b[1;36m═══ Running Processes ═══\x1b[0m");
            let _=Command::new("ps").args(["auxf"]).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
        }
        "kernel" => {
            println!("\x1b[1;36m═══ Kernel Info ═══\x1b[0m");
            let _=Command::new("uname").args(["-r","-v"]).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
            println!("\n\x1b[1;36m═══ Loaded Modules ═══\x1b[0m");
            let _=Command::new("lsmod").stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
        }
        "which" => { if let Some(n)=parts.next(){ let _=Command::new("which").arg(n).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status(); } }
        "type" => {
            if let Some(n)=parts.next() {
                if aliases.contains_key(n){ println!("{} is aliased to '{}'",n,aliases.get(n).unwrap()); }
                else {
                    let builtins=["exit","clear","pwd","cd","help","env","export","unset","history","alias","unalias","gui","tui","aios","logs","hw","services","kernel","which","exec","source","echo","type","ps","free","df","uname","uptime","whoami","date","ls","cat","grep","find","jobs","fg","bg"];
                    if builtins.contains(&n){ println!("{} is a shell builtin",n); }
                    else { let _=Command::new("which").arg(n).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status(); }
                }
            }
        }
        "exec" => {
            let rest: Vec<String>=parts.map(|x|x.to_string()).collect();
            if rest.is_empty(){eprintln!("exec: missing");return;}
            let mut c=Command::new(&rest[0]); if rest.len()>1{c.args(&rest[1..]);}
            c.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
            let _=c.status();
        }
        "source" | "." => {
            if let Some(f)=parts.next(){
                if let Ok(content)=fs::read_to_string(f){
                    for line in content.lines(){
                        let l=line.trim();
                        if l.is_empty()||l.starts_with('#'){continue;}
                        run_command(l,aliases,jobs,job_counter);
                    }
                } else { eprintln!("source: {}: No such file", f); }
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
        "ps"|"free"|"df"|"du"|"uname"|"uptime"|"whoami"|"id"|"date"|"ls"|"cat"|"head"|"tail"|"grep"|"find"|"wc"|"cp"|"mv"|"rm"|"mkdir"|"touch"|"chmod"|"chown"|"ln"|"stat"|"file"|"tar"|"gzip"|"gunzip"|"bzip2"|"xz"|"zip"|"unzip"|"curl"|"wget"|"ping"|"ip"|"ss"|"netstat"|"route"|"arp"|"nslookup"|"dig"|"traceroute"|"nc"|"kill"|"killall"|"top"|"htop"|"lsof"|"strace"|"lscpu"|"lsblk"|"lsusb"|"lspci"|"dmidecode"|"hdparm"|"iostat"|"vmstat"|"dmesg"|"git"|"make"|"cmake"|"gcc"|"g++"|"rustc"|"cargo"|"python"|"python3"|"node"|"npm"|"pip"|"pip3"|"vi"|"vim"|"nano"|"emacs"|"micro"|"less"|"more"|"hexdump"|"xxd"|"md5sum"|"sha256sum"|"mount"|"umount"|"modprobe"|"insmod"|"rmmod"|"lsmod" => {
            let mut c=Command::new(&cmd_clean);
            let args: Vec<String>=parts.map(|x|x.to_string()).collect();
            c.args(&args);
            c.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
            if bg {
                if let Ok(child)=c.spawn() {
                    *job_counter+=1;
                    jobs.push(Job{id:*job_counter, cmd:expanded.trim_end_matches(" &").to_string(), pid:child.id()});
                    println!("[{}] {} {}", *job_counter, child.id(), expanded.trim_end_matches(" &"));
                }
            } else {
                let _=c.status();
            }
        }
        _ => {
            // Try to run as GUI app in X if in X session
            if env::var("DISPLAY").is_ok() && is_gui_app(&cmd_clean) {
                let mut c=Command::new(&cmd_clean);
                let args: Vec<String>=parts.map(|x|x.to_string()).collect();
                c.args(&args);
                c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                if let Ok(child)=c.spawn() {
                    println!("Launched {} [PID {}]", cmd_clean, child.id());
                    if bg { *job_counter+=1; jobs.push(Job{id:*job_counter, cmd:expanded.clone(), pid:child.id()}); }
                }
                return;
            }
            // Try direct exec
            let mut p2 = expanded.split_whitespace();
            if let Some(first)=p2.next(){
                let first_clean = first.trim_end_matches('&');
                let args: Vec<String>=p2.map(|x|x.to_string()).filter(|x|x!="&").collect();
                let mut c=Command::new(first_clean);
                c.args(&args);
                c.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
                if bg {
                    if let Ok(child)=c.spawn() {
                        *job_counter+=1;
                        jobs.push(Job{id:*job_counter, cmd:expanded.trim_end_matches(" &").to_string(), pid:child.id()});
                        println!("[{}] {} {}", *job_counter, child.id(), expanded.trim_end_matches(" &"));
                    }
                    return;
                }
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

fn load_shellrc_simple(aliases: &mut HashMap<String, String>) {
    if let Some(p)=config_path() {
        if let Ok(content)=fs::read_to_string(p) {
            let mut jobs=vec![]; let mut jc=0;
            for line in content.lines() {
                let l=line.trim();
                if l.is_empty()||l.starts_with('#'){continue;}
                if l.starts_with("alias ") {
                    let rest=l.strip_prefix("alias ").unwrap_or("");
                    if let Some((k,v))=rest.split_once('=') {
                        aliases.insert(k.trim().to_string(), v.trim().trim_matches('\'').trim_matches('"').to_string());
                    }
                } else {
                    run_command(l,aliases,&mut jobs,&mut jc);
                }
            }
            save_aliases(aliases);
        }
    }
}

fn main() {
    let mut aliases = load_aliases();
    load_shellrc_simple(&mut aliases);
    
    println!("\x1b[1;36m╔═══════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[1;36m║                AIOS Shell v2.2                    ║\x1b[0m");
    println!("\x1b[1;36m║        Full-Featured Command Interface            ║\x1b[0m");
    println!("\x1b[1;36m╚═══════════════════════════════════════════════════╝\x1b[0m");
    println!("\x1b[2mType 'help' for all commands | TAB completion enabled\x1b[0m\n");
    
    let config = Config::builder()
        .history_ignore_space(true)
        .completion_type(rustyline::CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .build();
    
    let mut rl: Editor<(), FileHistory> = Editor::with_config(config).expect("Failed to init readline");
    if let Some(h)=history_path(){ let _=rl.load_history(&h); }

    
    let mut jobs: Vec<Job> = vec![];
    let mut job_counter: usize = 0;
    
    loop {
        let pwd = env::current_dir().unwrap_or_else(|_|PathBuf::from("/"));
        let user = env::var("USER").unwrap_or_else(|_|env::var("LOGNAME").unwrap_or_else(|_|"root".to_string()));
        let host = env::var("HOSTNAME").unwrap_or_else(|_|"aios".to_string());
        let short_pwd = if pwd.to_string_lossy() == "/" { "/".to_string() } else {
            pwd.file_name().and_then(|n|n.to_str()).unwrap_or("/").to_string()
        };
        let prompt = format!("\x1b[1;32m{}@{}\x1b[0m:\x1b[1;34m{}\x1b[0m$ ", user, host, if pwd.to_string_lossy().len()>20 { format!(".../{}", short_pwd) } else { pwd.display().to_string() });
        match rl.readline(&prompt) {
            Ok(line) => {
                let line=line.trim();
                if line.is_empty(){continue;}
                let _=rl.add_history_entry(line);
                if let Some(h)=history_path(){ let _=rl.save_history(&h); }
                for cmd in line.split([';']){
                    let cmd=cmd.trim();
                    if cmd.is_empty(){continue;}
                    // Handle && 
                    if cmd.contains("&&") {
                        for sub in cmd.split("&&") {
                            let sub=sub.trim();
                            if sub.is_empty(){continue;}
                            run_command(sub,&mut aliases,&mut jobs,&mut job_counter);
                        }
                    } else {
                        run_command(cmd,&mut aliases,&mut jobs,&mut job_counter);
                    }
                }
            }
            Err(ReadlineError::Interrupted)=>{println!("^C");}
            Err(ReadlineError::Eof)=>{println!("logout"); break;}
            Err(e)=>{eprintln!("Error: {:?}",e);break;}
        }
    }
}
