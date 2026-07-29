use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Win32Api {
    CreateFileW,
    ReadFile,
    WriteFile,
    CloseHandle,
    GetProcAddress,
    LoadLibraryW,
    VirtualAlloc,
    VirtualFree,
    CreateThread,
    ExitProcess,
    GetLastError,
    SetCurrentDirectoryW,
    GetModuleHandleW,
    CreateMutexW,
    WaitForSingleObject,
    ReleaseMutex,
}

impl Win32Api {
    pub fn from_ord(ord: u16) -> Option<Self> {
        match ord {
            0x0052 => Some(Self::CreateFileW),
            0x001D => Some(Self::ReadFile),
            0x0015 => Some(Self::WriteFile),
            0x001C => Some(Self::CloseHandle),
            0x01C2 => Some(Self::GetProcAddress),
            0x00D9 => Some(Self::LoadLibraryW),
            0x0030 => Some(Self::VirtualAlloc),
            0x0031 => Some(Self::VirtualFree),
            0x0025 => Some(Self::CreateThread),
            0x0004 => Some(Self::ExitProcess),
            0x000B => Some(Self::GetLastError),
            0x01A3 => Some(Self::SetCurrentDirectoryW),
            0x01E3 => Some(Self::GetModuleHandleW),
            0x0152 => Some(Self::CreateMutexW),
            0x0001 => Some(Self::WaitForSingleObject),
            0x0051 => Some(Self::ReleaseMutex),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::CreateFileW => "CreateFileW",
            Self::ReadFile => "ReadFile",
            Self::WriteFile => "WriteFile",
            Self::CloseHandle => "CloseHandle",
            Self::GetProcAddress => "GetProcAddress",
            Self::LoadLibraryW => "LoadLibraryW",
            Self::VirtualAlloc => "VirtualAlloc",
            Self::VirtualFree => "VirtualFree",
            Self::CreateThread => "CreateThread",
            Self::ExitProcess => "ExitProcess",
            Self::GetLastError => "GetLastError",
            Self::SetCurrentDirectoryW => "SetCurrentDirectoryW",
            Self::GetModuleHandleW => "GetModuleHandleW",
            Self::CreateMutexW => "CreateMutexW",
            Self::WaitForSingleObject => "WaitForSingleObject",
            Self::ReleaseMutex => "ReleaseMutex",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Win32Request {
    pub api_ord: u16,
    pub args: [u64; 4],
    pub pid: u64,
    pub process_handle: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Win32Response {
    pub result: i64,
    pub win_error: u32,
    pub out_data: Vec<u8>,
}

pub trait Win32Translator: Send {
    fn translate(&self, request: &Win32Request) -> Result<Win32Response>;
    fn translate_to_ipc(&self, request: &Win32Request) -> Result<IpcPacket>;
    fn name(&self) -> &str;
    fn register_dll(&mut self, name: &str, base_addr: u64);
    fn registered_dlls(&self) -> &HashMap<String, u64>;
}

pub struct DefaultWin32Translator {
    #[allow(dead_code)]
    block_id: u32,
    loaded_dlls: HashMap<String, u64>,
    #[allow(dead_code)]
    next_handle: u64,
}

impl DefaultWin32Translator {
    pub fn new(block_id: u32) -> Self {
        Self {
            block_id,
            loaded_dlls: HashMap::new(),
            next_handle: 100,
        }
    }

    #[allow(dead_code)]
    fn alloc_handle(&mut self) -> u64 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }
}

impl Win32Translator for DefaultWin32Translator {
    fn translate(&self, request: &Win32Request) -> Result<Win32Response> {
        let api = Win32Api::from_ord(request.api_ord).ok_or_else(|| {
            AIOSException::IPCError(format!(
                "Unknown Win32 API ordinal: 0x{:04X}",
                request.api_ord
            ))
        })?;

        match api {
            Win32Api::CreateFileW => Ok(Win32Response {
                result: 100,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::ReadFile => Ok(Win32Response {
                result: 1,
                win_error: 0,
                out_data: vec![0; request.args[2] as usize],
            }),
            Win32Api::WriteFile => Ok(Win32Response {
                result: 1,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::CloseHandle => Ok(Win32Response {
                result: 1,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::GetProcAddress => Ok(Win32Response {
                result: 0x7FF00000,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::LoadLibraryW => Ok(Win32Response {
                result: 0x70000000,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::VirtualAlloc => Ok(Win32Response {
                result: request.args[0] as i64,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::VirtualFree => Ok(Win32Response {
                result: 1,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::CreateThread => Ok(Win32Response {
                result: 200,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::ExitProcess => Ok(Win32Response {
                result: 0,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::GetLastError => Ok(Win32Response {
                result: 0,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::SetCurrentDirectoryW => Ok(Win32Response {
                result: 1,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::GetModuleHandleW => Ok(Win32Response {
                result: 0x70000000,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::CreateMutexW => Ok(Win32Response {
                result: 300,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::WaitForSingleObject => Ok(Win32Response {
                result: 0,
                win_error: 0,
                out_data: Vec::new(),
            }),
            Win32Api::ReleaseMutex => Ok(Win32Response {
                result: 1,
                win_error: 0,
                out_data: Vec::new(),
            }),
        }
    }

    fn translate_to_ipc(&self, request: &Win32Request) -> Result<IpcPacket> {
        let api = Win32Api::from_ord(request.api_ord).ok_or_else(|| {
            AIOSException::IPCError(format!(
                "Unknown Win32 API ordinal: 0x{:04X}",
                request.api_ord
            ))
        })?;

        let payload = Payload::Custom(
            format!("win32:{}", api.name()),
            bincode::serialize(request).unwrap_or_default(),
        );

        Ok(IpcPacket::new(self.block_id, 0, CommandId::Custom, payload))
    }

    fn name(&self) -> &str {
        "default-win32-translator"
    }

    fn register_dll(&mut self, name: &str, base_addr: u64) {
        self.loaded_dlls.insert(name.to_string(), base_addr);
    }

    fn registered_dlls(&self) -> &HashMap<String, u64> {
        &self.loaded_dlls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_win32_request(api_ord: u16, args: [u64; 4]) -> Win32Request {
        Win32Request {
            api_ord,
            args,
            pid: 100,
            process_handle: 50,
        }
    }

    #[test]
    fn test_win32_api_names() {
        assert_eq!(Win32Api::CreateFileW.name(), "CreateFileW");
        assert_eq!(Win32Api::ReadFile.name(), "ReadFile");
        assert_eq!(Win32Api::WriteFile.name(), "WriteFile");
        assert_eq!(Win32Api::VirtualAlloc.name(), "VirtualAlloc");
        assert_eq!(Win32Api::ExitProcess.name(), "ExitProcess");
    }

    #[test]
    fn test_win32_from_ord() {
        assert_eq!(Win32Api::from_ord(0x0052), Some(Win32Api::CreateFileW));
        assert_eq!(Win32Api::from_ord(0x001D), Some(Win32Api::ReadFile));
        assert_eq!(Win32Api::from_ord(0x0030), Some(Win32Api::VirtualAlloc));
        assert_eq!(Win32Api::from_ord(0xFFFF), None);
    }

    #[test]
    fn test_translate_create_file() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x0052, [0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 100);
        assert_eq!(resp.win_error, 0);
    }

    #[test]
    fn test_translate_read_file() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x001D, [100, 0, 4096, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 1);
        assert_eq!(resp.out_data.len(), 4096);
    }

    #[test]
    fn test_translate_write_file() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x0015, [100, 0, 256, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 1);
    }

    #[test]
    fn test_translate_virtual_alloc() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x0030, [0x10000, 4096, 0x3000, 0x04]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0x10000);
    }

    #[test]
    fn test_translate_create_thread() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x0025, [0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 200);
    }

    #[test]
    fn test_translate_exit_process() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x0004, [0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0);
    }

    #[test]
    fn test_translate_unknown_api() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0xFFFF, [0, 0, 0, 0]);
        assert!(translator.translate(&req).is_err());
    }

    #[test]
    fn test_translate_to_ipc() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x0052, [0, 0, 0, 0]);
        let pkt = translator.translate_to_ipc(&req).unwrap();
        assert_eq!(pkt.header.source_block, 1);
        assert_eq!(pkt.header.command_id, CommandId::Custom as u16);
    }

    #[test]
    fn test_register_dll() {
        let mut translator = DefaultWin32Translator::new(1);
        translator.register_dll("kernel32.dll", 0x7FF00000);
        assert_eq!(
            translator.registered_dlls().get("kernel32.dll"),
            Some(&0x7FF00000)
        );
    }

    #[test]
    fn test_translator_name() {
        let translator = DefaultWin32Translator::new(1);
        assert_eq!(translator.name(), "default-win32-translator");
    }

    #[test]
    fn test_translate_get_last_error() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x000B, [0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0);
    }

    #[test]
    fn test_translate_load_library() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x00D9, [0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0x70000000);
    }

    #[test]
    fn test_translate_close_handle() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x001C, [100, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 1);
    }

    #[test]
    fn test_translate_create_mutex() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x0152, [0, 1, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 300);
    }

    #[test]
    fn test_translate_wait_single() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x0001, [300, 5000, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0);
    }

    #[test]
    fn test_translate_get_proc_address() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x01C2, [0x70000000, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0x7FF00000);
    }

    #[test]
    fn test_speed_win32_translation() {
        let translator = DefaultWin32Translator::new(1);
        let req = make_win32_request(0x0052, [0, 0, 0, 0]);

        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            let _ = translator.translate(&req).unwrap();
        }
        let elapsed = start.elapsed();
        let per_us = elapsed.as_micros() as f64 / 10_000.0;
        let threshold = if cfg!(debug_assertions) { 5.0 } else { 2.0 };
        assert!(
            per_us < threshold,
            "Win32 translation too slow: {per_us} us (threshold: {threshold})"
        );
    }
}
