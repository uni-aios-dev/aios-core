//! Native block factory registry.
//!
//! A *native block* is a [`StatefulBlock`](aios_core::block::StatefulBlock)
//! implementation compiled into the kernel binary instead of being loaded from
//! disk as a WebAssembly module. Native blocks form the "base set" of the
//! hybrid block loader: they are created on demand through a [`BlockFactory`]
//! and registered with the `MessageRouter` exactly like a disk block would be,
//! so the kernel stays uniformly block-oriented.

use aios_core::block::{BlockId, StatefulBlock};
use aios_core::error::{AIOSException, Result};

/// Constructor for a compiled-in (native) block.
///
/// Implementations are cheap to clone-free and only build the block instance
/// when [`create`](BlockFactory::create) is called with the [`BlockId`] the
/// registry assigned.
pub trait BlockFactory: Send + Sync {
    /// Unique block name (also used as the registry lookup key).
    fn name(&self) -> &str;

    /// Block version string reported in the registry manifest.
    fn version(&self) -> &str;

    /// Build a fresh block instance bound to `id`.
    fn create(&self, id: BlockId) -> Result<Box<dyn StatefulBlock>>;
}

/// Ordered registry of [`BlockFactory`] implementations.
///
/// The registry is the single source of truth for the compiled-in base set.
/// Which factories are registered can be selected at build time (feature flags)
/// or at runtime, giving the hybrid loader its "base set" half.
pub struct BlockFactoryRegistry {
    factories: Vec<Box<dyn BlockFactory>>,
}

impl BlockFactoryRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    /// Register a factory. Fails if the name is already taken.
    pub fn register(&mut self, factory: Box<dyn BlockFactory>) -> Result<()> {
        if self.contains(factory.name()) {
            return Err(AIOSException::BlockAlreadyRegistered(
                factory.name().to_string(),
            ));
        }
        log::info!(
            "BlockFactory: registered native block '{}' v{}",
            factory.name(),
            factory.version()
        );
        self.factories.push(factory);
        Ok(())
    }

    /// Look up a factory by name.
    pub fn get(&self, name: &str) -> Option<&dyn BlockFactory> {
        self.factories
            .iter()
            .find(|f| f.name() == name)
            .map(|f| f.as_ref())
    }

    /// Whether a factory with `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Names of all registered factories, in registration order.
    pub fn names(&self) -> Vec<&str> {
        self.factories.iter().map(|f| f.name()).collect()
    }

    /// Number of registered factories.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    /// Instantiate the named block, returning [`AIOSException::BlockNotFound`]
    /// when no factory matches.
    pub fn instantiate(&self, name: &str, id: BlockId) -> Result<Box<dyn StatefulBlock>> {
        self.get(name)
            .ok_or_else(|| AIOSException::BlockNotFound(name.to_string()))?
            .create(id)
    }
}

impl Default for BlockFactoryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::block::BlockState;
    use aios_core::ipc_protocol::IpcPacket;

    struct EchoBlock {
        id: BlockId,
        name: String,
    }

    impl StatefulBlock for EchoBlock {
        fn id(&self) -> BlockId {
            self.id
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &str {
            "9.9.9"
        }
        fn state(&self) -> BlockState {
            BlockState::Active
        }
        fn handle_message(&mut self, _packet: &IpcPacket) -> Result<Option<IpcPacket>> {
            Ok(None)
        }
    }

    struct EchoFactory(&'static str);

    impl BlockFactory for EchoFactory {
        fn name(&self) -> &str {
            self.0
        }
        fn version(&self) -> &str {
            "9.9.9"
        }
        fn create(&self, id: BlockId) -> Result<Box<dyn StatefulBlock>> {
            Ok(Box::new(EchoBlock {
                id,
                name: self.0.to_string(),
            }))
        }
    }

    #[test]
    fn test_register_and_lookup() {
        let mut reg = BlockFactoryRegistry::new();
        reg.register(Box::new(EchoFactory("alpha"))).unwrap();
        reg.register(Box::new(EchoFactory("beta"))).unwrap();

        assert_eq!(reg.len(), 2);
        assert!(!reg.is_empty());
        assert!(reg.contains("alpha"));
        assert_eq!(reg.names(), vec!["alpha", "beta"]);
        assert_eq!(reg.get("beta").unwrap().version(), "9.9.9");
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn test_duplicate_registration_rejected() {
        let mut reg = BlockFactoryRegistry::new();
        reg.register(Box::new(EchoFactory("dup"))).unwrap();
        let err = reg.register(Box::new(EchoFactory("dup"))).unwrap_err();
        assert!(matches!(err, AIOSException::BlockAlreadyRegistered(_)));
    }

    #[test]
    fn test_instantiate_binds_id() {
        let mut reg = BlockFactoryRegistry::new();
        reg.register(Box::new(EchoFactory("alpha"))).unwrap();

        let block = reg.instantiate("alpha", BlockId::new(42)).unwrap();
        assert_eq!(block.id(), BlockId::new(42));
        assert_eq!(block.name(), "alpha");
        assert_eq!(block.version(), "9.9.9");
    }

    #[test]
    fn test_instantiate_unknown_fails() {
        let reg = BlockFactoryRegistry::new();
        let err = reg.instantiate("nope", BlockId::new(1)).err().unwrap();
        assert!(matches!(err, AIOSException::BlockNotFound(_)));
    }

    #[test]
    fn test_default_is_empty() {
        let reg = BlockFactoryRegistry::default();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }
}
