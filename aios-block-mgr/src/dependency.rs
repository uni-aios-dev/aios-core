use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyError {
    CircularDependency(Vec<String>),
    NotFound(String),
}

impl std::fmt::Display for DependencyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DependencyError::CircularDependency(chain) => {
                write!(f, "Circular dependency detected: {}", chain.join(" -> "))
            }
            DependencyError::NotFound(name) => {
                write!(f, "Block dependency not found: '{}'", name)
            }
        }
    }
}

impl std::error::Error for DependencyError {}

pub struct DependencyGraph {
    edges: HashMap<String, Vec<String>>,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self {
            edges: HashMap::new(),
        }
    }

    pub fn add_block(&mut self, name: &str) {
        self.edges.entry(name.to_string()).or_default();
    }

    pub fn add_dependency(&mut self, block: &str, depends_on: &str) -> Result<(), DependencyError> {
        self.edges.entry(block.to_string()).or_default();
        self.edges.entry(depends_on.to_string()).or_default();

        if block == depends_on {
            return Err(DependencyError::CircularDependency(vec![block.to_string()]));
        }

        let mut visited = HashSet::new();
        visited.insert(depends_on.to_string());
        if self.would_create_cycle(block, depends_on, &mut visited) {
            return Err(DependencyError::CircularDependency(vec![
                depends_on.to_string(),
                block.to_string(),
            ]));
        }

        let deps = self.edges.get_mut(block).unwrap();
        if !deps.contains(&depends_on.to_string()) {
            deps.push(depends_on.to_string());
        }
        Ok(())
    }

    fn would_create_cycle(&self, from: &str, to: &str, visited: &mut HashSet<String>) -> bool {
        if from == to {
            return true;
        }
        if let Some(deps) = self.edges.get(to) {
            for dep in deps {
                if !visited.contains(dep) {
                    visited.insert(dep.clone());
                    if self.would_create_cycle(from, dep, visited) {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn dependencies_of(&self, block: &str) -> Vec<String> {
        self.edges.get(block).cloned().unwrap_or_default()
    }

    pub fn dependents_of(&self, block: &str) -> Vec<String> {
        self.edges
            .iter()
            .filter(|(_, deps)| deps.contains(&block.to_string()))
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn load_order(&self) -> Result<Vec<String>, DependencyError> {
        let mut dep_count: HashMap<String, usize> = HashMap::new();
        let mut reverse: HashMap<String, Vec<String>> = HashMap::new();

        for (block, deps) in &self.edges {
            dep_count.insert(block.clone(), deps.len());
            for dep in deps {
                reverse.entry(dep.clone()).or_default().push(block.clone());
            }
        }
        for name in self.edges.keys() {
            reverse.entry(name.clone()).or_default();
        }

        let mut queue: VecDeque<String> = VecDeque::new();
        for (name, &count) in &dep_count {
            if count == 0 {
                queue.push_back(name.clone());
            }
        }

        let mut result: Vec<String> = Vec::new();
        while let Some(block) = queue.pop_front() {
            result.push(block.clone());
            if let Some(dependents) = reverse.get(&block) {
                for dependent in dependents {
                    if let Some(count) = dep_count.get_mut(dependent) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            queue.push_back(dependent.clone());
                        }
                    }
                }
            }
        }

        if result.len() != self.edges.len() {
            let remaining: Vec<String> = self
                .edges
                .keys()
                .filter(|k| !result.contains(k))
                .cloned()
                .collect();
            return Err(DependencyError::CircularDependency(remaining));
        }

        Ok(result)
    }

    pub fn unload_order(&self) -> Result<Vec<String>, DependencyError> {
        let mut order = self.load_order()?;
        order.reverse();
        Ok(order)
    }

    pub fn blocks(&self) -> Vec<&str> {
        self.edges.keys().map(|s| s.as_str()).collect()
    }

    pub fn has_block(&self, name: &str) -> bool {
        self.edges.contains_key(name)
    }

    pub fn remove_block(&mut self, name: &str) {
        self.edges.remove(name);
        for deps in self.edges.values_mut() {
            deps.retain(|d| d != name);
        }
    }
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_block_and_dependency() {
        let mut graph = DependencyGraph::new();
        graph.add_block("hal");
        graph.add_block("ipc_bus");
        graph.add_dependency("scheduler", "ipc_bus").unwrap();
        assert!(graph.has_block("scheduler"));
        assert!(graph.has_block("ipc_bus"));
    }

    #[test]
    fn test_dependencies_of() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("scheduler", "ipc_bus").unwrap();
        graph.add_dependency("scheduler", "hal").unwrap();
        let deps = graph.dependencies_of("scheduler");
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&"ipc_bus".to_string()));
        assert!(deps.contains(&"hal".to_string()));
    }

    #[test]
    fn test_dependents_of() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("scheduler", "ipc_bus").unwrap();
        graph.add_dependency("watchdog", "ipc_bus").unwrap();
        let dependents = graph.dependents_of("ipc_bus");
        assert_eq!(dependents.len(), 2);
    }

    #[test]
    fn test_load_order_simple() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("scheduler", "ipc_bus").unwrap();
        graph.add_dependency("scheduler", "hal").unwrap();
        graph.add_dependency("watchdog", "scheduler").unwrap();
        let order = graph.load_order().unwrap();
        let hal_pos = order.iter().position(|n| n == "hal").unwrap();
        let ipc_pos = order.iter().position(|n| n == "ipc_bus").unwrap();
        let sched_pos = order.iter().position(|n| n == "scheduler").unwrap();
        let wd_pos = order.iter().position(|n| n == "watchdog").unwrap();
        assert!(hal_pos < sched_pos);
        assert!(ipc_pos < sched_pos);
        assert!(sched_pos < wd_pos);
    }

    #[test]
    fn test_unload_order_reversed() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("scheduler", "ipc_bus").unwrap();
        graph.add_dependency("scheduler", "hal").unwrap();
        let load = graph.load_order().unwrap();
        let unload = graph.unload_order().unwrap();
        assert_eq!(load.len(), unload.len());

        let pos = |order: &[String], name: &str| order.iter().position(|x| x == name).unwrap();
        assert!(pos(&load, "ipc_bus") < pos(&load, "scheduler"));
        assert!(pos(&load, "hal") < pos(&load, "scheduler"));
        assert!(pos(&unload, "scheduler") < pos(&unload, "ipc_bus"));
        assert!(pos(&unload, "scheduler") < pos(&unload, "hal"));
    }

    #[test]
    fn test_circular_dependency_detected() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("a", "b").unwrap();
        graph.add_dependency("b", "c").unwrap();
        let result = graph.add_dependency("c", "a");
        assert!(result.is_err());
    }

    #[test]
    fn test_self_dependency() {
        let mut graph = DependencyGraph::new();
        let result = graph.add_dependency("a", "a");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_order_no_deps() {
        let mut graph = DependencyGraph::new();
        graph.add_block("a");
        graph.add_block("b");
        graph.add_block("c");
        let order = graph.load_order().unwrap();
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn test_remove_block() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("a", "b").unwrap();
        graph.add_dependency("c", "a").unwrap();
        graph.remove_block("a");
        assert!(!graph.has_block("a"));
        let deps_c = graph.dependencies_of("c");
        assert!(!deps_c.contains(&"a".to_string()));
    }
}
