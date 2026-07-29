use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::IpcPacket;
use std::sync::mpsc;

pub struct IpcSender {
    sender: mpsc::Sender<IpcPacket>,
}

impl IpcSender {
    pub fn send(&self, packet: IpcPacket) -> Result<()> {
        self.sender
            .send(packet)
            .map_err(|e| AIOSException::IPCError(format!("Send failed: {e}")))
    }
}

impl Clone for IpcSender {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

pub struct IpcReceiver {
    receiver: mpsc::Receiver<IpcPacket>,
}

impl IpcReceiver {
    pub fn receive(&self) -> Result<IpcPacket> {
        self.receiver
            .recv()
            .map_err(|e| AIOSException::IPCError(format!("Receive failed: {e}")))
    }

    pub fn try_receive(&self) -> Option<IpcPacket> {
        self.receiver.try_recv().ok()
    }
}

pub fn channel() -> (IpcSender, IpcReceiver) {
    let (tx, rx) = mpsc::channel();
    (IpcSender { sender: tx }, IpcReceiver { receiver: rx })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::{CommandId, Payload};

    #[test]
    fn test_channel_send_receive() {
        let (tx, rx) = channel();
        let pkt = IpcPacket::new(
            0,
            1,
            CommandId::HealthCheck,
            Payload::Binary(b"ping".to_vec()),
        );
        tx.send(pkt.clone()).unwrap();
        let received = rx.receive().unwrap();
        assert_eq!(received.header.packet_id, pkt.header.packet_id);
    }

    #[test]
    fn test_try_receive_empty() {
        let (_tx, rx) = channel();
        assert!(rx.try_receive().is_none());
    }

    #[test]
    fn test_clone_sender() {
        let (tx, rx) = channel();
        let tx2 = tx.clone();
        let p1 = IpcPacket::new(0, 1, CommandId::HealthCheck, Payload::Empty);
        let p2 = IpcPacket::new(0, 2, CommandId::HealthCheck, Payload::Empty);
        tx.send(p1).unwrap();
        tx2.send(p2).unwrap();
        assert!(rx.receive().is_ok());
        assert!(rx.receive().is_ok());
    }
}
