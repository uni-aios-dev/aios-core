use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::Priority;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn test_real_thread_executes_work() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();

    let mut s = Scheduler::new(1024);
    let pid = s
        .spawn_real_process("worker", Priority::Normal, 32, move |_term, _susp| {
            c.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

    assert!(s.is_real_process(pid));
    std::thread::sleep(Duration::from_millis(50));
    assert!(counter.load(Ordering::SeqCst) >= 1);

    s.kill_process(pid).unwrap();
}

#[test]
fn test_real_thread_terminate_signal() {
    let ran = Arc::new(AtomicBool::new(false));
    let r = ran.clone();

    let mut s = Scheduler::new(1024);
    let pid = s
        .spawn_real_process("looper", Priority::Normal, 16, move |term, _susp| {
            while !term.should_stop() {
                std::thread::sleep(Duration::from_millis(5));
            }
            r.store(true, Ordering::SeqCst);
        })
        .unwrap();

    std::thread::sleep(Duration::from_millis(20));
    s.kill_process(pid).unwrap();
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn test_real_thread_suspend_resume() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    let mut s = Scheduler::new(1024);
    let pid = s
        .spawn_real_process("counter", Priority::Normal, 16, move |_term, susp| {
            while r.load(Ordering::SeqCst) {
                if susp.is_suspended() {
                    std::thread::park();
                }
                c.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
            }
        })
        .unwrap();

    std::thread::sleep(Duration::from_millis(40));
    let before = count.load(Ordering::SeqCst);
    assert!(before > 0);

    s.suspend_process(pid).unwrap();
    std::thread::sleep(Duration::from_millis(40));
    let during = count.load(Ordering::SeqCst);

    s.resume_process(pid).unwrap();
    std::thread::sleep(Duration::from_millis(40));
    let after = count.load(Ordering::SeqCst);

    assert!(after > during);
    running.store(false, Ordering::SeqCst);
    s.kill_process(pid).unwrap();
}

#[test]
fn test_multiple_real_threads_parallel() {
    let total = Arc::new(AtomicUsize::new(0));

    let mut s = Scheduler::new(4096);
    for _ in 0..8 {
        let t = total.clone();
        s.spawn_real_process("par_worker", Priority::Normal, 32, move |_term, _susp| {
            t.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
    }

    assert_eq!(s.real_thread_count(), 8);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(total.load(Ordering::SeqCst), 8);
}

#[test]
fn test_real_thread_finished_detection() {
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();

    let mut s = Scheduler::new(1024);
    let pid = s
        .spawn_real_process("quick", Priority::Normal, 16, move |_term, _susp| {
            std::thread::sleep(Duration::from_millis(10));
            d.store(true, Ordering::SeqCst);
        })
        .unwrap();

    std::thread::sleep(Duration::from_millis(50));
    let finished = s.check_real_threads();
    assert!(finished.contains(&pid));
    assert!(done.load(Ordering::SeqCst));
}

#[test]
fn test_real_thread_ram_enforcement() {
    let mut s = Scheduler::new(100);
    s.spawn_process("big", Priority::Normal, 80).unwrap();

    let result = s.spawn_real_process("overflow", Priority::Normal, 30, |_t, _s| {});
    assert!(result.is_err());
}

#[test]
fn test_real_thread_kill_releases_ram() {
    let mut s = Scheduler::new(256);
    let pid = s
        .spawn_real_process("eater", Priority::Normal, 128, move |_term, _susp| {
            std::thread::sleep(Duration::from_secs(60));
        })
        .unwrap();

    assert_eq!(s.ram_usage(), (128, 256));
    s.kill_process(pid).unwrap();
    assert_eq!(s.ram_usage(), (0, 256));
}

#[test]
fn test_real_thread_with_priority_scheduling() {
    let mut s = Scheduler::new(4096);
    let _low = s.spawn_process("low_prio", Priority::Low, 8).unwrap();

    let high_running = Arc::new(AtomicBool::new(false));
    let hr = high_running.clone();
    let high_pid = s
        .spawn_real_process("high_real", Priority::High, 32, move |_term, _susp| {
            hr.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
        })
        .unwrap();

    std::thread::sleep(Duration::from_millis(30));
    assert!(high_running.load(Ordering::SeqCst));

    s.kill_process(high_pid).unwrap();
}

#[test]
fn test_real_thread_concurrent_data_race_free() {
    let counter = Arc::new(AtomicUsize::new(0));

    let mut s = Scheduler::new(4096);
    for _ in 0..4 {
        let c = counter.clone();
        s.spawn_real_process(
            "atomic_worker",
            Priority::Normal,
            32,
            move |_term, _susp| {
                for _ in 0..100 {
                    c.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .unwrap();
    }

    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(counter.load(Ordering::SeqCst), 400);
}

#[test]
fn test_mixed_real_and_logical_processes() {
    let mut s = Scheduler::new(2048);

    let log_pid = s.spawn_process("logical", Priority::Low, 64).unwrap();
    assert!(!s.is_real_process(log_pid));

    let real_pid = s
        .spawn_real_process("real", Priority::High, 64, |_t, _s| {})
        .unwrap();
    assert!(s.is_real_process(real_pid));

    assert_eq!(s.process_count(), 2);

    s.kill_process(real_pid).unwrap();
    assert_eq!(s.process_count(), 1);
    assert!(s.get_process(log_pid).is_some());
}
