use crate::task::{Priority, ProcessId};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct LockOwner {
    pub lock_id: u32,
    pub owner: ProcessId,
    pub original_priority: Priority,
}

pub struct PriorityInheritance {
    lock_owners: HashMap<u32, ProcessId>,
    original_priorities: HashMap<ProcessId, Priority>,
    lock_waiters: HashMap<u32, Vec<ProcessId>>,
    pending_boosts: Vec<PriorityBoost>,
}

#[derive(Debug, Clone)]
pub struct PriorityBoost {
    pub pid: ProcessId,
    pub from: Priority,
    pub to: Priority,
    pub reason: BoostReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoostReason {
    LockContention,
    ResourceWait,
    IoPending,
}

#[derive(Debug, Clone)]
pub struct InheritanceState {
    pub active_locks: usize,
    pub boosted_processes: usize,
    pub pending_boosts: usize,
    pub total_inheritances: u64,
}

impl Default for PriorityInheritance {
    fn default() -> Self {
        Self::new()
    }
}

impl PriorityInheritance {
    pub fn new() -> Self {
        Self {
            lock_owners: HashMap::new(),
            original_priorities: HashMap::new(),
            lock_waiters: HashMap::new(),
            pending_boosts: Vec::new(),
        }
    }

    pub fn acquire_lock(
        &mut self,
        lock_id: u32,
        requester: ProcessId,
        requester_priority: Priority,
    ) -> LockResult {
        if let Some(current_owner) = self.lock_owners.get(&lock_id) {
            if *current_owner == requester {
                return LockResult::AlreadyHeld;
            }

            let owner_priority = self
                .original_priorities
                .get(current_owner)
                .copied()
                .unwrap_or(Priority::Normal);

            if requester_priority > owner_priority {
                self.pending_boosts.push(PriorityBoost {
                    pid: *current_owner,
                    from: owner_priority,
                    to: requester_priority,
                    reason: BoostReason::LockContention,
                });
                self.original_priorities
                    .entry(*current_owner)
                    .or_insert(owner_priority);
            }

            self.lock_waiters
                .entry(lock_id)
                .or_default()
                .push(requester);

            LockResult::Blocked {
                owner: *current_owner,
            }
        } else {
            self.lock_owners.insert(lock_id, requester);
            self.original_priorities
                .entry(requester)
                .or_insert(requester_priority);
            LockResult::Acquired
        }
    }

    pub fn release_lock(&mut self, lock_id: u32, releaser: ProcessId) -> Option<ProcessId> {
        {
            let owner = self.lock_owners.get(&lock_id)?;
            if *owner != releaser {
                return None;
            }
        }

        self.lock_owners.remove(&lock_id);
        self.restore_priority(releaser);

        if let Some(waiters) = self.lock_waiters.get_mut(&lock_id) {
            if !waiters.is_empty() {
                let next = waiters.remove(0);
                self.lock_owners.insert(lock_id, next);
                return Some(next);
            }
        }

        None
    }

    pub fn request_resource(
        &mut self,
        pid: ProcessId,
        current_priority: Priority,
        resource_id: u32,
    ) -> ResourceResult {
        if let Some(owner) = self.lock_owners.get(&resource_id) {
            if *owner == pid {
                return ResourceResult::AlreadyHeld;
            }

            let owner_priority = self
                .original_priorities
                .get(owner)
                .copied()
                .unwrap_or(Priority::Normal);

            if current_priority > owner_priority {
                self.pending_boosts.push(PriorityBoost {
                    pid: *owner,
                    from: owner_priority,
                    to: current_priority,
                    reason: BoostReason::ResourceWait,
                });
            }

            ResourceResult::Blocked { owner: *owner }
        } else {
            self.lock_owners.insert(resource_id, pid);
            self.original_priorities
                .entry(pid)
                .or_insert(current_priority);
            ResourceResult::Granted
        }
    }

    pub fn apply_pending_boosts(&mut self) -> Vec<PriorityBoost> {
        std::mem::take(&mut self.pending_boosts)
    }

    pub fn restore_priority(&mut self, pid: ProcessId) -> Option<Priority> {
        self.original_priorities.remove(&pid)
    }

    pub fn release_all(&mut self, pid: ProcessId) -> Vec<u32> {
        let locks: Vec<u32> = self
            .lock_owners
            .iter()
            .filter(|(_, &owner)| owner == pid)
            .map(|(&lock_id, _)| lock_id)
            .collect();

        for lock_id in &locks {
            self.lock_owners.remove(lock_id);
            if let Some(waiters) = self.lock_waiters.get_mut(lock_id) {
                if !waiters.is_empty() {
                    let next = waiters.remove(0);
                    self.lock_owners.insert(*lock_id, next);
                }
            }
        }

        self.restore_priority(pid);
        locks
    }

    pub fn state(&self) -> InheritanceState {
        InheritanceState {
            active_locks: self.lock_owners.len(),
            boosted_processes: self.original_priorities.len(),
            pending_boosts: self.pending_boosts.len(),
            total_inheritances: 0,
        }
    }

    pub fn is_locked(&self, lock_id: u32) -> bool {
        self.lock_owners.contains_key(&lock_id)
    }

    pub fn owner_of(&self, lock_id: u32) -> Option<ProcessId> {
        self.lock_owners.get(&lock_id).copied()
    }

    pub fn waiters_of(&self, lock_id: u32) -> Vec<ProcessId> {
        self.lock_waiters.get(&lock_id).cloned().unwrap_or_default()
    }

    pub fn active_locks(&self) -> Vec<u32> {
        let mut locks: Vec<u32> = self.lock_owners.keys().copied().collect();
        locks.sort();
        locks
    }

    pub fn lock_owner_details(&self) -> Vec<LockOwner> {
        self.lock_owners
            .iter()
            .map(|(&lock_id, &owner)| LockOwner {
                lock_id,
                owner,
                original_priority: self
                    .original_priorities
                    .get(&owner)
                    .copied()
                    .unwrap_or(Priority::Normal),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockResult {
    Acquired,
    Blocked { owner: ProcessId },
    AlreadyHeld,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceResult {
    Granted,
    Blocked { owner: ProcessId },
    AlreadyHeld,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(n: u64) -> ProcessId {
        ProcessId::new(n)
    }

    #[test]
    fn test_acquire_and_release() {
        let mut pi = PriorityInheritance::new();
        assert_eq!(
            pi.acquire_lock(1, pid(1), Priority::Normal),
            LockResult::Acquired
        );
        assert_eq!(pi.owner_of(1), Some(pid(1)));
        assert!(pi.is_locked(1));

        let next = pi.release_lock(1, pid(1));
        assert_eq!(next, None);
        assert!(!pi.is_locked(1));
    }

    #[test]
    fn test_contention_boosts_owner() {
        let mut pi = PriorityInheritance::new();
        assert_eq!(
            pi.acquire_lock(1, pid(1), Priority::Low),
            LockResult::Acquired
        );

        let result = pi.acquire_lock(1, pid(2), Priority::High);
        assert_eq!(result, LockResult::Blocked { owner: pid(1) });

        let boosts = pi.apply_pending_boosts();
        assert_eq!(boosts.len(), 1);
        assert_eq!(boosts[0].pid, pid(1));
        assert_eq!(boosts[0].from, Priority::Low);
        assert_eq!(boosts[0].to, Priority::High);
        assert_eq!(boosts[0].reason, BoostReason::LockContention);
    }

    #[test]
    fn test_release_wakes_waiter() {
        let mut pi = PriorityInheritance::new();
        pi.acquire_lock(1, pid(1), Priority::Normal);
        pi.acquire_lock(1, pid(2), Priority::High);
        pi.acquire_lock(1, pid(3), Priority::Critical);

        let next = pi.release_lock(1, pid(1)).unwrap();
        assert_eq!(next, pid(2));

        let next = pi.release_lock(1, pid(2)).unwrap();
        assert_eq!(next, pid(3));

        let next = pi.release_lock(1, pid(3));
        assert_eq!(next, None);
    }

    #[test]
    fn test_already_held() {
        let mut pi = PriorityInheritance::new();
        pi.acquire_lock(1, pid(1), Priority::Normal);
        assert_eq!(
            pi.acquire_lock(1, pid(1), Priority::Normal),
            LockResult::AlreadyHeld
        );
    }

    #[test]
    fn test_release_wrong_owner() {
        let mut pi = PriorityInheritance::new();
        pi.acquire_lock(1, pid(1), Priority::Normal);
        assert_eq!(pi.release_lock(1, pid(2)), None);
    }

    #[test]
    fn test_request_resource() {
        let mut pi = PriorityInheritance::new();
        assert_eq!(
            pi.request_resource(pid(1), Priority::Normal, 100),
            ResourceResult::Granted
        );
        assert_eq!(
            pi.request_resource(pid(2), Priority::Normal, 100),
            ResourceResult::Blocked { owner: pid(1) }
        );
        assert_eq!(
            pi.request_resource(pid(1), Priority::Normal, 100),
            ResourceResult::AlreadyHeld
        );
    }

    #[test]
    fn test_release_all() {
        let mut pi = PriorityInheritance::new();
        pi.acquire_lock(1, pid(1), Priority::Normal);
        pi.acquire_lock(2, pid(1), Priority::Normal);
        pi.acquire_lock(3, pid(1), Priority::Normal);

        let released = pi.release_all(pid(1));
        assert_eq!(released.len(), 3);
        assert!(released.contains(&1));
        assert!(released.contains(&2));
        assert!(released.contains(&3));
    }

    #[test]
    fn test_state() {
        let mut pi = PriorityInheritance::new();
        pi.acquire_lock(1, pid(1), Priority::Normal);
        pi.acquire_lock(1, pid(2), Priority::High);

        let state = pi.state();
        assert_eq!(state.active_locks, 1);
        assert!(state.boosted_processes >= 1);
        assert!(state.pending_boosts >= 1);
    }

    #[test]
    fn test_waiters_of() {
        let mut pi = PriorityInheritance::new();
        pi.acquire_lock(1, pid(1), Priority::Low);
        pi.acquire_lock(1, pid(2), Priority::High);
        pi.acquire_lock(1, pid(3), Priority::Critical);

        let waiters = pi.waiters_of(1);
        assert_eq!(waiters, vec![pid(2), pid(3)]);
    }

    #[test]
    fn test_active_locks_sorted() {
        let mut pi = PriorityInheritance::new();
        pi.acquire_lock(5, pid(1), Priority::Normal);
        pi.acquire_lock(2, pid(2), Priority::Normal);
        pi.acquire_lock(8, pid(3), Priority::Normal);

        assert_eq!(pi.active_locks(), vec![2, 5, 8]);
    }

    #[test]
    fn test_lock_owner_details() {
        let mut pi = PriorityInheritance::new();
        pi.acquire_lock(1, pid(1), Priority::High);

        let details = pi.lock_owner_details();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].lock_id, 1);
        assert_eq!(details[0].owner, pid(1));
        assert_eq!(details[0].original_priority, Priority::High);
    }

    #[test]
    fn test_no_boost_for_lower_priority() {
        let mut pi = PriorityInheritance::new();
        pi.acquire_lock(1, pid(1), Priority::High);

        let result = pi.acquire_lock(1, pid(2), Priority::Low);
        assert_eq!(result, LockResult::Blocked { owner: pid(1) });

        let boosts = pi.apply_pending_boosts();
        assert!(boosts.is_empty());
    }
}
