# Phase 0: Advanced Optimization Testing Guide

## Overview

This document describes how to test the new Phase 0 (Priority 0) optimization crates on a virtual machine.

**Implemented Phases:**
- ✅ Phase 15: Zero-Copy IPC Ring Buffers (`aios-ringbuf` crate)
- ✅ Phase 17: AI KV-Cache & State Compression (`aios-compress` crate)
- ✅ Phase 18: Atomic Copy-on-Write Persistence (`aios-persistence` crate)
- ⏳ Phase 16: Hardware-Enforced Memory Protection (pending)

---

## Build Instructions

```powershell
# Set up PATH (Windows)
$env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")

# Build entire workspace
cd c:\wiprcode\AIOS
cargo build --workspace --release

# Build specific crates
cargo build -p aios-ringbuf --release
cargo build -p aios-compress --release
cargo build -p aios-persistence --release
```

---

## Run Tests

```powershell
# Run all tests
cargo test --workspace --lib

# Run specific crate tests
cargo test -p aios-ringbuf --lib
cargo test -p aios-compress --lib
cargo test -p aios-persistence --lib

# Run with output
cargo test --workspace --lib -- --nocapture
```

---

## Test Results Summary

### aios-ringbuf (Zero-Copy IPC Ring Buffers)
- **Tests**: 8 unit tests
- **Status**: ✅ All passing
- **Coverage**:
  - Lock-free ring buffer creation
  - Write/read operations
  - Wraparound handling
  - Zero-copy pointers
  - Fill ratio calculation
  - Overflow detection

### aios-compress (Memory Compression)
- **Tests**: 16 unit tests
- **Status**: ✅ All passing
- **Coverage**:
  - FP8 quantization (32-bit → 8-bit)
  - INT4 quantization (32-bit → 4-bit)
  - ZSTD compression/decompression
  - LRU cache management
  - Compression ratio estimation

### aios-persistence (CoW State Persistence)
- **Tests**: 6 unit tests (Unix), 3 on Windows
- **Status**: ✅ All passing
- **Coverage**:
  - State snapshot creation and serialization
  - Recovery log management
  - Atomic write operations
  - File integrity verification

---

## Virtual Machine Setup

### Recommended VM Configuration

```yaml
OS: Windows 11 or Linux (Ubuntu 22.04)
CPU: 4+ cores
RAM: 8+ GB
Disk: 20+ GB (SSD recommended)
```

### Dependencies

**Windows:**
```powershell
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Verify installation
rustc --version
cargo --version
```

**Linux:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

---

## Performance Benchmarking

### Ring Buffer Latency

```powershell
# Run benchmarks for write/read operations
cargo bench -p aios-ringbuf
```

Expected results (Release mode):
- Single write: < 1 microsecond
- Single read: < 1 microsecond
- Wraparound write: < 2 microseconds

### Compression Performance

```powershell
# Test compression speed
cargo test -p aios-compress --release -- --nocapture
```

Expected results:
- FP8 quantization: < 1 microsecond per sample
- ZSTD compression (level 3): ~100 MB/s
- Compression ratio: 2-5x for system state

### Persistence Operations

```powershell
# Test atomic write performance
cargo test -p aios-persistence --release -- --nocapture
```

Expected results:
- Snapshot creation: < 10 milliseconds (1MB state)
- Atomic rename: < 1 millisecond
- Recovery from log: < 50 milliseconds

---

## Integration with Existing AIOS

### Using Ring Buffers

```rust
use aios_ringbuf::{RingBuffer, RingBufferConfig};

let config = RingBufferConfig {
    capacity: 65536,
    zero_copy: true,
};

let rb = RingBuffer::new(config)?;
rb.write(b"Hello, Zero-Copy")?;

let mut buf = vec![0u8; 32];
let read = rb.read(&mut buf)?;
```

### Using Compression

```rust
use aios_compress::{StateCompressor, Quantizer};

let compressor = StateCompressor::new();
let compressed = compressor.compress(state_data)?;

let quantizer = Quantizer::new();
let quantized_fp8 = quantizer.quantize_fp8(float_buffer);
```

### Using Persistence

```rust
use aios_persistence::{SnapshotManager, CopyOnWriteStorage};

let storage = CopyOnWriteStorage::new(path)?;
storage.atomic_write("state.bin", state_data)?;

let loaded = storage.read("state.bin")?;
storage.rollback("state.bin")?;
```

---

## Known Limitations

### Windows Compatibility
- File-based tests (`cow_storage.rs`) are Unix-only due to file locking
- Recovery log parsing requires binary-safe handling
- Atomic rename works but with different guarantees than Linux

### Performance Notes
- Ring buffers use busy-spinning in `wait_for_data()` / `wait_for_space()`
- Consider adding condition variables or async/await in future versions
- Memory quantization is lossy (acceptable for idle buffers)

---

## Next Steps

### Phase 16: Hardware Memory Protection
```rust
// Planned: MPK/PKS integration
use aios_security::mpk::MemoryProtectionKey;

let mpk = MemoryProtectionKey::new(block_id)?;
mpk.protect_range(ptr, size)?;
```

### Integration with Live-Update
- Connect `aios-persistence` snapshots to `aios-live-update` hot-swap
- Use CoW storage as backup during block replacement
- Rollback to prior state if swap fails

### Performance Optimization
- Benchmark against current VecDeque-based IPC bus
- Measure memory overhead vs. throughput gains
- Profile on real hardware

---

## Troubleshooting

### Compilation Errors

**Error**: `cargo: command not found`
- **Solution**: Add Rust to PATH manually
  ```powershell
  $env:Path = $env:Path + ";C:\Users\<user>\.cargo\bin"
  ```

**Error**: `failed to read Cargo.toml`
- **Solution**: Ensure all new crates are listed in workspace members
  - Check `Cargo.toml` at root level

### Test Failures

**Error**: File access denied on Windows
- **Solution**: Tests are marked `#[cfg(unix)]` - skip on Windows
- **Expected**: 6 tests skipped on Windows, all passing on Linux

**Error**: Precision loss assertion in FP8 tests
- **Solution**: FP8 has inherent quantization loss (~0.2 units)
- **Expected**: Test tolerates ±0.3 range

---

## Contact & Feedback

For issues or feedback on Phase 0 implementations:
1. Check `docs/BUGS.md` for known issues
2. Review `AGENTS.md` for development rules
3. Update `docs/CHANGELOG.md` with findings

