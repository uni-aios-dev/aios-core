use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    NetBind,
    NetConnect,
    NetListen,
    FsRead,
    FsWrite,
    FsDelete,
    HwAccess,
    MemAlloc,
    MemShare,
    SchedModify,
    BlockLoad,
    BlockUnload,
    ProcessSpawn,
    ProcessKill,
    SystemConfig,
    All,
}

impl Capability {
    pub fn name(&self) -> &'static str {
        match self {
            Self::NetBind => "CAP_NET_BIND",
            Self::NetConnect => "CAP_NET_CONNECT",
            Self::NetListen => "CAP_NET_LISTEN",
            Self::FsRead => "CAP_FS_READ",
            Self::FsWrite => "CAP_FS_WRITE",
            Self::FsDelete => "CAP_FS_DELETE",
            Self::HwAccess => "CAP_HW_ACCESS",
            Self::MemAlloc => "CAP_MEM_ALLOC",
            Self::MemShare => "CAP_MEM_SHARE",
            Self::SchedModify => "CAP_SCHED_MODIFY",
            Self::BlockLoad => "CAP_BLOCK_LOAD",
            Self::BlockUnload => "CAP_BLOCK_UNLOAD",
            Self::ProcessSpawn => "CAP_PROCESS_SPAWN",
            Self::ProcessKill => "CAP_PROCESS_KILL",
            Self::SystemConfig => "CAP_SYSTEM_CONFIG",
            Self::All => "CAP_ALL",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::NetBind => "Bind to network ports",
            Self::NetConnect => "Initiate outbound connections",
            Self::NetListen => "Listen for incoming connections",
            Self::FsRead => "Read files from filesystem",
            Self::FsWrite => "Write files to filesystem",
            Self::FsDelete => "Delete files from filesystem",
            Self::HwAccess => "Access hardware directly",
            Self::MemAlloc => "Allocate system memory",
            Self::MemShare => "Share memory between blocks",
            Self::SchedModify => "Modify process scheduling",
            Self::BlockLoad => "Load new blocks into the system",
            Self::BlockUnload => "Unload blocks from the system",
            Self::ProcessSpawn => "Spawn new processes",
            Self::ProcessKill => "Terminate processes",
            Self::SystemConfig => "Modify system configuration",
            Self::All => "Unrestricted access to all capabilities",
        }
    }

    pub fn all_variants() -> Vec<Capability> {
        vec![
            Self::NetBind,
            Self::NetConnect,
            Self::NetListen,
            Self::FsRead,
            Self::FsWrite,
            Self::FsDelete,
            Self::HwAccess,
            Self::MemAlloc,
            Self::MemShare,
            Self::SchedModify,
            Self::BlockLoad,
            Self::BlockUnload,
            Self::ProcessSpawn,
            Self::ProcessKill,
            Self::SystemConfig,
        ]
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityToken {
    pub block_id: u32,
    pub capabilities: Vec<Capability>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub issuer_signature: [u8; 32],
}

impl CapabilityToken {
    pub fn new(
        block_id: u32,
        capabilities: Vec<Capability>,
        ttl_ms: u64,
        issuer_secret: &[u8],
    ) -> Self {
        let issued_at_ms = now_ms();
        let expires_at_ms = issued_at_ms + ttl_ms;
        let issuer_signature = Self::compute_signature(
            block_id,
            &capabilities,
            issued_at_ms,
            expires_at_ms,
            issuer_secret,
        );
        Self {
            block_id,
            capabilities,
            issued_at_ms,
            expires_at_ms,
            issuer_signature,
        }
    }

    pub fn has_capability(&self, cap: &Capability) -> bool {
        self.capabilities.contains(cap) || self.capabilities.contains(&Capability::All)
    }

    pub fn is_expired(&self) -> bool {
        now_ms() > self.expires_at_ms
    }

    pub fn remaining_ms(&self) -> u64 {
        now_ms().saturating_sub(self.expires_at_ms)
    }

    pub fn verify(&self, issuer_secret: &[u8]) -> bool {
        let expected = Self::compute_signature(
            self.block_id,
            &self.capabilities,
            self.issued_at_ms,
            self.expires_at_ms,
            issuer_secret,
        );
        self.issuer_signature == expected
    }

    pub fn compute_signature(
        block_id: u32,
        capabilities: &[Capability],
        issued_at_ms: u64,
        expires_at_ms: u64,
        secret: &[u8],
    ) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(secret);
        hasher.update(block_id.to_le_bytes());
        for cap in capabilities {
            hasher.update(cap.name().as_bytes());
        }
        hasher.update(issued_at_ms.to_le_bytes());
        hasher.update(expires_at_ms.to_le_bytes());
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_names() {
        assert_eq!(Capability::NetBind.name(), "CAP_NET_BIND");
        assert_eq!(Capability::All.name(), "CAP_ALL");
    }

    #[test]
    fn test_token_create_and_verify() {
        let token = CapabilityToken::new(1, vec![Capability::FsRead], 60_000, b"secret");
        assert!(token.verify(b"secret"));
        assert!(!token.verify(b"wrong"));
    }

    #[test]
    fn test_token_has_capability() {
        let token = CapabilityToken::new(
            1,
            vec![Capability::FsRead, Capability::FsWrite],
            60_000,
            b"secret",
        );
        assert!(token.has_capability(&Capability::FsRead));
        assert!(token.has_capability(&Capability::FsWrite));
        assert!(!token.has_capability(&Capability::NetBind));
    }

    #[test]
    fn test_token_all_grants_everything() {
        let token = CapabilityToken::new(1, vec![Capability::All], 60_000, b"secret");
        assert!(token.has_capability(&Capability::HwAccess));
        assert!(token.has_capability(&Capability::SystemConfig));
    }

    #[test]
    fn test_token_expiry() {
        let mut token = CapabilityToken::new(1, vec![Capability::FsRead], 60_000, b"secret");
        token.expires_at_ms = now_ms().saturating_sub(1000);
        assert!(token.is_expired());
    }

    #[test]
    fn test_all_variants_count() {
        assert_eq!(Capability::all_variants().len(), 15);
    }
}
