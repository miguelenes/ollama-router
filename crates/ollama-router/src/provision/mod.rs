//! russh + Tailscale handoff (no OpenSSH binary in the image).

mod orchestrator;
mod ssh;
mod watcher;

pub use orchestrator::{
    adopt_with_tailscale, provision_new_tailscale, MockTransport, ProvisionOrchestrator, SshTarget,
};
pub use ssh::{RemoteOutput, RusshTransport};
pub use watcher::ProvisionWatcher;

#[cfg(test)]
mod tests;
