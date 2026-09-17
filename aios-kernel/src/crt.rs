//! Minimal C runtime memory primitives required by the compiler.
//!
//! LLVM lowers bulk copies/fills/compares into calls to `memcpy`, `memmove`,
//! `memset` and `memcmp`/`bcmp`. The prebuilt `core` ships weak versions of
//! these (via `compiler_builtins`); the strong definitions here keep the linker
//! output predictable and give the kernel one place to own these routines.
//!
//! Each routine is implemented with `core::ptr` intrinsics so the optimizer
//! cannot rewrite its own loop into a recursive call to itself.

use core::ffi::c_void;

/// Copies `n` bytes from `src` to `dest`. The regions must not overlap.
///
/// # Safety
/// `dest` and `src` must be valid for `n` bytes and must not overlap.
#[no_mangle]
pub unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    core::ptr::copy_nonoverlapping(src as *const u8, dest as *mut u8, n);
    dest
}

/// Copies `n` bytes from `src` to `dest`, allowing the regions to overlap.
///
/// # Safety
/// `dest` and `src` must be valid for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    core::ptr::copy(src as *const u8, dest as *mut u8, n);
    dest
}

/// Fills `n` bytes at `dest` with the low byte of `c`.
///
/// # Safety
/// `dest` must be valid for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn memset(dest: *mut c_void, c: i32, n: usize) -> *mut c_void {
    core::ptr::write_bytes(dest as *mut u8, c as u8, n);
    dest
}

/// Compares `n` bytes of `a` and `b`; returns <0, 0 or >0.
///
/// # Safety
/// `a` and `b` must be valid for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    let a = a as *const u8;
    let b = b as *const u8;
    let mut i = 0usize;
    while i < n {
        let x = a.add(i).read();
        let y = b.add(i).read();
        if x != y {
            return x as i32 - y as i32;
        }
        i += 1;
    }
    0
}

/// Alias of [`memcmp`]; some LLVM lowering paths emit a `bcmp` call.
///
/// # Safety
/// Same contract as [`memcmp`].
#[no_mangle]
pub unsafe extern "C" fn bcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    memcmp(a, b, n)
}
