//! OS-native push-based device notifications for hot-plug.
//!
//! The polling and cheap-signal paths in [`crate::hotplug::HotplugMonitor`] still
//! need a timer tick to notice a device change. This module adds the push
//! alternative: the kernel tells AIOS *immediately* when the device tree moves,
//! so a full `HardwareProfile::detect()` + fingerprint diff only ever runs when
//! it is guaranteed to be useful.
//!
//! - **Windows**: a hidden message-only window is registered with
//!   `RegisterDeviceNotificationW` (all device-interface classes). `WM_DEVICECHANGE`
//!   with `DBT_DEVICEARRIVAL` / `DBT_DEVICEREMOVECOMPLETE` wakes a dedicated
//!   message-pump thread, which forwards a coarse [`NativeEvent`] to the monitor.
//! - **Linux**: a `NETLINK_KOBJECT_UEVENT` socket subscribes to the kernel's
//!   udev event multicast group. `add`/`remove`/`bind`/`unbind` uevents are
//!   parsed and forwarded. A non-blocking socket plus a stop flag allows clean
//!   shutdown (no leak of the read thread).
//! - **Other platforms**: no native source, [`NativeHotplugMonitor::start`]
//!   degrades to an inert monitor and the caller keeps its poll cadence.
//!
//! The native layer deliberately does **not** build fingerprints itself — the
//! authoritative device set comes from `HardwareProfile::detect()` through
//! [`crate::fingerprint::extract_fingerprints`]. It only triggers that detection
//! promptly. Raw device-interface paths / uevent subsystem names are reduced to
//! a coarse [`BusHint`] so the monitor can ignore unrelated churn (e.g. display
//! interfaces, audio, HID consumer events) without paying for a full scan.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Coarse bus classification derived from a native event, used to filter out
/// churn that is irrelevant to AIOS fingerprinting (which covers USB, PCI, NVMe
/// and the block/storage layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusHint {
    /// USB device (or `usb#vid_…` interface path).
    Usb,
    /// PCI device (`pci#ven_…&dev_…`).
    Pci,
    /// NVMe controller/namespace.
    Nvme,
    /// Block/storage (IDE, SCSI, SATA, MMC…).
    Storage,
    /// Anything else — not fingerprinted by AIOS.
    Other,
}

/// A single push notification from the OS device manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeEvent {
    /// Device arrival (`true`) or removal (`false`).
    pub added: bool,
    /// Coarse bus classification (see [`BusHint`]).
    pub bus: BusHint,
}

impl NativeEvent {
    /// Whether the event concerns a bus AIOS actually fingerprints.
    pub fn is_relevant(&self) -> bool {
        self.bus != BusHint::Other
    }
}

/// Background listener for OS-native device notifications. Dropping the
/// monitor stops the listener thread. When no native source is available the
/// monitor is inert and [`NativeHotplugMonitor::try_recv`] always returns
/// `None`, so callers transparently fall back to their poll cadence.
pub struct NativeHotplugMonitor {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    rx: Option<Receiver<NativeEvent>>,
    hwnd: Arc<AtomicUsize>,
}

impl NativeHotplugMonitor {
    /// Start the native listener (best effort). Never fails: if the OS source
    /// cannot be set up the returned monitor simply produces no events.
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let hwnd = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel();
        let handle = spawn_native(Arc::clone(&stop), Arc::clone(&hwnd), tx);
        let rx = handle.as_ref().map(|_| rx);
        Self {
            stop,
            handle,
            rx,
            hwnd,
        }
    }

    /// Poll for a pending native event without blocking.
    pub fn try_recv(&self) -> Option<NativeEvent> {
        self.rx.as_ref()?.try_recv().ok()
    }

    /// Whether the native listener actually set up an OS source. On Windows
    /// this becomes `true` once the hidden message-only window exists and is
    /// registered for device notifications; on Linux it is `true` while the
    /// netlink listener thread is running (socket setup races inside it).
    pub fn is_active(&self) -> bool {
        #[cfg(windows)]
        {
            self.hwnd.load(Ordering::Relaxed) != 0
        }
        #[cfg(not(windows))]
        {
            self.handle.is_some()
        }
    }
}

impl Drop for NativeHotplugMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        #[cfg(windows)]
        {
            // The pump thread stores the HWND right after creating the window.
            // Give a still-starting pump a moment to publish it, then ask it to
            // close; a pump that never started exits on its own (setup failed).
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.hwnd.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let h = self.hwnd.load(Ordering::Relaxed);
            if h != 0 {
                pnp::request_close(h);
            }
        }
        if let Some(handle) = self.handle.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(windows)]
fn spawn_native(
    stop: Arc<AtomicBool>,
    hwnd: Arc<AtomicUsize>,
    tx: Sender<NativeEvent>,
) -> Option<JoinHandle<()>> {
    Some(thread::spawn(move || {
        let _ = pnp::run(stop, &hwnd, tx);
    }))
}

#[cfg(target_os = "linux")]
fn spawn_native(
    stop: Arc<AtomicBool>,
    _hwnd: Arc<AtomicUsize>,
    tx: Sender<NativeEvent>,
) -> Option<JoinHandle<()>> {
    udev::spawn(stop, tx)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn spawn_native(
    _stop: Arc<AtomicBool>,
    _hwnd: Arc<AtomicUsize>,
    _tx: Sender<NativeEvent>,
) -> Option<JoinHandle<()>> {
    None
}

/// Windows implementation: hidden message-only window + `RegisterDeviceNotificationW`.
#[cfg(windows)]
mod pnp {
    #![allow(clippy::upper_case_acronyms)]
    use super::*;
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    type HWND = *mut c_void;
    type WPARAM = usize;
    type LPARAM = isize;
    type LRESULT = isize;
    type BOOL = i32;

    const WM_DEVICECHANGE: u32 = 0x0219;
    const WM_DESTROY: u32 = 0x0002;
    const WM_CLOSE: u32 = 0x0010;
    const DBT_DEVICEARRIVAL: usize = 0x8000;
    const DBT_DEVICEREMOVECOMPLETE: usize = 0x8004;
    const DBT_DEVTYP_DEVICEINTERFACE: u32 = 0x0005;
    const DEVICE_NOTIFY_ALL_INTERFACE_CLASSES: u32 = 0x0004;
    const HWND_MESSAGE: isize = -3;

    type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

    #[repr(C)]
    struct WndClassW {
        style: u32,
        lpfn_wnd_proc: Option<WndProc>,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: *mut c_void,
        h_icon: *mut c_void,
        h_cursor: *mut c_void,
        hbr_background: *mut c_void,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
    }

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct Msg {
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        time: u32,
        pt: Point,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    /// `DEV_BROADCAST_DEVICEINTERFACE_W` (first three fields equal
    /// `DEV_BROADCAST_HDR`).
    #[repr(C)]
    struct DevBroadcastDevInterfaceW {
        size: u32,
        devicetype: u32,
        reserved: u32,
        class_guid: Guid,
        name: [u16; 1],
    }

    unsafe extern "system" {
        fn RegisterClassW(lp_wnd_class: *const WndClassW) -> u16;
        fn CreateWindowExW(
            dw_ex_style: u32,
            lp_class_name: *const u16,
            lp_window_name: *const u16,
            dw_style: u32,
            x: i32,
            y: i32,
            n_width: i32,
            n_height: i32,
            h_wnd_parent: HWND,
            h_menu: *mut c_void,
            h_instance: *mut c_void,
            lp_param: *mut c_void,
        ) -> HWND;
        fn DefWindowProcW(h_wnd: HWND, msg: u32, w_param: WPARAM, l_param: LPARAM) -> LRESULT;
        fn GetMessageW(lp_msg: *mut Msg, h_wnd: HWND, w_min: u32, w_max: u32) -> BOOL;
        fn TranslateMessage(lp_msg: *const Msg) -> BOOL;
        fn DispatchMessageW(lp_msg: *const Msg) -> LRESULT;
        fn DestroyWindow(h_wnd: HWND) -> BOOL;
        fn PostMessageW(h_wnd: HWND, msg: u32, w_param: WPARAM, l_param: LPARAM) -> BOOL;
        fn PostQuitMessage(n_exit_code: i32);
        fn GetModuleHandleW(lp_module_name: *const u16) -> *mut c_void;
        fn RegisterDeviceNotificationW(
            h_recipient: *mut c_void,
            filter: *const c_void,
            flags: u32,
        ) -> *mut c_void;
        fn UnregisterDeviceNotification(h: *mut c_void) -> BOOL;
    }

    // The message-pump thread's sender; the window procedure runs on that
    // thread, so thread-local storage is safe and avoids a user-data pointer.
    thread_local! {
        static TX: RefCell<Option<Sender<NativeEvent>>> = const { RefCell::new(None) };
    }

    static NEXT_CLASS_ID: AtomicUsize = AtomicUsize::new(0);

    fn to_wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    unsafe fn read_wide(mut p: *const u16) -> String {
        let mut v = Vec::new();
        for _ in 0..512 {
            let c = *p;
            if c == 0 {
                break;
            }
            v.push(c);
            p = p.add(1);
        }
        String::from_utf16_lossy(&v)
    }

    pub(super) fn classify_interface(name: &str) -> BusHint {
        let n = name.to_ascii_lowercase();
        if n.contains("vid_") || n.contains("usb#") || n.contains("usbmi") {
            BusHint::Usb
        } else if n.contains("ven_") || n.contains("pci#") || n.contains("pciide") {
            BusHint::Pci
        } else if n.contains("nvme") {
            BusHint::Nvme
        } else if n.contains("ide#")
            || n.contains("scsi#")
            || n.contains("disk")
            || n.contains("stor")
        {
            BusHint::Storage
        } else {
            BusHint::Other
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_DEVICECHANGE
                if wparam == DBT_DEVICEARRIVAL || wparam == DBT_DEVICEREMOVECOMPLETE =>
            {
                let di = lparam as *const DevBroadcastDevInterfaceW;
                if !di.is_null() && (*di).devicetype == DBT_DEVTYP_DEVICEINTERFACE {
                    let name = read_wide((*di).name.as_ptr());
                    let bus = classify_interface(&name);
                    let added = wparam == DBT_DEVICEARRIVAL;
                    TX.with(|tx| {
                        if let Some(tx) = tx.borrow().as_ref() {
                            let _ = tx.send(NativeEvent { added, bus });
                        }
                    });
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Create the hidden window, subscribe to device notifications and pump
    /// messages until the monitor is dropped. Publishes the `HWND` into
    /// `hwnd_out` right after the window exists (before the message loop) so
    /// [`NativeHotplugMonitor::drop`] can post `WM_CLOSE`.
    pub(super) fn run(
        stop: Arc<AtomicBool>,
        hwnd_out: &Arc<AtomicUsize>,
        tx: Sender<NativeEvent>,
    ) -> Option<usize> {
        unsafe {
            TX.with(|t| *t.borrow_mut() = Some(tx));
            let id = NEXT_CLASS_ID.fetch_add(1, Ordering::Relaxed);
            let class_name = to_wide(&format!(
                "AIOS.Hotplug.Window.{}.{}",
                std::process::id(),
                id
            ));
            let hinst = GetModuleHandleW(std::ptr::null());
            let wc = WndClassW {
                style: 0,
                lpfn_wnd_proc: Some(wnd_proc),
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                h_instance: hinst,
                h_icon: std::ptr::null_mut(),
                h_cursor: std::ptr::null_mut(),
                hbr_background: std::ptr::null_mut(),
                lpsz_menu_name: std::ptr::null(),
                lpsz_class_name: class_name.as_ptr(),
            };
            let _ = RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE as HWND,
                std::ptr::null_mut(),
                hinst,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                return None;
            }
            if stop.load(Ordering::Relaxed) {
                let _ = DestroyWindow(hwnd);
                return None;
            }
            let mut filter: DevBroadcastDevInterfaceW = std::mem::zeroed();
            filter.size = std::mem::size_of::<DevBroadcastDevInterfaceW>() as u32;
            filter.devicetype = DBT_DEVTYP_DEVICEINTERFACE;
            let hdev = RegisterDeviceNotificationW(
                hwnd,
                &filter as *const DevBroadcastDevInterfaceW as *const c_void,
                DEVICE_NOTIFY_ALL_INTERFACE_CLASSES,
            );
            if hdev.is_null() {
                let _ = DestroyWindow(hwnd);
                return None;
            }
            hwnd_out.store(hwnd as usize, Ordering::Relaxed);
            let mut msg: Msg = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if stop.load(Ordering::Relaxed) {
                    let _ = PostMessageW(hwnd, WM_CLOSE, 0, 0);
                    continue;
                }
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
            let _ = UnregisterDeviceNotification(hdev);
            Some(hwnd as usize)
        }
    }

    pub(super) fn request_close(hwnd: usize) {
        unsafe {
            let _ = PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0);
        }
    }
}

/// Linux implementation: `NETLINK_KOBJECT_UEVENT` udev listener.
#[cfg(target_os = "linux")]
mod udev {
    use super::*;

    /// Spawn the udev listener thread. Socket setup happens inside the thread;
    /// if it fails the thread exits quietly and the caller keeps its poll
    /// cadence as the fallback.
    pub(super) fn spawn(stop: Arc<AtomicBool>, tx: Sender<NativeEvent>) -> Option<JoinHandle<()>> {
        Some(thread::spawn(move || run(stop, tx)))
    }

    fn run(stop: Arc<AtomicBool>, tx: Sender<NativeEvent>) {
        unsafe {
            let fd = libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                libc::NETLINK_KOBJECT_UEVENT,
            );
            if fd < 0 {
                return;
            }
            let mut addr: libc::sockaddr_nl = std::mem::zeroed();
            addr.nl_family = libc::AF_NETLINK as u16;
            addr.nl_groups = 1;
            let bound = libc::bind(
                fd,
                &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            );
            if bound < 0 {
                let _ = libc::close(fd);
                return;
            }
            let mut buf = [0u8; 8192];
            while !stop.load(Ordering::Relaxed) {
                let n = libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0);
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::Interrupted
                    {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                    let _ = libc::close(fd);
                    return;
                }
                if n > 0 {
                    if let Some(ev) = parse_uevent(&buf[..n as usize]) {
                        if ev.is_relevant() && tx.send(ev).is_err() {
                            let _ = libc::close(fd);
                            return;
                        }
                    }
                }
            }
            let _ = libc::close(fd);
        }
    }

    /// Parse a `"ACTION@/dev/path\0KEY=VALUE\0…"` uevent buffer.
    fn parse_uevent(buf: &[u8]) -> Option<NativeEvent> {
        let text = std::str::from_utf8(buf).ok()?;
        let action = text.split('@').next().unwrap_or("");
        let added = matches!(action, "add" | "bind" | "change");
        let removed = matches!(action, "remove" | "unbind");
        if !added && !removed {
            return None;
        }
        let subsystem = text
            .split('\0')
            .find_map(|kv| kv.strip_prefix("SUBSYSTEM="))?;
        Some(NativeEvent {
            added,
            bus: classify_subsystem(subsystem),
        })
    }

    fn classify_subsystem(s: &str) -> BusHint {
        if s.starts_with("usb") {
            BusHint::Usb
        } else if s == "pci" {
            BusHint::Pci
        } else if s == "nvme" {
            BusHint::Nvme
        } else if matches!(
            s,
            "block" | "scsi" | "ide" | "usb-storage" | "mmc" | "ata" | "sata"
        ) {
            BusHint::Storage
        } else {
            BusHint::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relevance_filters_other_buses() {
        assert!(NativeEvent {
            added: true,
            bus: BusHint::Usb
        }
        .is_relevant());
        assert!(NativeEvent {
            added: false,
            bus: BusHint::Pci
        }
        .is_relevant());
        assert!(!NativeEvent {
            added: true,
            bus: BusHint::Other
        }
        .is_relevant());
    }

    #[cfg(windows)]
    #[test]
    fn interface_classification() {
        assert_eq!(
            pnp::classify_interface(r"\\?\usb#vid_046d&pid_0825#6&2a"),
            BusHint::Usb
        );
        assert_eq!(
            pnp::classify_interface(r"\\?\pci#ven_10ec&dev_b822&subsys"),
            BusHint::Pci
        );
        assert_eq!(
            pnp::classify_interface(r"\\?\storage#nvme#disk"),
            BusHint::Nvme
        );
        assert_eq!(
            pnp::classify_interface(r"\\?\ide#disk#wd"),
            BusHint::Storage
        );
        assert_eq!(
            pnp::classify_interface(r"\\?\display#hdmi#monitor"),
            BusHint::Other
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uevent_parsing() {
        let add = b"add@/devices/pci0000:00/0000:00:14.0/usb1/1-1\0ACTION=add\0SUBSYSTEM=usb\0DEVNAME=/dev/bus/usb/001/002\0\0";
        let ev = udev::parse_uevent(add).expect("parses");
        assert!(ev.added);
        assert_eq!(ev.bus, BusHint::Usb);

        let remove = b"remove@/devices/virtual/mem/null\0ACTION=remove\0SUBSYSTEM=mem\0\0";
        let ev = udev::parse_uevent(remove).expect("parses");
        assert!(!ev.added);
        assert_eq!(ev.bus, BusHint::Other);
        assert!(!ev.is_relevant());

        let unrelated = b"move@/devices/somewhere\0ACTION=move\0SUBSYSTEM=net\0\0";
        assert!(udev::parse_uevent(unrelated).is_none());
    }
}
