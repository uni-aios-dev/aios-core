use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_core::crypto;
use aios_persistence::cow_storage::CopyOnWriteStorage;
use aios_persistence::recovery::RecoveryLog;
use aios_persistence::snapshot::{SnapshotManager, StateSnapshot};
use std::fs;

#[test]
fn test_snapshot_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SnapshotManager::new(dir.path().to_path_buf(), 1024 * 1024).unwrap();

    let snap = StateSnapshot::new(
        "snap_1".into(),
        vec![1, 2, 3, 4, 5],
        crypto::compute_sha256_bytes(&[1, 2, 3, 4, 5]),
    );
    mgr.save(&snap).unwrap();

    let loaded = mgr.load("snap_1").unwrap();
    assert_eq!(loaded.data, vec![1, 2, 3, 4, 5]);
    assert!(loaded.verify());
}

#[test]
fn test_snapshot_list_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SnapshotManager::new(dir.path().to_path_buf(), 1024 * 1024).unwrap();

    let s1 = StateSnapshot::new("a".into(), vec![10], crypto::compute_sha256_bytes(&[10]));
    let s2 = StateSnapshot::new("b".into(), vec![20], crypto::compute_sha256_bytes(&[20]));
    mgr.save(&s1).unwrap();
    mgr.save(&s2).unwrap();

    let list = mgr.list_snapshots().unwrap();
    assert_eq!(list.len(), 2);

    mgr.delete("a").unwrap();
    let list = mgr.list_snapshots().unwrap();
    assert_eq!(list.len(), 1);
    assert!(list.contains(&"b".to_string()));
}

#[test]
fn test_cow_storage_write_read() {
    let dir = tempfile::tempdir().unwrap();
    let storage = CopyOnWriteStorage::new(dir.path().to_path_buf()).unwrap();

    storage
        .atomic_write("block_a", b"hello AIOS persistence")
        .unwrap();
    let read = storage.read("block_a").unwrap();
    assert_eq!(read, b"hello AIOS persistence");
}

#[test]
fn test_cow_storage_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let storage = CopyOnWriteStorage::new(dir.path().to_path_buf()).unwrap();

    storage.atomic_write("test", b"original").unwrap();
    let rolled_back = storage.rollback("test").unwrap();
    assert!(!rolled_back);

    let read = storage.read("test").unwrap();
    assert_eq!(read, b"original");
}

#[test]
fn test_cow_storage_exists_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let storage = CopyOnWriteStorage::new(dir.path().to_path_buf()).unwrap();

    assert!(!storage.exists("nope"));
    storage.atomic_write("alpha", b"1").unwrap();
    assert!(storage.exists("alpha"));

    storage.delete("alpha").unwrap();
    assert!(!storage.exists("alpha"));
}

#[test]
fn test_cow_storage_file_size() {
    let dir = tempfile::tempdir().unwrap();
    let storage = CopyOnWriteStorage::new(dir.path().to_path_buf()).unwrap();

    storage.atomic_write("sized", b"hello").unwrap();
    let size = storage.file_size("sized").unwrap();
    assert_eq!(size, 5);
}

#[test]
fn test_recovery_log_entries() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("recovery.log");
    let mut log = RecoveryLog::new(log_path, 100).unwrap();

    let id1 = log.log_entry("swap", "block_a").unwrap();
    let id2 = log.log_entry("deploy", "block_b").unwrap();

    log.mark_completed(id1).unwrap();

    let pending = log.get_pending_entries().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, id2);
}

#[test]
fn test_recovery_log_clear() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("recovery.log");
    let mut log = RecoveryLog::new(log_path, 100).unwrap();

    log.log_entry("op1", "target1").unwrap();
    log.log_entry("op2", "target2").unwrap();
    log.clear().unwrap();

    let pending = log.get_pending_entries().unwrap();
    assert!(pending.is_empty());
}

#[test]
fn test_block_registry_disk_load_real() {
    let dir = tempfile::tempdir().unwrap();

    let wasm = r#"
        (module
            (func (export "init"))
            (func (export "version") (result i32) i32.const 42)
        )
    "#
    .as_bytes();
    fs::write(dir.path().join("mymod_1.0.0.wasm"), wasm).unwrap();

    let data = b"raw_binary_data";
    fs::write(dir.path().join("rawmod_2.0.0.bin"), data).unwrap();

    let mut registry = BlockRegistry::new();
    let results = registry.load_from_path(dir.path());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_ok()));

    let wasm_block = registry.find_by_name("mymod").unwrap();
    assert_eq!(wasm_block.manifest.version, "1.0.0");

    let bin_block = registry.find_by_name("rawmod").unwrap();
    assert_eq!(bin_block.manifest.version, "2.0.0");
    assert_eq!(bin_block.binary, data);
}

#[test]
fn test_block_loader_from_directory() {
    let dir = tempfile::tempdir().unwrap();

    for i in 0..5 {
        let name = format!("block_{}_1.0.0.bin", i);
        let path = dir.path().join(&name);
        fs::write(&path, format!("data_{}", i).as_bytes()).unwrap();
    }

    fs::write(dir.path().join("readme.txt"), b"ignore this").unwrap();

    let mut registry = BlockRegistry::new();
    let results = BlockLoader::load_from_directory(&mut registry, dir.path());
    assert_eq!(results.len(), 5);
    assert_eq!(registry.count(), 5);
}

#[test]
fn test_snapshot_large_payload() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SnapshotManager::new(dir.path().to_path_buf(), 1024 * 1024 * 10).unwrap();

    let large = vec![0xABu8; 1024 * 512];
    let snap = StateSnapshot::new(
        "big".into(),
        large.clone(),
        crypto::compute_sha256_bytes(&large),
    );
    mgr.save(&snap).unwrap();
    let loaded = mgr.load("big").unwrap();
    assert_eq!(loaded.data, large);
    assert!(loaded.verify());
}

#[test]
fn test_cow_storage_total_size() {
    let dir = tempfile::tempdir().unwrap();
    let storage = CopyOnWriteStorage::new(dir.path().to_path_buf()).unwrap();

    storage.atomic_write("f1", b"hello").unwrap();
    storage.atomic_write("f2", b"world!").unwrap();
    let total = storage.total_size().unwrap();
    assert_eq!(total, 11);
}
