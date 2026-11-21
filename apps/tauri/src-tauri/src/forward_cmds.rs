//! Port-forward commands: start and stop a vault [`ForwardRule`] on its
//! host's shared connection. Every status change goes out as a `forward`
//! event (`{ruleId, status}`); a rule whose bind address is taken becomes
//! `{"state": "error", "detail": "address in use"}` without affecting other
//! rules.

use std::sync::Arc;
use std::time::Duration;

use core_api::ForwardStatus;
use core_model::{ForwardRule, Id};
use core_ssh::NoProgress;
use serde::Serialize;
use tauri::State;

use crate::state::{AppState, CmdError, lock};

/// How long `forward_start` waits for the rule to leave `Starting` before
/// returning that status (later changes still arrive as events).
const START_SETTLE: Duration = Duration::from_secs(10);

/// A started rule's current status.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardStatusEntry {
    pub rule_id: Id,
    pub host_id: Id,
    pub status: ForwardStatus,
}

impl AppState {
    /// Starts rule `rule_id` (connecting to its host if needed) and returns
    /// its status once it is active or failed.
    pub async fn forward_start(&self, rule_id: Id) -> Result<ForwardStatus, CmdError> {
        let rule = self
            .with_vault(move |v| {
                v.get::<ForwardRule>(rule_id)?
                    .ok_or_else(|| CmdError::not_found(format!("forward rule {rule_id}")))
            })
            .await?;
        let (guard, conn) = self
            .host_connection(rule.host_id, Arc::new(NoProgress))
            .await?;
        lock(&self.0.forward_hosts).insert(rule.id, rule.host_id);
        let mut handle = self.0.forwards.start(&rule, conn);
        drop(guard);
        Ok(
            match tokio::time::timeout(START_SETTLE, handle.wait_settled()).await {
                Ok(status) => status,
                Err(_) => handle.status(),
            },
        )
    }

    /// Stops a rule and waits until its listener and connections are gone.
    pub async fn forward_stop(&self, rule_id: Id) {
        self.0.forwards.stop(rule_id).await;
        let host = lock(&self.0.forward_hosts).get(&rule_id).copied();
        if let Some(host) = host {
            self.release_if_idle(host).await;
        }
    }

    /// Status of every rule started in this run.
    pub fn forward_statuses(&self) -> Vec<ForwardStatusEntry> {
        let started: Vec<(Id, Id)> = lock(&self.0.forward_hosts)
            .iter()
            .map(|(r, h)| (*r, *h))
            .collect();
        let mut out: Vec<ForwardStatusEntry> = started
            .into_iter()
            .filter_map(|(rule_id, host_id)| {
                self.0
                    .forwards
                    .status(rule_id)
                    .map(|status| ForwardStatusEntry {
                        rule_id,
                        host_id,
                        status,
                    })
            })
            .collect();
        out.sort_by_key(|e| e.rule_id);
        out
    }
}

#[tauri::command]
pub async fn forward_start(
    state: State<'_, AppState>,
    rule_id: Id,
) -> Result<ForwardStatus, CmdError> {
    state.forward_start(rule_id).await
}

#[tauri::command]
pub async fn forward_stop(state: State<'_, AppState>, rule_id: Id) -> Result<(), CmdError> {
    state.forward_stop(rule_id).await;
    Ok(())
}

#[tauri::command]
pub async fn forward_statuses(
    state: State<'_, AppState>,
) -> Result<Vec<ForwardStatusEntry>, CmdError> {
    Ok(state.forward_statuses())
}
