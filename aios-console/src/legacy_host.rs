//! Host-only exact-output adapters for legacy shell commands backed by crates.

use std::sync::{Arc, Mutex};

use aios_cluster::scheduler::DistributedScheduler;
use aios_cluster::types::{RemoteProcessId, RemoteProcessSpec};

/// Formats the exact legacy `aios` TUI shell `cluster` output.
///
/// The adapter reads the same `DistributedScheduler` as the legacy dispatcher,
/// so read-only and mutating subcommands keep their previous behavior.
pub fn aios_cluster_text(
    cluster: Option<Arc<Mutex<DistributedScheduler>>>,
    parts: &[&str],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(cluster) = cluster else {
        out.push("  clustering disabled (set AIOS_CLUSTER_PEERS to enable)".into());
        return out;
    };
    let sub = parts.get(1).copied().unwrap_or("");
    match sub {
        "" | "help" => {
            out.push("Usage: cluster status | cluster nodes | cluster spawn <name> [ram_mb] [priority] [target_node] | cluster kill <node> <pid> | cluster migrate <node> <pid> [target_node]".into());
        }
        "status" => {
            let s = cluster.lock().unwrap();
            out.push(format!(
                "  self: [{}] {} tier={}",
                s.self_info().id,
                s.self_info().name,
                s.self_info().tier
            ));
            for n in s.nodes() {
                out.push(format!(
                    "  [{}] {} tier={} {:?} load={:.1}%",
                    n.id,
                    n.name,
                    n.tier,
                    n.status,
                    n.metrics.load_fraction() * 100.0
                ));
            }
            let remote = s.processes();
            if remote.is_empty() {
                out.push("  remote processes: none".into());
            } else {
                out.push(format!("  remote processes ({}):", remote.len()));
                for p in remote {
                    out.push(format!(
                        "    {} {} [{}] {}MB",
                        p.id, p.name, p.state, p.ram_mb
                    ));
                }
            }
            out.push(format!(
                "  local processes hosted: {}",
                s.local_processes().len()
            ));
        }
        "nodes" => {
            let s = cluster.lock().unwrap();
            for n in s.nodes() {
                out.push(format!(
                    "  [{}] {} tier={} {:?} load={:.1}%",
                    n.id,
                    n.name,
                    n.tier,
                    n.status,
                    n.metrics.load_fraction() * 100.0
                ));
            }
        }
        "spawn" => {
            let name = parts.get(2).copied().unwrap_or("");
            if name.is_empty() {
                out.push("Usage: cluster spawn <name> [ram_mb] [priority] [target_node]".into());
                return out;
            }
            let ram = parts
                .get(3)
                .and_then(|p| p.parse::<u64>().ok())
                .unwrap_or(128);
            let prio = parts.get(4).and_then(|p| p.parse::<u8>().ok()).unwrap_or(2);
            let target = parts.get(5).and_then(|p| p.parse::<u64>().ok());
            let spec = RemoteProcessSpec::new(name, prio, ram);
            match cluster.lock().unwrap().spawn(spec, target) {
                Ok(rid) => out.push(format!("  spawned {rid} on node {}", rid.node)),
                Err(e) => out.push(format!("  spawn failed: {e}")),
            }
        }
        "kill" => {
            let node = parts.get(2).and_then(|p| p.parse::<u64>().ok());
            let pid = parts.get(3).and_then(|p| p.parse::<u64>().ok());
            let (Some(node), Some(pid)) = (node, pid) else {
                out.push("Usage: cluster kill <node> <pid>".into());
                return out;
            };
            let rid = RemoteProcessId { node, pid };
            match cluster.lock().unwrap().kill(rid) {
                Ok(()) => out.push(format!("  killed {rid}")),
                Err(e) => out.push(format!("  kill failed: {e}")),
            }
        }
        "migrate" => {
            let node = parts.get(2).and_then(|p| p.parse::<u64>().ok());
            let pid = parts.get(3).and_then(|p| p.parse::<u64>().ok());
            let (Some(node), Some(pid)) = (node, pid) else {
                out.push("Usage: cluster migrate <node> <pid> [target_node]".into());
                return out;
            };
            let rid = RemoteProcessId { node, pid };
            let target = parts.get(4).and_then(|p| p.parse::<u64>().ok());
            match cluster.lock().unwrap().migrate(rid, target) {
                Ok(new_rid) => out.push(format!("  migrated {rid} -> {new_rid}")),
                Err(e) => out.push(format!("  migrate failed: {e}")),
            }
        }
        _ => {
            out.push("Usage: cluster status | cluster nodes | cluster spawn ... | cluster kill <node> <pid> | cluster migrate <node> <pid> [target_node]".into());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::aios_cluster_text;

    #[test]
    fn disabled_cluster_keeps_the_legacy_hint() {
        for input in [
            "cluster",
            "cluster status",
            "cluster spawn worker",
            "cluster kill 1 2",
            "cluster migrate 1 2 3",
        ] {
            let parts: Vec<&str> = input.split_whitespace().collect();
            let lines = aios_cluster_text(None, &parts);
            assert_eq!(
                lines,
                ["  clustering disabled (set AIOS_CLUSTER_PEERS to enable)"],
                "input: {input}"
            );
        }
    }
}
