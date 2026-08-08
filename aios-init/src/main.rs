use libc::{c_char, c_int, c_ulong};
use std::ffi::CStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MS_NOSUID: c_ulong = 2;
const MS_NODEV: c_ulong = 4;
const MS_NOEXEC: c_ulong = 8;

static TERM_REQUESTED: AtomicBool = AtomicBool::new(false);
static CHILD_EVENT: AtomicBool = AtomicBool::new(false);

fn main() {
    unsafe {
        libc::_exit(run());
    }
}

fn run() -> i32 {
    setup_signals();
    mount_all();
    setup_console();
    log("aios-init: AIOS initramfs init (PID 1)");

    let targets: [(&CStr, &[&CStr]); 2] = [
        (c"/system/aios-core", &[c"/system/aios-core"]),
        (c"/installer", &[c"/installer"]),
    ];

    for (path, args) in targets {
        match run_block(path, args) {
            RunResult::TermRequested => {
                log("aios-init: shutdown requested — entering idle loop");
                idle();
            }
            RunResult::GaveUp(code) => {
                log(&format!(
                    "aios-init: block {} gave up (last exit {code})",
                    path.to_string_lossy()
                ));
            }
        }
    }

    log("aios-init: no AIOS block found — starting emergency shell");
    emergency_shell()
}

enum RunResult {
    TermRequested,
    GaveUp(i32),
}

const MAX_RESTARTS: u32 = 3;

fn run_block(path: &CStr, args: &[&CStr]) -> RunResult {
    let mut restarts = 0;
    loop {
        let Some(pid) = spawn(path, args) else {
            log(&format!(
                "aios-init: failed to fork {} (errno {})",
                path.to_string_lossy(),
                std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
            ));
            return RunResult::GaveUp(-1);
        };
        log(&format!(
            "aios-init: started block pid {pid}: {}",
            path.to_string_lossy()
        ));
        let code = supervise(pid);
        if TERM_REQUESTED.load(Ordering::SeqCst) {
            return RunResult::TermRequested;
        }
        log(&format!("aios-init: block pid {pid} exited (code {code})"));
        if restarts >= MAX_RESTARTS {
            return RunResult::GaveUp(code);
        }
        restarts += 1;
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn supervise(pid: c_int) -> i32 {
    loop {
        if TERM_REQUESTED.load(Ordering::SeqCst) {
            log("aios-init: forwarding shutdown to child");
            shutdown_child(pid);
            return 0;
        }
        if !CHILD_EVENT.swap(false, Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
        loop {
            let mut status: c_int = 0;
            let reaped = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if reaped <= 0 {
                break;
            }
            if reaped == pid {
                return exit_code(status);
            }
            log(&format!("aios-init: reaped child pid {reaped}"));
        }
    }
}

fn shutdown_child(pid: c_int) {
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    for _ in 0..50 {
        if reap(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    loop {
        if reap(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn reap(pid: c_int) -> bool {
    loop {
        let mut status: c_int = 0;
        let reaped = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if reaped <= 0 {
            return false;
        }
        if reaped == pid {
            return true;
        }
    }
}

fn exit_code(status: c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

fn spawn(path: &CStr, args: &[&CStr]) -> Option<c_int> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return None;
    }
    if pid == 0 {
        let env = Env::from_current();
        let envp = env.ptrs();
        let mut argv: Vec<*const c_char> = args.iter().map(|arg| arg.as_ptr()).collect();
        argv.push(std::ptr::null());
        unsafe {
            libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
            libc::_exit(127);
        }
    }
    Some(pid)
}

struct Env {
    items: Vec<std::ffi::CString>,
}

impl Env {
    fn from_current() -> Self {
        let mut items = Vec::new();
        for (key, value) in std::env::vars_os() {
            let mut bytes = key.into_vec();
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_bytes());
            if let Ok(cstring) = std::ffi::CString::new(bytes) {
                items.push(cstring);
            }
        }
        Env { items }
    }

    fn ptrs(&self) -> Vec<*const c_char> {
        let mut ptrs: Vec<*const c_char> = self.items.iter().map(|item| item.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        ptrs
    }
}

fn emergency_shell() -> ! {
    let shells: [(&CStr, &[&CStr]); 3] = [
        (c"/bin/sh", &[c"/bin/sh"]),
        (c"/bin/busybox", &[c"/bin/busybox", c"sh"]),
        (c"/bin/ash", &[c"/bin/ash"]),
    ];
    for (path, args) in shells {
        let mut attempts = 0;
        while attempts < 20 {
            let Some(pid) = spawn(path, args) else {
                break;
            };
            log(&format!(
                "aios-init: emergency shell pid {pid}: {}",
                path.to_string_lossy()
            ));
            let code = supervise(pid);
            if TERM_REQUESTED.load(Ordering::SeqCst) {
                log("aios-init: shutdown requested — entering idle loop");
                idle();
            }
            log(&format!(
                "aios-init: shell pid {pid} exited (code {code}) — respawning"
            ));
            attempts += 1;
            std::thread::sleep(Duration::from_millis(500));
        }
        log(&format!(
            "aios-init: shell {} unavailable",
            path.to_string_lossy()
        ));
    }
    idle()
}

fn idle() -> ! {
    loop {
        reap_any();
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn reap_any() {
    loop {
        let mut status: c_int = 0;
        let reaped = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if reaped <= 0 {
            return;
        }
        log(&format!("aios-init: reaped child pid {reaped}"));
    }
}

fn setup_signals() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_term as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGHUP, &sa, std::ptr::null_mut());

        sa.sa_sigaction = handle_child as *const () as usize;
        sa.sa_flags = libc::SA_RESTART | libc::SA_NOCLDSTOP;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGCHLD, &sa, std::ptr::null_mut());

        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

extern "C" fn handle_term(_signal: c_int) {
    TERM_REQUESTED.store(true, Ordering::SeqCst);
}

extern "C" fn handle_child(_signal: c_int) {
    CHILD_EVENT.store(true, Ordering::SeqCst);
}

fn mount_all() {
    ensure_dir(c"/proc");
    ensure_dir(c"/sys");
    ensure_dir(c"/dev");
    ensure_dir(c"/tmp");

    mount_fs(c"proc", c"/proc", c"proc", MS_NOSUID | MS_NOEXEC | MS_NODEV);
    mount_fs(
        c"sysfs",
        c"/sys",
        c"sysfs",
        MS_NOSUID | MS_NOEXEC | MS_NODEV,
    );
    if !mount_fs(c"devtmpfs", c"/dev", c"devtmpfs", MS_NOSUID) {
        setup_dev_nodes();
    }
    mount_fs(c"tmpfs", c"/tmp", c"tmpfs", MS_NOSUID | MS_NODEV);
}

fn mount_fs(source: &CStr, target: &CStr, fstype: &CStr, flags: c_ulong) -> bool {
    let ok = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags,
            std::ptr::null(),
        ) == 0
    };
    if !ok {
        log(&format!(
            "aios-init: mount {} at {} failed ({})",
            fstype.to_string_lossy(),
            target.to_string_lossy(),
            std::io::Error::last_os_error()
        ));
    }
    ok
}

fn setup_dev_nodes() {
    log("aios-init: devtmpfs unavailable — creating basic device nodes");
    mknod(c"/dev/console", 5, 1, 0o600);
    mknod(c"/dev/null", 1, 3, 0o666);
    mknod(c"/dev/tty", 5, 0, 0o666);
}

fn mknod(path: &CStr, major: u32, minor: u32, mode: libc::mode_t) {
    let device = libc::makedev(major, minor);
    let full_mode = libc::S_IFCHR as libc::mode_t | mode;
    let rc = unsafe { libc::mknod(path.as_ptr(), full_mode, device) };
    if rc != 0 {
        log(&format!(
            "aios-init: mknod {} failed ({})",
            path.to_string_lossy(),
            std::io::Error::last_os_error()
        ));
    }
}

fn ensure_dir(path: &CStr) {
    unsafe {
        libc::mkdir(path.as_ptr(), 0o755);
    }
}

fn setup_console() {
    unsafe {
        let fd = libc::open(c"/dev/console".as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        if fd < 0 {
            log("aios-init: cannot open /dev/console");
            return;
        }
        libc::dup2(fd, libc::STDIN_FILENO);
        libc::dup2(fd, libc::STDOUT_FILENO);
        libc::dup2(fd, libc::STDERR_FILENO);
        if fd > 2 {
            libc::close(fd);
        }
    }
}

fn log(message: &str) {
    let line = format!("[aios-init] {message}\n");
    unsafe {
        libc::write(
            libc::STDERR_FILENO,
            line.as_ptr() as *const libc::c_void,
            line.len(),
        );
    }
}
