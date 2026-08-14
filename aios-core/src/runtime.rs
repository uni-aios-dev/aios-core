//! Bridges synchronous callers into the async world.
//!
//! Several subsystems expose synchronous wrappers around async internals
//! (driver provisioning, store installs, browser navigation). Those wrappers
//! are called both from plain threads and from the kernel's `#[tokio::main]`
//! main thread, which already lives inside a tokio runtime. Creating a fresh
//! runtime and calling `block_on` in that context panics with
//! "Cannot start a runtime from within a runtime". [`block_on_future`]
//! detects the situation and blocks through the existing handle instead.

use std::future::Future;

/// Run `fut` to completion from a caller that does not own a tokio runtime.
///
/// - Outside any runtime: builds a fresh multi-thread runtime and blocks on it.
/// - Inside a multi-thread runtime (e.g. the kernel TUI's `#[tokio::main]`
///   main thread): parks the current worker with `block_in_place` and blocks on
///   the same handle, so no nested runtime is created and other runtime tasks
///   keep making progress. This path works for non-`Send` futures (e.g. one
///   borrowing a non-`Send` engine).
/// - Inside a single-thread runtime: falls back to a fresh runtime, which tokio
///   still forbids; no codebase path calls into this case.
pub fn block_on_future<F>(fut: F) -> F::Output
where
    F: Future,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                tokio::task::block_in_place(move || handle.block_on(fut))
            } else {
                fresh_runtime().block_on(fut)
            }
        }
        Err(_) => fresh_runtime().block_on(fut),
    }
}

fn fresh_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("failed to build blocking tokio runtime")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_from_plain_thread() {
        assert_eq!(block_on_future(async { 6 * 7 }), 42);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runs_inside_multi_thread_runtime_without_nesting() {
        let v = block_on_future(async { 40 + 2 });
        assert_eq!(v, 42);
        assert!(block_on_future(async { 9 }) == 9);
    }
}
