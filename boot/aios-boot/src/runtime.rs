//! Symbols the `x86_64-unknown-uefi` target expects from a C runtime.
//!
//! The target has no libc. LLVM, however, recognises the "read a wide string
//! until NUL" loop in `CStr16::from_ptr` (used by `uefi::system::firmware_vendor`
//! and the HII protocol wrappers) and lowers it to a call to `wcslen`. Without
//! this shim the final link fails with `undefined symbol: wcslen`.

/// Length of the NUL-terminated wide string at `s`, excluding the terminator.
///
/// This is the implementation LLVM's `wcslen` libcall resolves to. The read is
/// volatile on purpose: a plain loop would itself be recognised as a `wcslen`
/// idiom and turn into infinite recursion.
///
/// # Safety
/// `s` must point to a readable NUL-terminated array of 16-bit code units.
#[no_mangle]
pub unsafe extern "C" fn wcslen(s: *const u16) -> usize {
    let mut len = 0usize;
    loop {
        // SAFETY: the caller guarantees a NUL-terminated array.
        let unit = unsafe { core::ptr::read_volatile(s.add(len)) };
        if unit == 0 {
            return len;
        }
        len += 1;
    }
}
