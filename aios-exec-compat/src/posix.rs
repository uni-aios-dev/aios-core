use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PosixSyscall {
    SysOpen,
    SysRead,
    SysWrite,
    SysClose,
    SysLseek,
    SysFork,
    SysExec,
    SysExit,
    SysMmap,
    SysMunmap,
    SysSocket,
    SysConnect,
    SysSend,
    SysRecv,
    SysGetpid,
    SysGetuid,
    SysStat,
    SysFstat,
}

impl PosixSyscall {
    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            2 => Some(Self::SysOpen),
            0 => Some(Self::SysRead),
            1 => Some(Self::SysWrite),
            3 => Some(Self::SysClose),
            8 => Some(Self::SysLseek),
            57 => Some(Self::SysFork),
            59 => Some(Self::SysExec),
            60 => Some(Self::SysExit),
            9 => Some(Self::SysMmap),
            11 => Some(Self::SysMunmap),
            41 => Some(Self::SysSocket),
            42 => Some(Self::SysConnect),
            44 => Some(Self::SysSend),
            45 => Some(Self::SysRecv),
            39 => Some(Self::SysGetpid),
            24 => Some(Self::SysGetuid),
            4 => Some(Self::SysStat),
            5 => Some(Self::SysFstat),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::SysOpen => "sys_open",
            Self::SysRead => "sys_read",
            Self::SysWrite => "sys_write",
            Self::SysClose => "sys_close",
            Self::SysLseek => "sys_lseek",
            Self::SysFork => "sys_fork",
            Self::SysExec => "sys_exec",
            Self::SysExit => "sys_exit",
            Self::SysMmap => "sys_mmap",
            Self::SysMunmap => "sys_munmap",
            Self::SysSocket => "sys_socket",
            Self::SysConnect => "sys_connect",
            Self::SysSend => "sys_send",
            Self::SysRecv => "sys_recv",
            Self::SysGetpid => "sys_getpid",
            Self::SysGetuid => "sys_getuid",
            Self::SysStat => "sys_stat",
            Self::SysFstat => "sys_fstat",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyscallRequest {
    pub syscall_id: u32,
    pub args: [u64; 6],
    pub pid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyscallResponse {
    pub result: i64,
    pub errno: u32,
    pub out_data: Vec<u8>,
}

pub trait PosixTranslator: Send {
    fn translate(&self, request: &SyscallRequest) -> Result<SyscallResponse>;
    fn translate_to_ipc(&self, request: &SyscallRequest) -> Result<IpcPacket>;
    fn name(&self) -> &str;
}

pub struct DefaultPosixTranslator {
    block_id: u32,
    process_ram_limits: HashMap<u64, u64>,
}

impl DefaultPosixTranslator {
    pub fn new(block_id: u32) -> Self {
        Self {
            block_id,
            process_ram_limits: HashMap::new(),
        }
    }

    pub fn set_ram_limit(&mut self, pid: u64, limit_mb: u64) {
        self.process_ram_limits.insert(pid, limit_mb);
    }
}

impl PosixTranslator for DefaultPosixTranslator {
    fn translate(&self, request: &SyscallRequest) -> Result<SyscallResponse> {
        let syscall = PosixSyscall::from_id(request.syscall_id).ok_or_else(|| {
            AIOSException::IPCError(format!("Unknown POSIX syscall: {}", request.syscall_id))
        })?;

        match syscall {
            PosixSyscall::SysRead => Ok(SyscallResponse {
                result: request.args[1] as i64,
                errno: 0,
                out_data: vec![0; request.args[1] as usize],
            }),
            PosixSyscall::SysWrite => Ok(SyscallResponse {
                result: request.args[2] as i64,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysOpen => Ok(SyscallResponse {
                result: 3,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysClose => Ok(SyscallResponse {
                result: 0,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysExit => Ok(SyscallResponse {
                result: 0,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysGetpid => Ok(SyscallResponse {
                result: request.pid as i64,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysGetuid => Ok(SyscallResponse {
                result: 1000,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysFork => Ok(SyscallResponse {
                result: 0,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysLseek => Ok(SyscallResponse {
                result: request.args[1] as i64,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysMmap => Ok(SyscallResponse {
                result: request.args[0] as i64,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysMunmap => Ok(SyscallResponse {
                result: 0,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysStat => Ok(SyscallResponse {
                result: 0,
                errno: 0,
                out_data: vec![0; 144],
            }),
            PosixSyscall::SysFstat => Ok(SyscallResponse {
                result: 0,
                errno: 0,
                out_data: vec![0; 144],
            }),
            PosixSyscall::SysSocket => Ok(SyscallResponse {
                result: 4,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysConnect => Ok(SyscallResponse {
                result: 0,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysSend => Ok(SyscallResponse {
                result: request.args[2] as i64,
                errno: 0,
                out_data: Vec::new(),
            }),
            PosixSyscall::SysRecv => Ok(SyscallResponse {
                result: request.args[1] as i64,
                errno: 0,
                out_data: vec![0; request.args[1] as usize],
            }),
            PosixSyscall::SysExec => Ok(SyscallResponse {
                result: 0,
                errno: 0,
                out_data: Vec::new(),
            }),
        }
    }

    fn translate_to_ipc(&self, request: &SyscallRequest) -> Result<IpcPacket> {
        let syscall = PosixSyscall::from_id(request.syscall_id).ok_or_else(|| {
            AIOSException::IPCError(format!("Unknown POSIX syscall: {}", request.syscall_id))
        })?;

        let payload = Payload::Custom(
            format!("posix:{}", syscall.name()),
            bincode::serialize(request).unwrap_or_default(),
        );

        Ok(IpcPacket::new(self.block_id, 0, CommandId::Custom, payload))
    }

    fn name(&self) -> &str {
        "default-posix-translator"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(syscall_id: u32, args: [u64; 6]) -> SyscallRequest {
        SyscallRequest {
            syscall_id,
            args,
            pid: 100,
        }
    }

    #[test]
    fn test_posix_syscall_names() {
        assert_eq!(PosixSyscall::SysOpen.name(), "sys_open");
        assert_eq!(PosixSyscall::SysRead.name(), "sys_read");
        assert_eq!(PosixSyscall::SysWrite.name(), "sys_write");
        assert_eq!(PosixSyscall::SysFork.name(), "sys_fork");
        assert_eq!(PosixSyscall::SysExit.name(), "sys_exit");
        assert_eq!(PosixSyscall::SysGetpid.name(), "sys_getpid");
    }

    #[test]
    fn test_posix_from_id() {
        assert_eq!(PosixSyscall::from_id(2), Some(PosixSyscall::SysOpen));
        assert_eq!(PosixSyscall::from_id(0), Some(PosixSyscall::SysRead));
        assert_eq!(PosixSyscall::from_id(1), Some(PosixSyscall::SysWrite));
        assert_eq!(PosixSyscall::from_id(999), None);
    }

    #[test]
    fn test_translate_read() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(0, [0, 512, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 512);
        assert_eq!(resp.errno, 0);
        assert_eq!(resp.out_data.len(), 512);
    }

    #[test]
    fn test_translate_write() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(1, [1, 0, 256, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 256);
        assert_eq!(resp.errno, 0);
    }

    #[test]
    fn test_translate_open() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(2, [0, 0, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 3);
        assert_eq!(resp.errno, 0);
    }

    #[test]
    fn test_translate_close() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(3, [3, 0, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0);
    }

    #[test]
    fn test_translate_exit() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(60, [0, 0, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0);
    }

    #[test]
    fn test_translate_getpid() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(39, [0, 0, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 100);
    }

    #[test]
    fn test_translate_getuid() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(24, [0, 0, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 1000);
    }

    #[test]
    fn test_translate_unknown_syscall() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(999, [0, 0, 0, 0, 0, 0]);
        assert!(translator.translate(&req).is_err());
    }

    #[test]
    fn test_translate_to_ipc() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(0, [0, 128, 0, 0, 0, 0]);
        let pkt = translator.translate_to_ipc(&req).unwrap();
        assert_eq!(pkt.header.source_block, 1);
        assert_eq!(pkt.header.command_id, CommandId::Custom as u16);
    }

    #[test]
    fn test_translate_fork() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(57, [0, 0, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0);
        assert_eq!(resp.errno, 0);
    }

    #[test]
    fn test_translate_lseek() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(8, [3, 4096, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 4096);
    }

    #[test]
    fn test_translate_mmap() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(9, [4096, 0, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 4096);
    }

    #[test]
    fn test_translate_socket() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(41, [0, 0, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 4);
    }

    #[test]
    fn test_translate_send() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(44, [4, 0, 100, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 100);
    }

    #[test]
    fn test_translate_recv() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(45, [4, 256, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 256);
        assert_eq!(resp.out_data.len(), 256);
    }

    #[test]
    fn test_translate_stat() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(4, [0, 0, 0, 0, 0, 0]);
        let resp = translator.translate(&req).unwrap();
        assert_eq!(resp.result, 0);
        assert_eq!(resp.out_data.len(), 144);
    }

    #[test]
    fn test_ram_limit() {
        let mut translator = DefaultPosixTranslator::new(1);
        translator.set_ram_limit(100, 512);
        assert_eq!(translator.process_ram_limits.get(&100), Some(&512));
    }

    #[test]
    fn test_translator_name() {
        let translator = DefaultPosixTranslator::new(1);
        assert_eq!(translator.name(), "default-posix-translator");
    }

    #[test]
    fn test_speed_syscall_translation() {
        let translator = DefaultPosixTranslator::new(1);
        let req = make_request(0, [0, 512, 0, 0, 0, 0]);

        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            let _ = translator.translate(&req).unwrap();
        }
        let elapsed = start.elapsed();
        let per_us = elapsed.as_micros() as f64 / 10_000.0;
        let threshold = if cfg!(debug_assertions) { 5.0 } else { 2.0 };
        assert!(
            per_us < threshold,
            "Syscall translation too slow: {per_us} us (threshold: {threshold})"
        );
    }
}
