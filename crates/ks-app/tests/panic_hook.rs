//! The panic hook must run the terminal cleanup before the panic is reported.
//!
//! This lives in its own integration-test binary because the panic hook is process-global.

use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ks_app::terminal::install_panic_hook;

#[test]
fn cleanup_runs_when_a_panic_happens() {
    let cleaned = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cleaned);
    install_panic_hook(move || flag.store(true, Ordering::SeqCst));

    let result = panic::catch_unwind(|| panic!("boom"));

    assert!(result.is_err());
    assert!(
        cleaned.load(Ordering::SeqCst),
        "terminal cleanup did not run"
    );

    // A panic on another thread (e.g. a contained background task) must not restore the
    // terminal: the UI is still running.
    cleaned.store(false, Ordering::SeqCst);
    let joined = std::thread::spawn(|| panic!("worker boom")).join();
    assert!(joined.is_err());
    assert!(
        !cleaned.load(Ordering::SeqCst),
        "a worker-thread panic must not restore the terminal"
    );
}
