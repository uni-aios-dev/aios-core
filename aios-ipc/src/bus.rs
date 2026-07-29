use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::IpcPacket;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackpressurePolicy {
    Reject,
    DropOldest,
}

pub struct IpcBus {
    queue: VecDeque<IpcPacket>,
    max_queue_size: usize,
    frozen: bool,
    backpressure: BackpressurePolicy,
    seen_packet_ids: HashSet<u64>,
    dedup_enabled: bool,
    metrics: BusMetrics,
}

#[derive(Debug, Clone, Default)]
pub struct BusMetrics {
    pub total_sent: u64,
    pub total_received: u64,
    pub total_dropped: u64,
    pub total_deduplicated: u64,
    pub peak_queue_depth: usize,
    pub send_latency_sum_us: u128,
    pub send_count_for_latency: u64,
}

impl BusMetrics {
    pub fn avg_send_latency_us(&self) -> f64 {
        if self.send_count_for_latency == 0 {
            0.0
        } else {
            self.send_latency_sum_us as f64 / self.send_count_for_latency as f64
        }
    }

    pub fn queue_depth(&self) -> usize {
        self.peak_queue_depth
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl IpcBus {
    pub fn new(max_queue_size: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            max_queue_size,
            frozen: false,
            backpressure: BackpressurePolicy::Reject,
            seen_packet_ids: HashSet::new(),
            dedup_enabled: false,
            metrics: BusMetrics::default(),
        }
    }

    pub fn with_backpressure(mut self, policy: BackpressurePolicy) -> Self {
        self.backpressure = policy;
        self
    }

    pub fn with_dedup(mut self) -> Self {
        self.dedup_enabled = true;
        self
    }

    fn is_duplicate(&self, packet: &IpcPacket) -> bool {
        self.dedup_enabled && self.seen_packet_ids.contains(&packet.header.packet_id)
    }

    fn record_send_latency(&mut self, start: Instant) {
        let us = start.elapsed().as_micros();
        self.metrics.send_latency_sum_us += us;
        self.metrics.send_count_for_latency += 1;
    }

    pub fn send(&mut self, packet: IpcPacket) -> Result<()> {
        let start = Instant::now();
        if self.frozen {
            return Err(AIOSException::IPCError("Bus is frozen".into()));
        }
        if self.is_duplicate(&packet) {
            self.metrics.total_deduplicated += 1;
            return Ok(());
        }
        if self.queue.len() >= self.max_queue_size {
            match self.backpressure {
                BackpressurePolicy::Reject => {
                    self.metrics.total_dropped += 1;
                    return Err(AIOSException::IPCError("Message queue full".into()));
                }
                BackpressurePolicy::DropOldest => {
                    if let Some(dropped) = self.queue.pop_front() {
                        self.seen_packet_ids.remove(&dropped.header.packet_id);
                        self.metrics.total_dropped += 1;
                        log::warn!(
                            "IPC: Drop-oldest: evicted packet {} → {}",
                            dropped.header.source_block,
                            dropped.header.target_block,
                        );
                    }
                }
            }
        }
        log::trace!(
            "IPC: {} → {} [cmd=0x{:04X}]",
            packet.header.source_block,
            packet.header.target_block,
            packet.header.command_id,
        );
        self.seen_packet_ids.insert(packet.header.packet_id);
        self.queue.push_back(packet);
        self.metrics.total_sent += 1;
        if self.queue.len() > self.metrics.peak_queue_depth {
            self.metrics.peak_queue_depth = self.queue.len();
        }
        self.record_send_latency(start);
        Ok(())
    }

    pub fn send_priority(&mut self, packet: IpcPacket) -> Result<()> {
        let start = Instant::now();
        if self.frozen {
            return Err(AIOSException::IPCError("Bus is frozen".into()));
        }
        if self.is_duplicate(&packet) {
            self.metrics.total_deduplicated += 1;
            return Ok(());
        }
        if self.queue.len() >= self.max_queue_size {
            match self.backpressure {
                BackpressurePolicy::Reject => {
                    self.metrics.total_dropped += 1;
                    return Err(AIOSException::IPCError("Message queue full".into()));
                }
                BackpressurePolicy::DropOldest => {
                    if let Some(dropped) = self.queue.pop_front() {
                        self.seen_packet_ids.remove(&dropped.header.packet_id);
                        self.metrics.total_dropped += 1;
                    }
                }
            }
        }
        let priority = packet.header.priority;
        let pos = self
            .queue
            .iter()
            .position(|p| p.header.priority < priority)
            .unwrap_or(self.queue.len());
        self.seen_packet_ids.insert(packet.header.packet_id);
        self.queue.insert(pos, packet);
        self.metrics.total_sent += 1;
        if self.queue.len() > self.metrics.peak_queue_depth {
            self.metrics.peak_queue_depth = self.queue.len();
        }
        self.record_send_latency(start);
        Ok(())
    }

    pub fn receive(&mut self) -> Option<IpcPacket> {
        let pkt = self.queue.pop_front();
        if let Some(ref p) = pkt {
            self.seen_packet_ids.remove(&p.header.packet_id);
            self.metrics.total_received += 1;
        }
        pkt
    }

    pub fn peek(&self) -> Option<&IpcPacket> {
        self.queue.front()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn flush(&mut self) {
        self.queue.clear();
        self.seen_packet_ids.clear();
    }

    pub fn freeze(&mut self) -> Vec<IpcPacket> {
        self.frozen = true;
        self.seen_packet_ids.clear();
        self.queue.drain(..).collect()
    }

    pub fn unfreeze(&mut self, packets: Vec<IpcPacket>) {
        self.frozen = false;
        for pkt in packets.into_iter().rev() {
            self.seen_packet_ids.insert(pkt.header.packet_id);
            self.queue.push_front(pkt);
        }
    }

    pub fn reroute(&mut self, old_target: u32, new_target: u32) -> usize {
        let mut count = 0;
        for pkt in self.queue.iter_mut() {
            if pkt.header.target_block == old_target {
                pkt.header.target_block = new_target;
                count += 1;
            }
        }
        if count > 0 {
            log::info!(
                "IPC: Rerouted {} pending packets {} → {}",
                count,
                old_target,
                new_target
            );
        }
        count
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    pub fn metrics(&self) -> &BusMetrics {
        &self.metrics
    }

    pub fn reset_metrics(&mut self) {
        self.metrics.reset();
    }
}

impl Default for IpcBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

pub struct SharedIpcBus {
    inner: Arc<Mutex<IpcBus>>,
}

impl SharedIpcBus {
    pub fn new(max_queue_size: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(IpcBus::new(max_queue_size))),
        }
    }

    pub fn with_backpressure(self, policy: BackpressurePolicy) -> Self {
        if let Ok(mut bus) = self.inner.lock() {
            bus.backpressure = policy;
        }
        self
    }

    pub fn with_dedup(self) -> Self {
        if let Ok(mut bus) = self.inner.lock() {
            bus.dedup_enabled = true;
        }
        self
    }

    pub fn send(&self, packet: IpcPacket) -> Result<()> {
        self.inner
            .lock()
            .map_err(|_| AIOSException::IPCError("Lock poisoned".into()))?
            .send(packet)
    }

    pub fn receive(&self) -> Option<IpcPacket> {
        self.inner.lock().ok()?.receive()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|b| b.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().map(|b| b.is_empty()).unwrap_or(true)
    }

    pub fn flush(&self) {
        if let Ok(mut bus) = self.inner.lock() {
            bus.flush();
        }
    }

    pub fn metrics(&self) -> BusMetrics {
        self.inner
            .lock()
            .map(|b| b.metrics().clone())
            .unwrap_or_default()
    }
}

impl Clone for SharedIpcBus {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::{CommandId, Payload};

    fn test_packet(target: u32) -> IpcPacket {
        IpcPacket::new(0, target, CommandId::HealthCheck, Payload::Empty)
    }

    fn test_packet_with_priority(target: u32, priority: u8) -> IpcPacket {
        IpcPacket::new(0, target, CommandId::HealthCheck, Payload::Empty).with_priority(priority)
    }

    #[test]
    fn test_send_receive() {
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();
        assert_eq!(bus.len(), 2);
        let pkt = bus.receive().unwrap();
        assert_eq!(pkt.header.target_block, 1);
    }

    #[test]
    fn test_queue_full_reject() {
        let mut bus = IpcBus::new(2);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();
        assert!(bus.send(test_packet(3)).is_err());
        assert_eq!(bus.metrics().total_dropped, 1);
    }

    #[test]
    fn test_drop_oldest_backpressure() {
        let mut bus = IpcBus::new(2).with_backpressure(BackpressurePolicy::DropOldest);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();
        bus.send(test_packet(3)).unwrap();
        assert_eq!(bus.len(), 2);
        assert_eq!(bus.metrics().total_dropped, 1);
        let first = bus.receive().unwrap();
        assert_eq!(first.header.target_block, 2);
    }

    #[test]
    fn test_deduplication() {
        let mut bus = IpcBus::new(10).with_dedup();
        let pkt = test_packet(1);
        let id = pkt.header.packet_id;
        bus.send(pkt).unwrap();
        let dup = IpcPacket {
            header: aios_core::ipc_protocol::Header {
                packet_id: id,
                source_block: 0,
                target_block: 1,
                command_id: CommandId::HealthCheck as u16,
                priority: 0,
                payload_len: 0,
                checksum: [0; 32],
            },
            payload: Payload::Empty,
        };
        bus.send(dup).unwrap();
        assert_eq!(bus.len(), 1);
        assert_eq!(bus.metrics().total_deduplicated, 1);
    }

    #[test]
    fn test_bus_metrics() {
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();
        bus.receive().unwrap();
        let m = bus.metrics();
        assert_eq!(m.total_sent, 2);
        assert_eq!(m.total_received, 1);
        assert!(m.avg_send_latency_us() >= 0.0);
    }

    #[test]
    fn test_reset_metrics() {
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.reset_metrics();
        assert_eq!(bus.metrics().total_sent, 0);
    }

    #[test]
    fn test_freeze_unfreeze() {
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();
        let frozen = bus.freeze();
        assert!(bus.is_empty());
        assert!(bus.is_frozen());
        bus.unfreeze(frozen);
        assert!(!bus.is_frozen());
        assert_eq!(bus.len(), 2);
    }

    #[test]
    fn test_frozen_bus_rejects() {
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        let _ = bus.freeze();
        assert!(bus.send(test_packet(2)).is_err());
    }

    #[test]
    fn test_shared_bus() {
        let bus = SharedIpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();
        let p1 = bus.receive().unwrap();
        let p2 = bus.receive().unwrap();
        assert_eq!(p1.header.target_block, 1);
        assert_eq!(p2.header.target_block, 2);
    }

    #[test]
    fn test_shared_bus_metrics() {
        let bus = SharedIpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();
        let m = bus.metrics();
        assert_eq!(m.total_sent, 2);
    }

    #[test]
    fn test_priority_queue_ordering() {
        let mut bus = IpcBus::new(10);
        bus.send_priority(test_packet_with_priority(1, 1)).unwrap();
        bus.send_priority(test_packet_with_priority(2, 4)).unwrap();
        bus.send_priority(test_packet_with_priority(3, 2)).unwrap();

        let p1 = bus.receive().unwrap();
        let p2 = bus.receive().unwrap();
        let p3 = bus.receive().unwrap();
        assert_eq!(p1.header.target_block, 2);
        assert_eq!(p2.header.target_block, 3);
        assert_eq!(p3.header.target_block, 1);
    }

    #[test]
    fn test_priority_fifo_within_same_level() {
        let mut bus = IpcBus::new(10);
        bus.send_priority(test_packet_with_priority(1, 2)).unwrap();
        bus.send_priority(test_packet_with_priority(2, 2)).unwrap();

        let p1 = bus.receive().unwrap();
        let p2 = bus.receive().unwrap();
        assert_eq!(p1.header.target_block, 1);
        assert_eq!(p2.header.target_block, 2);
    }

    #[test]
    fn test_drop_oldest_with_priority() {
        let mut bus = IpcBus::new(2).with_backpressure(BackpressurePolicy::DropOldest);
        bus.send_priority(test_packet_with_priority(1, 1)).unwrap();
        bus.send_priority(test_packet_with_priority(2, 1)).unwrap();
        bus.send_priority(test_packet_with_priority(3, 5)).unwrap();
        assert_eq!(bus.len(), 2);
        let first = bus.receive().unwrap();
        assert_eq!(first.header.target_block, 3);
    }

    #[test]
    fn test_reroute_pending_packets() {
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(3)).unwrap();

        let rerouted = bus.reroute(1, 99);
        assert_eq!(rerouted, 2);
        assert_eq!(bus.len(), 4);

        let p1 = bus.receive().unwrap();
        assert_eq!(p1.header.target_block, 99);
        let p2 = bus.receive().unwrap();
        assert_eq!(p2.header.target_block, 2);
        let p3 = bus.receive().unwrap();
        assert_eq!(p3.header.target_block, 99);
        let p4 = bus.receive().unwrap();
        assert_eq!(p4.header.target_block, 3);
    }

    #[test]
    fn test_reroute_no_matching_packets() {
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();

        let rerouted = bus.reroute(5, 99);
        assert_eq!(rerouted, 0);
        assert_eq!(bus.len(), 2);
    }
}
