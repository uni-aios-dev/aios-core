use crate::task::{Priority, Process, ProcessGroup, ProcessId, ProcessState, ProcessTimer};
use aios_core::error::{AIOSException, Result};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SchedulingMode {
    Normal,
    RealTime,
}

pub struct TerminateFlag(Arc<AtomicBool>);

impl TerminateFlag {
    pub fn should_stop(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn new() -> (Self, Arc<AtomicBool>) {
        let arc = Arc::new(AtomicBool::new(false));
        (Self(arc.clone()), arc)
    }
}

pub struct SuspendFlag(Arc<AtomicBool>);

impl SuspendFlag {
    pub fn is_suspended(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn new() -> (Self, Arc<AtomicBool>) {
        let arc = Arc::new(AtomicBool::new(false));
        (Self(arc.clone()), arc)
    }
}

struct RealThread {
    thread: std::thread::Thread,
    handle: Option<std::thread::JoinHandle<()>>,
    terminate: Arc<AtomicBool>,
    suspend: Arc<AtomicBool>,
    /// Desired CPU affinity, shared with the spawned thread so it can pin
    /// itself without affecting the scheduler thread.
    affinity: Arc<Mutex<Vec<usize>>>,
}

#[derive(Debug, Clone)]
pub struct RealThreadState {
    pub pid: ProcessId,
    pub finished: bool,
    pub suspended: bool,
    pub terminated: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JitterEntry {
    pub pid: ProcessId,
    pub expected_ms: u64,
    pub actual_ms: u64,
    pub timestamp: u64,
}

pub struct Scheduler {
    processes: HashMap<ProcessId, Process>,
    priority_queues: BTreeMap<Priority, Vec<ProcessId>>,
    current: Option<ProcessId>,
    timer: Option<ProcessTimer>,
    next_pid: u64,
    total_ram_mb: u64,
    used_ram_mb: u64,
    default_time_slice_ms: u64,
    max_crash_restarts: u32,
    crash_log: Vec<CrashEvent>,
    aging_threshold_ms: u64,
    last_scheduled_ms: HashMap<ProcessId, u64>,
    round_robin_positions: HashMap<Priority, usize>,
    memory_pressure_threshold: f64,
    memory_pressure_callbacks: Vec<String>,
    groups: HashMap<u64, ProcessGroup>,
    next_group_id: u64,
    scheduling_mode: SchedulingMode,
    rt_deadlines: HashMap<ProcessId, u64>,
    jitter_log: Vec<JitterEntry>,
    max_jitter_entries: usize,
    real_threads: HashMap<ProcessId, RealThread>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CrashEvent {
    pub pid: ProcessId,
    pub name: String,
    pub timestamp: u64,
    pub crash_count: u32,
}

impl Scheduler {
    pub fn new(total_ram_mb: u64) -> Self {
        Self {
            processes: HashMap::new(),
            priority_queues: BTreeMap::new(),
            current: None,
            timer: None,
            next_pid: 1,
            total_ram_mb,
            used_ram_mb: 0,
            default_time_slice_ms: 50,
            max_crash_restarts: 3,
            crash_log: Vec::new(),
            aging_threshold_ms: 500,
            last_scheduled_ms: HashMap::new(),
            round_robin_positions: HashMap::new(),
            memory_pressure_threshold: 0.8,
            memory_pressure_callbacks: Vec::new(),
            groups: HashMap::new(),
            next_group_id: 1,
            scheduling_mode: SchedulingMode::Normal,
            rt_deadlines: HashMap::new(),
            jitter_log: Vec::new(),
            max_jitter_entries: 1000,
            real_threads: HashMap::new(),
        }
    }

    pub fn with_aging_threshold(mut self, ms: u64) -> Self {
        self.aging_threshold_ms = ms;
        self
    }

    pub fn with_time_slice(mut self, ms: u64) -> Self {
        self.default_time_slice_ms = ms;
        self
    }

    pub fn with_max_restarts(mut self, max: u32) -> Self {
        self.max_crash_restarts = max;
        self
    }

    pub fn with_memory_pressure_threshold(mut self, threshold: f64) -> Self {
        self.memory_pressure_threshold = threshold;
        self
    }

    pub fn force_preempt(&mut self) {
        if let Some(timer) = &mut self.timer {
            timer.force_expire();
        }
        if let Some(current) = self.current {
            if let Some(proc) = self.processes.get_mut(&current) {
                proc.cpu_time_ms += self.timer.as_ref().map(|t| t.elapsed_ms()).unwrap_or(0);
                proc.state = ProcessState::Ready;
            }
            self.last_scheduled_ms.insert(current, now_ms());
            self.current = None;
            self.timer = None;
        }
    }

    pub fn set_scheduling_mode(&mut self, mode: SchedulingMode) {
        self.scheduling_mode = mode;
    }

    pub fn scheduling_mode(&self) -> SchedulingMode {
        self.scheduling_mode
    }

    pub fn set_rt_deadline(&mut self, pid: ProcessId, deadline_ms: u64) {
        self.rt_deadlines.insert(pid, deadline_ms);
    }

    pub fn clear_rt_deadline(&mut self, pid: ProcessId) {
        self.rt_deadlines.remove(&pid);
    }

    pub fn rt_deadlines(&self) -> &HashMap<ProcessId, u64> {
        &self.rt_deadlines
    }

    pub fn jitter_log(&self) -> &[JitterEntry] {
        &self.jitter_log
    }

    pub fn clear_jitter_log(&mut self) {
        self.jitter_log.clear();
    }

    fn record_jitter(&mut self, pid: ProcessId, expected_ms: u64, actual_ms: u64) {
        self.jitter_log.push(JitterEntry {
            pid,
            expected_ms,
            actual_ms,
            timestamp: now_ms(),
        });
        if self.jitter_log.len() > self.max_jitter_entries {
            self.jitter_log.remove(0);
        }
    }

    fn schedule_next_rt(&mut self) -> Option<ProcessId> {
        let now = now_ms();
        let mut best: Option<(u64, ProcessId)> = None;

        for (pid, proc) in self.processes.iter() {
            if proc.state != ProcessState::Ready {
                continue;
            }
            let deadline = self.rt_deadlines.get(pid).copied().unwrap_or(u64::MAX);
            let remaining = deadline.saturating_sub(now);
            if best.is_none() || remaining < best.as_ref().unwrap().0 {
                best = Some((remaining, *pid));
            }
        }

        if let Some((_remaining, pid)) = best {
            if let Some(old) = self.current {
                if old != pid {
                    if let Some(old_proc) = self.processes.get_mut(&old) {
                        old_proc.state = ProcessState::Ready;
                    }
                }
            }

            let proc = self.processes.get_mut(&pid).unwrap();
            proc.state = ProcessState::Running;
            self.current = Some(pid);
            let time_slice = self.default_time_slice_ms;
            self.timer = Some(ProcessTimer::new(pid, time_slice));

            if let Some(&deadline) = self.rt_deadlines.get(&pid) {
                let expected = self.default_time_slice_ms;
                let actual =
                    now.saturating_sub(self.last_scheduled_ms.get(&pid).copied().unwrap_or(now));
                if actual > expected {
                    self.record_jitter(pid, expected, actual);
                }
                if now >= deadline {
                    self.record_jitter(pid, expected, actual);
                }
            }

            self.last_scheduled_ms.insert(pid, now);
            Some(pid)
        } else {
            self.current = None;
            self.timer = None;
            None
        }
    }

    pub fn set_last_scheduled(&mut self, pid: ProcessId, ms: u64) {
        self.last_scheduled_ms.insert(pid, ms);
    }

    pub fn is_scheduled(&self) -> bool {
        self.current.is_some()
    }

    pub fn spawn_process(
        &mut self,
        name: &str,
        priority: Priority,
        ram_mb: u64,
    ) -> Result<ProcessId> {
        if self.used_ram_mb + ram_mb > self.total_ram_mb {
            return Err(AIOSException::SchedulerError(format!(
                "Insufficient RAM: requested {}MB, available {}MB",
                ram_mb,
                self.total_ram_mb - self.used_ram_mb
            )));
        }

        let pid = ProcessId::new(self.next_pid);
        self.next_pid += 1;

        let process = Process::new(pid, name.to_string(), priority, ram_mb)
            .with_max_restarts(self.max_crash_restarts);
        self.processes.insert(pid, process);
        self.priority_queues.entry(priority).or_default().push(pid);
        self.used_ram_mb += ram_mb;
        self.last_scheduled_ms.insert(pid, now_ms());

        log::info!("Scheduler: Spawned {} ({}) with {}MB", pid, name, ram_mb);
        Ok(pid)
    }

    pub fn spawn_real_process<F>(
        &mut self,
        name: &str,
        priority: Priority,
        ram_mb: u64,
        f: F,
    ) -> Result<ProcessId>
    where
        F: FnOnce(TerminateFlag, SuspendFlag) + Send + 'static,
    {
        if self.used_ram_mb + ram_mb > self.total_ram_mb {
            return Err(AIOSException::SchedulerError(format!(
                "Insufficient RAM: requested {}MB, available {}MB",
                ram_mb,
                self.total_ram_mb - self.used_ram_mb
            )));
        }

        let pid = ProcessId::new(self.next_pid);
        self.next_pid += 1;

        let (term_flag, term_arc) = TerminateFlag::new();
        let (_susp_flag, susp_arc) = SuspendFlag::new();

        let thread_name = format!("aios-{}-{}", name, pid.0);
        let builder = std::thread::Builder::new().name(thread_name.clone());

        let suspend_check = susp_arc.clone();
        let term_for_thread = term_arc.clone();
        let susp_for_thread = susp_arc.clone();
        let affinity_slot: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let affinity_for_thread = affinity_slot.clone();
        let handle = builder
            .spawn(move || loop {
                if term_flag.should_stop() {
                    break;
                }
                if suspend_check.load(Ordering::Relaxed) {
                    std::thread::park();
                    continue;
                }
                // Apply the requested CPU affinity to *this* thread before the
                // payload runs (the OS call targets the current thread).
                if let Ok(cores) = affinity_for_thread.lock() {
                    if !cores.is_empty() {
                        let _ = crate::cpu_affinity::set_current_thread_affinity(&cores);
                    }
                }
                f(
                    TerminateFlag(term_for_thread.clone()),
                    SuspendFlag(susp_for_thread.clone()),
                );
                break;
            })
            .map_err(|e| AIOSException::SchedulerError(format!("Failed to spawn thread: {}", e)))?;

        let thread_ref = handle.thread().clone();

        let process = Process::new(pid, name.to_string(), priority, ram_mb)
            .with_max_restarts(self.max_crash_restarts);
        self.processes.insert(pid, process);
        self.priority_queues.entry(priority).or_default().push(pid);
        self.used_ram_mb += ram_mb;
        self.last_scheduled_ms.insert(pid, now_ms());

        self.real_threads.insert(
            pid,
            RealThread {
                thread: thread_ref,
                handle: Some(handle),
                terminate: term_arc,
                suspend: susp_arc,
                affinity: affinity_slot,
            },
        );

        log::info!(
            "Scheduler: Spawned real process {} ({}) with {}MB on OS thread",
            pid,
            name,
            ram_mb
        );
        Ok(pid)
    }

    pub fn spawn_child(
        &mut self,
        parent_pid: ProcessId,
        name: &str,
        priority: Priority,
        ram_mb: u64,
    ) -> Result<ProcessId> {
        if self.used_ram_mb + ram_mb > self.total_ram_mb {
            return Err(AIOSException::SchedulerError(format!(
                "Insufficient RAM: requested {}MB, available {}MB",
                ram_mb,
                self.total_ram_mb - self.used_ram_mb
            )));
        }

        let pid = ProcessId::new(self.next_pid);
        self.next_pid += 1;

        let process = Process::new(pid, name.to_string(), priority, ram_mb)
            .with_parent(parent_pid)
            .with_max_restarts(self.max_crash_restarts);
        self.processes.insert(pid, process);
        self.priority_queues.entry(priority).or_default().push(pid);
        self.used_ram_mb += ram_mb;

        log::info!(
            "Scheduler: Spawned child {} ({}) of parent {}",
            pid,
            name,
            parent_pid
        );
        Ok(pid)
    }

    pub fn kill_process(&mut self, pid: ProcessId) -> Result<Process> {
        if let Some(mut real) = self.real_threads.remove(&pid) {
            real.terminate.store(true, Ordering::Relaxed);
            real.suspend.store(false, Ordering::Relaxed);
            real.thread.unpark();
            if let Some(handle) = real.handle.take() {
                let _ = handle.join();
            }
        }

        let mut process = self
            .processes
            .remove(&pid)
            .ok_or(AIOSException::ProcessNotFound(pid.0))?;

        process.state = ProcessState::Terminated;
        self.used_ram_mb = self.used_ram_mb.saturating_sub(process.ram_quota_mb);

        if let Some(queue) = self.priority_queues.get_mut(&process.priority) {
            queue.retain(|&p| p != pid);
            if queue.is_empty() {
                self.priority_queues.remove(&process.priority);
            }
        }

        if self.current == Some(pid) {
            self.current = None;
            self.timer = None;
        }

        log::info!("Scheduler: Killed {} ({})", pid, process.name);
        Ok(process)
    }

    pub fn set_priority(&mut self, pid: ProcessId, new_priority: Priority) -> Result<()> {
        let process = self
            .processes
            .get_mut(&pid)
            .ok_or(AIOSException::ProcessNotFound(pid.0))?;

        let old_priority = process.priority;
        if old_priority == new_priority {
            return Ok(());
        }

        process.priority = new_priority;

        if let Some(queue) = self.priority_queues.get_mut(&old_priority) {
            queue.retain(|&p| p != pid);
            if queue.is_empty() {
                self.priority_queues.remove(&old_priority);
            }
        }
        self.priority_queues
            .entry(new_priority)
            .or_default()
            .push(pid);

        log::info!(
            "Scheduler: {} priority {} → {}",
            pid,
            old_priority,
            new_priority
        );
        Ok(())
    }

    pub fn suspend_process(&mut self, pid: ProcessId) -> Result<()> {
        let process = self
            .processes
            .get_mut(&pid)
            .ok_or(AIOSException::ProcessNotFound(pid.0))?;

        process.state = ProcessState::Suspended;
        if self.current == Some(pid) {
            self.current = None;
            self.timer = None;
        }

        if let Some(real) = self.real_threads.get(&pid) {
            real.suspend.store(true, Ordering::Relaxed);
            real.thread.unpark();
        }

        log::info!("Scheduler: Suspended {}", pid);
        Ok(())
    }

    pub fn resume_process(&mut self, pid: ProcessId) -> Result<()> {
        let process = self
            .processes
            .get_mut(&pid)
            .ok_or(AIOSException::ProcessNotFound(pid.0))?;

        if process.state == ProcessState::Suspended {
            process.state = ProcessState::Ready;
        }

        if let Some(real) = self.real_threads.get(&pid) {
            real.suspend.store(false, Ordering::Relaxed);
            real.thread.unpark();
        }

        log::info!("Scheduler: Resumed {}", pid);
        Ok(())
    }

    pub fn report_crash(&mut self, pid: ProcessId) -> Result<CrashEvent> {
        let process = self
            .processes
            .get_mut(&pid)
            .ok_or(AIOSException::ProcessNotFound(pid.0))?;

        process.crash_count += 1;
        process.state = ProcessState::Crashed;

        let event = CrashEvent {
            pid,
            name: process.name.clone(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            crash_count: process.crash_count,
        };

        log::warn!(
            "Scheduler: Process {} crashed (count={}/{})",
            pid,
            process.crash_count,
            process.max_restarts
        );

        self.crash_log.push(event.clone());

        if process.crash_count >= process.max_restarts {
            log::error!(
                "Scheduler: Process {} exceeded max restarts ({}), will not auto-restart",
                pid,
                process.max_restarts
            );
        }

        Ok(event)
    }

    pub fn should_restart(&self, pid: ProcessId) -> bool {
        self.processes
            .get(&pid)
            .map(|p| p.state == ProcessState::Crashed && p.crash_count < p.max_restarts)
            .unwrap_or(false)
    }

    pub fn schedule_next(&mut self) -> Option<ProcessId> {
        if self.scheduling_mode == SchedulingMode::RealTime {
            return self.schedule_next_rt();
        }

        if let Some(current) = self.current {
            if let Some(timer) = &self.timer {
                if !timer.quota_exceeded() {
                    return Some(current);
                }
            }
            if let Some(proc) = self.processes.get_mut(&current) {
                proc.cpu_time_ms += self.timer.as_ref().map(|t| t.elapsed_ms()).unwrap_or(0);
                proc.state = ProcessState::Ready;
            }
            self.last_scheduled_ms.insert(current, now_ms());
        }

        let skip = self.current;
        let mut candidate: Option<(Priority, u32, ProcessId)> = None;

        for (priority, queue) in self.priority_queues.iter().rev() {
            if queue.is_empty() {
                continue;
            }
            let rr_pos = self
                .round_robin_positions
                .get(priority)
                .copied()
                .unwrap_or(0);
            let len = queue.len();
            for i in 0..len {
                let idx = (rr_pos + i) % len;
                let pid = queue[idx];
                if Some(pid) == skip {
                    continue;
                }
                if let Some(proc) = self.processes.get(&pid) {
                    if proc.state == ProcessState::Ready {
                        let mut effective = *priority;
                        if let Some(&last_ms) = self.last_scheduled_ms.get(&pid) {
                            let wait = now_ms().saturating_sub(last_ms);
                            let boost = (wait / self.aging_threshold_ms).min(4) as u8;
                            effective = Priority::from_u8((effective as u8) + boost);
                        }
                        let weight = Self::priority_weight(effective);
                        if candidate.is_none()
                            || effective > candidate.as_ref().unwrap().0
                            || (effective == candidate.as_ref().unwrap().0
                                && weight > candidate.as_ref().unwrap().1)
                        {
                            candidate = Some((effective, weight, pid));
                        }
                    }
                }
            }
        }

        if let Some((_priority, _weight, pid)) = candidate {
            if let Some(old) = self.current {
                if let Some(old_proc) = self.processes.get_mut(&old) {
                    old_proc.state = ProcessState::Ready;
                }
            }

            if let Some(proc) = self.processes.get(&pid) {
                let prio = proc.priority;
                if let Some(queue) = self.priority_queues.get(&prio) {
                    if let Some(pos) = queue.iter().position(|&p| p == pid) {
                        let new_pos = (pos + 1) % queue.len();
                        self.round_robin_positions.insert(prio, new_pos);
                    }
                }
            }

            let proc = self.processes.get_mut(&pid).unwrap();
            let weight = Self::priority_weight(proc.priority);
            let time_slice = self.default_time_slice_ms * weight as u64;
            proc.state = ProcessState::Running;
            self.current = Some(pid);
            self.timer = Some(ProcessTimer::new(pid, time_slice));

            return Some(pid);
        }

        self.current = None;
        self.timer = None;
        None
    }

    pub fn tick(&mut self) -> Option<ProcessId> {
        if let Some(timer) = &self.timer {
            if timer.quota_exceeded() {
                if let Some(current) = self.current {
                    if let Some(proc) = self.processes.get_mut(&current) {
                        proc.cpu_time_ms += timer.elapsed_ms();
                        proc.state = ProcessState::Ready;
                    }
                    self.current = None;
                    self.timer = None;
                }
            }
        }
        self.schedule_next()
    }

    pub fn get_process(&self, pid: ProcessId) -> Option<&Process> {
        self.processes.get(&pid)
    }

    pub fn all_processes(&self) -> Vec<&Process> {
        self.processes.values().collect()
    }

    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    pub fn ram_usage(&self) -> (u64, u64) {
        (self.used_ram_mb, self.total_ram_mb)
    }

    pub fn crash_log(&self) -> &[CrashEvent] {
        &self.crash_log
    }

    pub fn running_count(&self) -> usize {
        self.processes
            .values()
            .filter(|p| p.state == ProcessState::Running)
            .count()
    }

    pub fn ready_count(&self) -> usize {
        self.processes
            .values()
            .filter(|p| p.state == ProcessState::Ready)
            .count()
    }

    pub fn priority_weight(priority: Priority) -> u32 {
        match priority {
            Priority::Background => 1,
            Priority::Low => 2,
            Priority::Normal => 3,
            Priority::High => 4,
            Priority::Critical => 5,
        }
    }

    pub fn memory_pressure(&self) -> MemoryPressure {
        let usage = self.used_ram_mb as f64 / self.total_ram_mb.max(1) as f64;
        if usage >= self.memory_pressure_threshold {
            MemoryPressure::Critical(usage)
        } else if usage >= self.memory_pressure_threshold * 0.75 {
            MemoryPressure::Warning(usage)
        } else {
            MemoryPressure::Normal(usage)
        }
    }

    pub fn memory_pressure_callbacks(&self) -> &[String] {
        &self.memory_pressure_callbacks
    }

    pub fn register_memory_pressure_callback(&mut self, name: String) {
        if !self.memory_pressure_callbacks.contains(&name) {
            self.memory_pressure_callbacks.push(name);
        }
    }

    pub fn check_memory_pressure(&self) -> Option<MemoryPressureEvent> {
        match self.memory_pressure() {
            MemoryPressure::Critical(usage) => Some(MemoryPressureEvent {
                level: PressureLevel::Critical,
                usage,
                used_mb: self.used_ram_mb,
                total_mb: self.total_ram_mb,
                callbacks: self.memory_pressure_callbacks.clone(),
            }),
            MemoryPressure::Warning(usage) => Some(MemoryPressureEvent {
                level: PressureLevel::Warning,
                usage,
                used_mb: self.used_ram_mb,
                total_mb: self.total_ram_mb,
                callbacks: self.memory_pressure_callbacks.clone(),
            }),
            _ => None,
        }
    }

    pub fn create_group(&mut self, name: String, priority: Priority) -> u64 {
        let id = self.next_group_id;
        self.next_group_id += 1;
        let group = ProcessGroup::new(id, name, priority);
        self.groups.insert(id, group);
        id
    }

    pub fn create_session(&mut self, name: String, priority: Priority) -> u64 {
        let group_id = self.next_group_id;
        self.next_group_id += 1;
        let session_id = group_id;
        let mut group = ProcessGroup::new(group_id, name, priority);
        group.session_id = Some(session_id);
        self.groups.insert(group_id, group);
        session_id
    }

    pub fn add_to_group(&mut self, pid: ProcessId, group_id: u64) -> Result<()> {
        let group = self.groups.get_mut(&group_id).ok_or_else(|| {
            AIOSException::SchedulerError(format!("Group {} not found", group_id))
        })?;
        group.add_member(pid);

        if let Some(proc) = self.processes.get_mut(&pid) {
            proc.group_id = Some(group_id);
        }
        Ok(())
    }

    pub fn remove_from_group(&mut self, pid: ProcessId) -> Result<()> {
        let group_id = self
            .processes
            .get(&pid)
            .and_then(|p| p.group_id)
            .ok_or_else(|| {
                AIOSException::SchedulerError(format!("Process {} has no group", pid.0))
            })?;

        if let Some(group) = self.groups.get_mut(&group_id) {
            group.remove_member(pid);
        }

        if let Some(proc) = self.processes.get_mut(&pid) {
            proc.group_id = None;
        }
        Ok(())
    }

    pub fn get_group(&self, group_id: u64) -> Option<&ProcessGroup> {
        self.groups.get(&group_id)
    }

    pub fn all_groups(&self) -> Vec<&ProcessGroup> {
        self.groups.values().collect()
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn group_members(&self, group_id: u64) -> Vec<&Process> {
        if let Some(group) = self.groups.get(&group_id) {
            group
                .member_pids
                .iter()
                .filter_map(|pid| self.processes.get(pid))
                .collect()
        } else {
            Vec::new()
        }
    }

    pub fn is_real_process(&self, pid: ProcessId) -> bool {
        self.real_threads.contains_key(&pid)
    }

    pub fn real_thread_count(&self) -> usize {
        self.real_threads.len()
    }

    pub fn get_real_thread_state(&self, pid: ProcessId) -> Option<RealThreadState> {
        self.real_threads.get(&pid).map(|rt| {
            let finished = rt.handle.as_ref().is_some_and(|h| h.is_finished());
            let suspended = rt.suspend.load(Ordering::Relaxed);
            let terminated = rt.terminate.load(Ordering::Relaxed);
            RealThreadState {
                pid,
                finished,
                suspended,
                terminated,
            }
        })
    }

    pub fn list_real_threads(&self) -> Vec<ProcessId> {
        self.real_threads.keys().copied().collect()
    }

    pub fn check_real_threads(&mut self) -> Vec<ProcessId> {
        let mut finished = Vec::new();
        for (pid, real) in &self.real_threads {
            if let Some(handle) = &real.handle {
                if handle.is_finished() {
                    finished.push(*pid);
                }
            }
        }
        for pid in &finished {
            if let Some(mut real) = self.real_threads.remove(pid) {
                if let Some(handle) = real.handle.take() {
                    let _ = handle.join();
                }
            }
            if let Some(proc) = self.processes.get_mut(pid) {
                if proc.state != ProcessState::Suspended {
                    proc.state = ProcessState::Ready;
                }
            }
        }
        finished
    }

    pub fn set_cpu_affinity(&mut self, pid: ProcessId, cores: &[usize]) -> Result<()> {
        crate::cpu_affinity::validate_cores(cores)?;

        let affinity_slot = {
            let real = self.real_threads.get(&pid).ok_or_else(|| {
                AIOSException::SchedulerError(format!("Process {} is not a real thread", pid.0))
            })?;
            real.affinity.clone()
        };

        {
            let mut guard = affinity_slot
                .lock()
                .map_err(|_| AIOSException::SchedulerError("Affinity lock poisoned".into()))?;
            *guard = cores.to_vec();
        }

        log::info!(
            "Scheduler: Set CPU affinity for {} to cores {:?}",
            pid,
            cores
        );
        Ok(())
    }

    pub fn get_cpu_affinity(&self, pid: ProcessId) -> Option<Vec<usize>> {
        let real = self.real_threads.get(&pid)?;
        let guard = real.affinity.lock().ok()?;
        if guard.is_empty() {
            None
        } else {
            Some(guard.clone())
        }
    }

    pub fn available_cpu_cores() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }

    pub fn kill_group(&mut self, group_id: u64) -> Result<Vec<ProcessId>> {
        let group = self.groups.get(&group_id).ok_or_else(|| {
            AIOSException::SchedulerError(format!("Group {} not found", group_id))
        })?;

        let pids: Vec<ProcessId> = group.member_pids.clone();

        for &pid in &pids {
            let _ = self.kill_process(pid);
        }

        self.groups.remove(&group_id);

        Ok(pids)
    }

    pub fn suspend_group(&mut self, group_id: u64) -> Result<Vec<ProcessId>> {
        let group = self.groups.get(&group_id).ok_or_else(|| {
            AIOSException::SchedulerError(format!("Group {} not found", group_id))
        })?;

        let pids: Vec<ProcessId> = group.member_pids.clone();
        let mut suspended = Vec::new();

        for &pid in &pids {
            if let Some(proc) = self.processes.get_mut(&pid) {
                if proc.state == ProcessState::Running || proc.state == ProcessState::Ready {
                    proc.state = ProcessState::Suspended;
                    suspended.push(pid);
                }
            }
        }

        Ok(suspended)
    }

    pub fn resume_group(&mut self, group_id: u64) -> Result<Vec<ProcessId>> {
        let group = self.groups.get(&group_id).ok_or_else(|| {
            AIOSException::SchedulerError(format!("Group {} not found", group_id))
        })?;

        let pids: Vec<ProcessId> = group.member_pids.clone();
        let mut resumed = Vec::new();

        for &pid in &pids {
            if let Some(proc) = self.processes.get_mut(&pid) {
                if proc.state == ProcessState::Suspended {
                    proc.state = ProcessState::Ready;
                    resumed.push(pid);
                }
            }
        }

        Ok(resumed)
    }

    pub fn set_group_priority(&mut self, group_id: u64, priority: Priority) -> Result<()> {
        let group = self.groups.get_mut(&group_id).ok_or_else(|| {
            AIOSException::SchedulerError(format!("Group {} not found", group_id))
        })?;

        group.priority = priority;

        let pids: Vec<ProcessId> = group.member_pids.clone();
        for &pid in &pids {
            self.set_priority(pid, priority)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemoryPressure {
    Normal(f64),
    Warning(f64),
    Critical(f64),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryPressureEvent {
    pub level: PressureLevel,
    pub usage: f64,
    pub used_mb: u64,
    pub total_mb: u64,
    pub callbacks: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PressureLevel {
    Normal,
    Warning,
    Critical,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_scheduler() -> Scheduler {
        Scheduler::new(256).with_time_slice(10)
    }

    #[test]
    fn test_spawn_and_kill() {
        let mut s = make_scheduler();
        let pid = s.spawn_process("worker", Priority::Normal, 32).unwrap();
        assert_eq!(s.process_count(), 1);
        assert_eq!(s.get_process(pid).unwrap().name, "worker");

        let killed = s.kill_process(pid).unwrap();
        assert_eq!(killed.name, "worker");
        assert_eq!(killed.state, ProcessState::Terminated);
        assert_eq!(s.process_count(), 0);
        assert_eq!(s.ram_usage(), (0, 256));
    }

    #[test]
    fn test_ram_limit() {
        let mut s = Scheduler::new(100);
        s.spawn_process("a", Priority::Normal, 60).unwrap();
        s.spawn_process("b", Priority::Normal, 30).unwrap();
        assert_eq!(s.ram_usage(), (90, 100));
        assert!(s.spawn_process("c", Priority::Normal, 20).is_err());
    }

    #[test]
    fn test_priority_scheduling() {
        let mut s = make_scheduler();
        let low = s.spawn_process("low", Priority::Low, 8).unwrap();
        let high = s.spawn_process("high", Priority::High, 8).unwrap();
        let _bg = s.spawn_process("bg", Priority::Background, 8).unwrap();

        let scheduled = s.schedule_next().unwrap();
        assert_eq!(scheduled, high);

        s.kill_process(high).unwrap();
        let next = s.schedule_next().unwrap();
        assert_eq!(next, low);
    }

    #[test]
    fn test_priority_change() {
        let mut s = make_scheduler();
        let pid = s.spawn_process("task", Priority::Low, 8).unwrap();
        s.set_priority(pid, Priority::Critical).unwrap();
        assert_eq!(s.get_process(pid).unwrap().priority, Priority::Critical);
    }

    #[test]
    fn test_suspend_resume() {
        let mut s = make_scheduler();
        let pid = s.spawn_process("task", Priority::Normal, 8).unwrap();
        s.suspend_process(pid).unwrap();
        assert_eq!(s.get_process(pid).unwrap().state, ProcessState::Suspended);
        s.resume_process(pid).unwrap();
        assert_eq!(s.get_process(pid).unwrap().state, ProcessState::Ready);
    }

    #[test]
    fn test_crash_and_restart() {
        let mut s = make_scheduler().with_max_restarts(2);
        let pid = s.spawn_process("fragile", Priority::Normal, 8).unwrap();

        let event = s.report_crash(pid).unwrap();
        assert_eq!(event.crash_count, 1);
        assert!(s.should_restart(pid));

        let event = s.report_crash(pid).unwrap();
        assert_eq!(event.crash_count, 2);
        assert!(!s.should_restart(pid));

        assert_eq!(s.crash_log().len(), 2);
    }

    #[test]
    fn test_child_process() {
        let mut s = make_scheduler();
        let parent = s.spawn_process("parent", Priority::Normal, 32).unwrap();
        let child = s
            .spawn_child(parent, "child", Priority::Normal, 16)
            .unwrap();
        assert_eq!(s.get_process(child).unwrap().parent_pid, Some(parent));
        assert_eq!(s.process_count(), 2);
    }

    #[test]
    fn test_aging_boosts_low_priority() {
        let mut s = Scheduler::new(8192).with_aging_threshold(100);
        let low = s.spawn_process("low", Priority::Low, 8).unwrap();
        let high = s.spawn_process("high", Priority::High, 8).unwrap();

        let first = s.schedule_next().unwrap();
        assert_eq!(first, high);

        s.last_scheduled_ms
            .insert(low, now_ms().saturating_sub(600));

        if let Some(timer) = &mut s.timer {
            timer.force_expire();
        }
        if let Some(current) = s.current {
            if let Some(proc) = s.processes.get_mut(&current) {
                proc.cpu_time_ms += 50;
                proc.state = ProcessState::Ready;
            }
            s.current = None;
            s.timer = None;
        }

        let next = s.schedule_next().unwrap();
        assert_eq!(next, low);
    }

    #[test]
    fn test_priority_weight() {
        assert_eq!(Scheduler::priority_weight(Priority::Background), 1);
        assert_eq!(Scheduler::priority_weight(Priority::Low), 2);
        assert_eq!(Scheduler::priority_weight(Priority::Normal), 3);
        assert_eq!(Scheduler::priority_weight(Priority::High), 4);
        assert_eq!(Scheduler::priority_weight(Priority::Critical), 5);
    }

    #[test]
    fn test_weighted_time_slice() {
        let mut s = Scheduler::new(256).with_time_slice(10);
        let a = s.spawn_process("a", Priority::Normal, 8).unwrap();
        let b = s.spawn_process("b", Priority::Normal, 8).unwrap();

        let first = s.schedule_next().unwrap();
        assert_eq!(first, a);
        let timer = s.timer.as_ref().unwrap();
        assert_eq!(timer.quota_ms, 30);

        s.force_preempt();
        let second = s.schedule_next().unwrap();
        assert_eq!(second, b);
        let timer = s.timer.as_ref().unwrap();
        assert_eq!(timer.quota_ms, 30);
    }

    #[test]
    fn test_weighted_time_slice_cross_priority() {
        let mut s = Scheduler::new(256).with_time_slice(10);
        let bg = s.spawn_process("bg", Priority::Background, 8).unwrap();
        let crit = s.spawn_process("crit", Priority::Critical, 8).unwrap();

        let first = s.schedule_next().unwrap();
        assert_eq!(first, crit);
        let timer = s.timer.as_ref().unwrap();
        assert_eq!(timer.quota_ms, 50);

        s.kill_process(crit).unwrap();
        let second = s.schedule_next().unwrap();
        assert_eq!(second, bg);
        let timer = s.timer.as_ref().unwrap();
        assert_eq!(timer.quota_ms, 10);
    }

    #[test]
    fn test_round_robin_within_priority() {
        let mut s = Scheduler::new(256).with_time_slice(10);
        let a = s.spawn_process("a", Priority::Normal, 8).unwrap();
        let b = s.spawn_process("b", Priority::Normal, 8).unwrap();
        let c = s.spawn_process("c", Priority::Normal, 8).unwrap();

        let first = s.schedule_next().unwrap();
        s.force_preempt();
        let second = s.schedule_next().unwrap();
        s.force_preempt();
        let third = s.schedule_next().unwrap();

        assert_eq!(first, a);
        assert_eq!(second, b);
        assert_eq!(third, c);
    }

    #[test]
    fn test_memory_pressure_normal() {
        let mut s = Scheduler::new(1000);
        let _ = s.spawn_process("a", Priority::Normal, 100);
        assert_eq!(s.memory_pressure(), MemoryPressure::Normal(0.1));
        assert!(s.check_memory_pressure().is_none());
    }

    #[test]
    fn test_memory_pressure_warning() {
        let mut s = Scheduler::new(100).with_memory_pressure_threshold(0.8);
        let _ = s.spawn_process("a", Priority::Normal, 65);
        assert!(matches!(s.memory_pressure(), MemoryPressure::Warning(_)));
        let event = s.check_memory_pressure().unwrap();
        assert_eq!(event.level, PressureLevel::Warning);
    }

    #[test]
    fn test_memory_pressure_critical() {
        let mut s = Scheduler::new(100).with_memory_pressure_threshold(0.8);
        let _ = s.spawn_process("a", Priority::Normal, 85);
        assert!(matches!(s.memory_pressure(), MemoryPressure::Critical(_)));
        let event = s.check_memory_pressure().unwrap();
        assert_eq!(event.level, PressureLevel::Critical);
        assert_eq!(event.callbacks.len(), 0);
    }

    #[test]
    fn test_memory_pressure_callback() {
        let mut s = Scheduler::new(100).with_memory_pressure_threshold(0.8);
        s.register_memory_pressure_callback("ai_orchestrator".into());
        let _ = s.spawn_process("a", Priority::Normal, 85);
        let event = s.check_memory_pressure().unwrap();
        assert_eq!(event.callbacks, vec!["ai_orchestrator"]);
        assert_eq!(s.memory_pressure_callbacks().len(), 1);
    }

    #[test]
    fn test_create_group() {
        let mut s = make_scheduler();
        let gid = s.create_group("workers".into(), Priority::Normal);
        assert_eq!(gid, 1);
        assert_eq!(s.group_count(), 1);
        assert_eq!(s.get_group(gid).unwrap().name, "workers");
    }

    #[test]
    fn test_create_session() {
        let mut s = make_scheduler();
        let sid = s.create_session("session_a".into(), Priority::High);
        assert_eq!(s.group_count(), 1);
        let group = s.get_group(sid).unwrap();
        assert_eq!(group.session_id, Some(sid));
    }

    #[test]
    fn test_add_to_group() {
        let mut s = make_scheduler();
        let pid = s.spawn_process("worker1", Priority::Normal, 32).unwrap();
        let gid = s.create_group("team".into(), Priority::Normal);
        s.add_to_group(pid, gid).unwrap();
        assert_eq!(s.get_group(gid).unwrap().member_count(), 1);
        assert!(s.get_group(gid).unwrap().contains(pid));
        assert_eq!(s.get_process(pid).unwrap().group_id, Some(gid));
    }

    #[test]
    fn test_add_to_group_not_found() {
        let mut s = make_scheduler();
        let pid = s.spawn_process("worker", Priority::Normal, 32).unwrap();
        let result = s.add_to_group(pid, 999);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_from_group() {
        let mut s = make_scheduler();
        let pid = s.spawn_process("worker1", Priority::Normal, 32).unwrap();
        let gid = s.create_group("team".into(), Priority::Normal);
        s.add_to_group(pid, gid).unwrap();
        s.remove_from_group(pid).unwrap();
        assert_eq!(s.get_group(gid).unwrap().member_count(), 0);
        assert!(s.get_process(pid).unwrap().group_id.is_none());
    }

    #[test]
    fn test_group_members() {
        let mut s = make_scheduler();
        let p1 = s.spawn_process("w1", Priority::Normal, 16).unwrap();
        let p2 = s.spawn_process("w2", Priority::Normal, 16).unwrap();
        let gid = s.create_group("team".into(), Priority::Normal);
        s.add_to_group(p1, gid).unwrap();
        s.add_to_group(p2, gid).unwrap();
        let members = s.group_members(gid);
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn test_kill_group() {
        let mut s = make_scheduler();
        let p1 = s.spawn_process("w1", Priority::Normal, 16).unwrap();
        let p2 = s.spawn_process("w2", Priority::Normal, 16).unwrap();
        let gid = s.create_group("team".into(), Priority::Normal);
        s.add_to_group(p1, gid).unwrap();
        s.add_to_group(p2, gid).unwrap();
        let killed = s.kill_group(gid).unwrap();
        assert_eq!(killed.len(), 2);
        assert!(s.get_process(p1).is_none());
        assert!(s.get_process(p2).is_none());
        assert_eq!(s.group_count(), 0);
    }

    #[test]
    fn test_suspend_group() {
        let mut s = make_scheduler();
        let p1 = s.spawn_process("w1", Priority::Normal, 16).unwrap();
        let p2 = s.spawn_process("w2", Priority::Normal, 16).unwrap();
        let gid = s.create_group("team".into(), Priority::Normal);
        s.add_to_group(p1, gid).unwrap();
        s.add_to_group(p2, gid).unwrap();
        let suspended = s.suspend_group(gid).unwrap();
        assert_eq!(suspended.len(), 2);
        assert_eq!(s.get_process(p1).unwrap().state, ProcessState::Suspended);
        assert_eq!(s.get_process(p2).unwrap().state, ProcessState::Suspended);
    }

    #[test]
    fn test_resume_group() {
        let mut s = make_scheduler();
        let p1 = s.spawn_process("w1", Priority::Normal, 16).unwrap();
        let p2 = s.spawn_process("w2", Priority::Normal, 16).unwrap();
        let gid = s.create_group("team".into(), Priority::Normal);
        s.add_to_group(p1, gid).unwrap();
        s.add_to_group(p2, gid).unwrap();
        s.suspend_group(gid).unwrap();
        let resumed = s.resume_group(gid).unwrap();
        assert_eq!(resumed.len(), 2);
        assert_eq!(s.get_process(p1).unwrap().state, ProcessState::Ready);
        assert_eq!(s.get_process(p2).unwrap().state, ProcessState::Ready);
    }

    #[test]
    fn test_set_group_priority() {
        let mut s = make_scheduler();
        let p1 = s.spawn_process("w1", Priority::Normal, 16).unwrap();
        let p2 = s.spawn_process("w2", Priority::Normal, 16).unwrap();
        let gid = s.create_group("team".into(), Priority::Normal);
        s.add_to_group(p1, gid).unwrap();
        s.add_to_group(p2, gid).unwrap();
        s.set_group_priority(gid, Priority::Critical).unwrap();
        assert_eq!(s.get_process(p1).unwrap().priority, Priority::Critical);
        assert_eq!(s.get_process(p2).unwrap().priority, Priority::Critical);
    }

    #[test]
    fn test_rt_mode_default() {
        let s = Scheduler::new(64 * 1024);
        assert_eq!(s.scheduling_mode(), SchedulingMode::Normal);
        assert!(s.rt_deadlines().is_empty());
        assert!(s.jitter_log().is_empty());
    }

    #[test]
    fn test_rt_set_mode() {
        let mut s = Scheduler::new(64 * 1024);
        s.set_scheduling_mode(SchedulingMode::RealTime);
        assert_eq!(s.scheduling_mode(), SchedulingMode::RealTime);
    }

    #[test]
    fn test_rt_deadline_management() {
        let mut s = Scheduler::new(64 * 1024);
        let p1 = s.spawn_process("sensor", Priority::Normal, 16).unwrap();
        s.set_rt_deadline(p1, 100);
        assert_eq!(s.rt_deadlines().get(&p1), Some(&100));
        s.clear_rt_deadline(p1);
        assert!(s.rt_deadlines().get(&p1).is_none());
    }

    #[test]
    fn test_rt_schedule_picks_earliest_deadline() {
        let mut s = Scheduler::new(64 * 1024);
        let p1 = s.spawn_process("fast", Priority::Normal, 16).unwrap();
        let p2 = s.spawn_process("slow", Priority::Normal, 16).unwrap();
        let now = now_ms();
        s.set_rt_deadline(p1, now + 10);
        s.set_rt_deadline(p2, now + 100);
        s.set_scheduling_mode(SchedulingMode::RealTime);
        let scheduled = s.schedule_next().unwrap();
        assert_eq!(scheduled, p1);
    }

    #[test]
    fn test_rt_schedule_skips_non_ready() {
        let mut s = Scheduler::new(64 * 1024);
        let p1 = s.spawn_process("blocked", Priority::Normal, 16).unwrap();
        let p2 = s.spawn_process("ready", Priority::Normal, 16).unwrap();
        let now = now_ms();
        s.set_rt_deadline(p1, now + 5);
        s.set_rt_deadline(p2, now + 50);
        s.suspend_process(p1).unwrap();
        s.set_scheduling_mode(SchedulingMode::RealTime);
        let scheduled = s.schedule_next().unwrap();
        assert_eq!(scheduled, p2);
    }

    #[test]
    fn test_rt_jitter_recorded_on_late_schedule() {
        let mut s = Scheduler::new(64 * 1024);
        let p = s.spawn_process("rt_task", Priority::Normal, 16).unwrap();
        let now = now_ms();
        s.set_rt_deadline(p, now.saturating_sub(100));
        s.set_scheduling_mode(SchedulingMode::RealTime);
        s.schedule_next();
        assert_eq!(s.jitter_log().len(), 1);
        assert_eq!(s.jitter_log()[0].pid, p);
    }

    #[test]
    fn test_rt_jitter_log_clear() {
        let mut s = Scheduler::new(64 * 1024);
        let p = s.spawn_process("rt_task", Priority::Normal, 16).unwrap();
        s.set_rt_deadline(p, 10);
        s.set_scheduling_mode(SchedulingMode::RealTime);
        s.schedule_next();
        assert!(!s.jitter_log().is_empty());
        s.clear_jitter_log();
        assert!(s.jitter_log().is_empty());
    }

    #[test]
    fn test_rt_no_candidates_returns_none() {
        let mut s = Scheduler::new(64 * 1024);
        s.set_scheduling_mode(SchedulingMode::RealTime);
        assert!(s.schedule_next().is_none());
    }

    #[test]
    fn test_rt_switch_to_normal_mode() {
        let mut s = Scheduler::new(64 * 1024);
        s.set_scheduling_mode(SchedulingMode::RealTime);
        assert_eq!(s.scheduling_mode(), SchedulingMode::RealTime);
        s.set_scheduling_mode(SchedulingMode::Normal);
        assert_eq!(s.scheduling_mode(), SchedulingMode::Normal);
    }

    #[test]
    fn test_spawn_real_process() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let mut s = Scheduler::new(256);
        let pid = s
            .spawn_real_process("real_worker", Priority::Normal, 32, move |_term, _susp| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();

        assert_eq!(s.process_count(), 1);
        assert!(s.is_real_process(pid));
        assert_eq!(s.real_thread_count(), 1);

        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(counter.load(Ordering::SeqCst) >= 1);

        s.kill_process(pid).unwrap();
        assert_eq!(s.process_count(), 0);
        assert_eq!(s.real_thread_count(), 0);
    }

    #[test]
    fn test_real_process_terminate_flag() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let ran = Arc::new(AtomicBool::new(false));
        let ran_clone = ran.clone();

        let mut s = Scheduler::new(256);
        let pid = s
            .spawn_real_process("terminating", Priority::Normal, 16, move |term, _susp| {
                while !term.should_stop() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                ran_clone.store(true, Ordering::SeqCst);
            })
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        s.kill_process(pid).unwrap();
        assert!(ran.load(Ordering::SeqCst));
    }

    #[test]
    fn test_real_process_suspend_resume() {
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        use std::sync::Arc;

        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let mut s = Scheduler::new(256);
        let pid = s
            .spawn_real_process("suspendible", Priority::Normal, 16, move |_term, susp| {
                while running_clone.load(Ordering::SeqCst) {
                    if susp.is_suspended() {
                        std::thread::park();
                    }
                    count_clone.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(30));
        let count_before = count.load(Ordering::SeqCst);
        assert!(count_before > 0);

        s.suspend_process(pid).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let count_during = count.load(Ordering::SeqCst);

        s.resume_process(pid).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let count_after = count.load(Ordering::SeqCst);

        assert!(count_after > count_during);

        running.store(false, Ordering::SeqCst);
        s.kill_process(pid).unwrap();
    }

    #[test]
    fn test_real_process_check_finished() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let done = Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        let mut s = Scheduler::new(256);
        let pid = s
            .spawn_real_process("quick", Priority::Normal, 16, move |_term, _susp| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                done_clone.store(true, Ordering::SeqCst);
            })
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        let finished = s.check_real_threads();
        assert!(finished.contains(&pid));
        assert!(!s.is_real_process(pid));
        assert!(done.load(Ordering::SeqCst));
    }

    #[test]
    fn test_real_process_ram_limit() {
        let mut s = Scheduler::new(100);
        s.spawn_process("a", Priority::Normal, 60).unwrap();
        assert!(s
            .spawn_real_process("b", Priority::Normal, 50, |_t, _s| {})
            .is_err());
    }

    #[test]
    fn test_multiple_real_processes() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        let total = Arc::new(AtomicUsize::new(0));

        let mut s = Scheduler::new(1024);
        for i in 0..4 {
            let t = total.clone();
            s.spawn_real_process(
                &format!("worker_{}", i),
                Priority::Normal,
                32,
                move |_term, _susp| {
                    t.fetch_add(1, Ordering::SeqCst);
                },
            )
            .unwrap();
        }

        assert_eq!(s.real_thread_count(), 4);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(total.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn test_set_cpu_affinity() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let mut s = Scheduler::new(256);
        let pid = s
            .spawn_real_process(
                "affinity_test",
                Priority::Normal,
                16,
                move |_term, _susp| {
                    while running_clone.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                },
            )
            .unwrap();

        assert!(s.get_cpu_affinity(pid).is_none());

        let cores = [0];
        s.set_cpu_affinity(pid, &cores).unwrap();
        assert_eq!(s.get_cpu_affinity(pid), Some(vec![0]));

        let cores2 = [0, 1];
        s.set_cpu_affinity(pid, &cores2).unwrap();
        assert_eq!(s.get_cpu_affinity(pid), Some(vec![0, 1]));

        running.store(false, Ordering::SeqCst);
        s.kill_process(pid).unwrap();
    }

    #[test]
    fn test_set_cpu_affinity_nonexistent_process() {
        let mut s = Scheduler::new(256);
        let result = s.set_cpu_affinity(ProcessId::new(999), &[0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_available_cpu_cores() {
        let cores = Scheduler::available_cpu_cores();
        assert!(cores >= 1);
    }

    #[test]
    fn test_list_real_threads() {
        let mut s = Scheduler::new(256);
        let pid = s
            .spawn_real_process("list_test", Priority::Normal, 32, |_, _| {
                std::thread::sleep(std::time::Duration::from_millis(20));
            })
            .unwrap();

        let threads = s.list_real_threads();
        assert!(threads.contains(&pid));
        assert_eq!(threads.len(), 1);

        s.kill_process(pid).unwrap();
        let threads = s.list_real_threads();
        assert!(threads.is_empty());
    }

    #[test]
    fn test_get_real_thread_state() {
        let mut s = Scheduler::new(256);
        let pid = s
            .spawn_real_process("state_test", Priority::Normal, 32, |_, _| {})
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        let state = s.get_real_thread_state(pid);
        assert!(state.is_some());
        let state = state.unwrap();
        assert!(!state.terminated);
        assert!(!state.suspended);

        s.kill_process(pid).unwrap();
        let state = s.get_real_thread_state(pid);
        assert!(state.is_none());
    }
}
