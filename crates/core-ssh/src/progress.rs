//! Connect-phase reporting, so a UI can show which step a connect is on and
//! which one failed.

use serde::Serialize;

/// A step of connecting a session. The SSH layer reports the first three;
/// the app reports `Shell` once it opens the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectStep {
    /// Opening the TCP connection.
    Tcp,
    /// Key exchange and host-key verification.
    HostKey,
    /// Authentication.
    Auth,
    /// Opening the shell channel.
    Shell,
}

/// Receives each [`ConnectStep`] as it begins. Called from the connecting
/// task, so it must not block.
pub trait ConnectProgress: Send + Sync {
    fn step(&self, step: ConnectStep);
}

/// Ignores every step.
pub struct NoProgress;

impl ConnectProgress for NoProgress {
    fn step(&self, _step: ConnectStep) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_serialize_camel_case() {
        let json = serde_json::to_string(&[
            ConnectStep::Tcp,
            ConnectStep::HostKey,
            ConnectStep::Auth,
            ConnectStep::Shell,
        ])
        .unwrap();
        assert_eq!(json, r#"["tcp","hostKey","auth","shell"]"#);
    }
}
