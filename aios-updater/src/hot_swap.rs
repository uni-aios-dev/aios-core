use aios_core::block::BlockId;

pub struct HotSwapEngine {
    swap_count: u64,
}

impl HotSwapEngine {
    pub fn new() -> Self {
        Self { swap_count: 0 }
    }

    pub fn perform_hot_swap(
        &mut self,
        block_id: BlockId,
        block_name: &str,
        old_version: &str,
        new_version: &str,
    ) -> Result<String, String> {
        self.swap_count += 1;
        log::info!(
            "Hot-swap #{}: '{}' (id={}) {old_version} -> {new_version}",
            self.swap_count,
            block_name,
            block_id.0,
        );
        Ok(format!(
            "Hot-swapped '{}' {old_version} -> {new_version}",
            block_name
        ))
    }

    pub fn swap_count(&self) -> u64 {
        self.swap_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hot_swap_engine_creation() {
        let engine = HotSwapEngine::new();
        assert_eq!(engine.swap_count(), 0);
    }

    #[test]
    fn test_perform_hot_swap() {
        let mut engine = HotSwapEngine::new();
        let result = engine
            .perform_hot_swap(BlockId(1), "test-block", "1.0", "2.0")
            .unwrap();
        assert!(result.contains("test-block"));
        assert_eq!(engine.swap_count(), 1);
    }
}
