use crate::capability::{BackendKind, CapabilitySet};
use crate::output::CommandOutput;
use crate::registry::{resolve, Invocation};

/// Executes resolved commands against one concrete environment.
pub trait CommandBackend {
    /// Environment this backend represents.
    fn kind(&self) -> BackendKind;

    /// Capabilities granted to the current session.
    fn capabilities(&self) -> &CapabilitySet;

    /// Short backend name used in diagnostics.
    fn name(&self) -> &'static str;

    /// Executes one resolved invocation.
    fn execute(&mut self, invocation: &Invocation) -> CommandOutput;
}

/// Verifies scope and capabilities, then calls the backend.
pub fn dispatch<B: CommandBackend>(backend: &mut B, input: &str) -> CommandOutput {
    let invocation = match resolve(input) {
        Ok(invocation) => invocation,
        Err(error) => return error.into_output(),
    };
    dispatch_invocation(backend, &invocation)
}

/// Verifies scope and capabilities for an already resolved invocation.
pub fn dispatch_invocation<B: CommandBackend>(
    backend: &mut B,
    invocation: &Invocation,
) -> CommandOutput {
    if !invocation.spec.scope.supports(backend.kind()) {
        return CommandOutput::unavailable(&alloc::format!(
            "'{}' is only available on the {} backend (active: {})",
            invocation.spec.id,
            scope_backend_name(invocation.spec.scope),
            backend.kind().as_str()
        ));
    }

    for capability in invocation.spec.capabilities {
        if !backend.capabilities().has(*capability) {
            return CommandOutput::denied(&alloc::format!(
                "'{}' requires capability {:?}",
                invocation.spec.id,
                capability
            ));
        }
    }

    backend.execute(invocation)
}

fn scope_backend_name(scope: crate::capability::Scope) -> &'static str {
    match scope {
        crate::capability::Scope::Host => "host",
        crate::capability::Scope::Kernel => "kernel",
        crate::capability::Scope::Both => "host or kernel",
    }
}
