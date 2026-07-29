use crate::scheduler::Scheduler;
use crate::task::{Priority, ProcessId};
use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};

pub fn handle_process_command(
    scheduler: &mut Scheduler,
    packet: &IpcPacket,
) -> Result<Option<IpcPacket>> {
    match packet.header.command_id {
        cmd if cmd == CommandId::SpawnProcess as u16 => {
            if let Payload::SpawnProcess {
                ref name,
                priority,
                ram_mb,
            } = packet.payload
            {
                let prio = Priority::from_u8(priority);
                match scheduler.spawn_process(name, prio, ram_mb) {
                    Ok(pid) => Ok(Some(IpcPacket::response_ok(
                        0,
                        packet.header.source_block,
                        packet.header.packet_id,
                        Payload::Text(format!("{}", pid.0)),
                    ))),
                    Err(e) => Ok(Some(IpcPacket::response_err(
                        0,
                        packet.header.source_block,
                        packet.header.packet_id,
                        e.to_string(),
                    ))),
                }
            } else {
                Err(AIOSException::InvalidPayload(
                    "SpawnProcess requires SpawnProcess payload".into(),
                ))
            }
        }

        cmd if cmd == CommandId::KillProcess as u16 => {
            if let Payload::KillProcess { pid } = packet.payload {
                match scheduler.kill_process(ProcessId::new(pid)) {
                    Ok(proc) => Ok(Some(IpcPacket::response_ok(
                        0,
                        packet.header.source_block,
                        packet.header.packet_id,
                        Payload::Text(format!("killed {}", proc.name)),
                    ))),
                    Err(e) => Ok(Some(IpcPacket::response_err(
                        0,
                        packet.header.source_block,
                        packet.header.packet_id,
                        e.to_string(),
                    ))),
                }
            } else {
                Err(AIOSException::InvalidPayload(
                    "KillProcess requires KillProcess payload".into(),
                ))
            }
        }

        cmd if cmd == CommandId::AdjustPriority as u16 => {
            if let Payload::AdjustPriority { pid, new_priority } = packet.payload {
                let prio = Priority::from_u8(new_priority);
                match scheduler.set_priority(ProcessId::new(pid), prio) {
                    Ok(()) => Ok(Some(IpcPacket::response_ok(
                        0,
                        packet.header.source_block,
                        packet.header.packet_id,
                        Payload::Text(format!("priority set to {}", prio)),
                    ))),
                    Err(e) => Ok(Some(IpcPacket::response_err(
                        0,
                        packet.header.source_block,
                        packet.header.packet_id,
                        e.to_string(),
                    ))),
                }
            } else {
                Err(AIOSException::InvalidPayload(
                    "AdjustPriority requires AdjustPriority payload".into(),
                ))
            }
        }

        _ => Err(AIOSException::IPCError(format!(
            "Unknown process command 0x{:04X}",
            packet.header.command_id
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::Scheduler;

    fn make_scheduler() -> Scheduler {
        Scheduler::new(1024).with_time_slice(10)
    }

    #[test]
    fn test_spawn_process_via_ipc() {
        let mut sched = make_scheduler();
        let pkt = IpcPacket::new(
            0,
            3,
            CommandId::SpawnProcess,
            Payload::SpawnProcess {
                name: "test_proc".into(),
                priority: 2,
                ram_mb: 64,
            },
        );
        let resp = handle_process_command(&mut sched, &pkt).unwrap().unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));
    }

    #[test]
    fn test_kill_process_via_ipc() {
        let mut sched = make_scheduler();
        let pid = sched.spawn_process("victim", Priority::Normal, 32).unwrap();

        let pkt = IpcPacket::new(
            0,
            3,
            CommandId::KillProcess,
            Payload::KillProcess { pid: pid.0 },
        );
        let resp = handle_process_command(&mut sched, &pkt).unwrap().unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));
        assert_eq!(sched.process_count(), 0);
    }

    #[test]
    fn test_adjust_priority_via_ipc() {
        let mut sched = make_scheduler();
        let pid = sched.spawn_process("task", Priority::Low, 8).unwrap();

        let pkt = IpcPacket::new(
            0,
            3,
            CommandId::AdjustPriority,
            Payload::AdjustPriority {
                pid: pid.0,
                new_priority: 4,
            },
        );
        let resp = handle_process_command(&mut sched, &pkt).unwrap().unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));
        assert_eq!(sched.get_process(pid).unwrap().priority, Priority::Critical);
    }
}
