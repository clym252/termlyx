use std::sync::Arc;

use core_api::OutputSink;
use core_ssh::{ConnectProgress, ConnectStep};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::state::Reaper;

/// How a session ended, sent once on its `onClosed` channel.
///
/// `reason: None` is a normal end (the remote shell exited, with
/// `exitStatus` when the server reported it, or the session was closed);
/// `reason: Some(why)` means the connection was lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionClosed {
    pub reason: Option<String>,
    pub exit_status: Option<u32>,
}

/// Forwards session output to the webview over two Tauri channels.
///
/// Output frames go out as `InvokeResponseBody::Raw`, which the JS side
/// receives as an `ArrayBuffer` (no JSON number-array encoding). Batching
/// (~16 ms frames) and ack-based backpressure happen upstream in
/// `core_api::SessionManager`.
pub struct ChannelSink {
    output: Channel<InvokeResponseBody>,
    closed: Channel<SessionClosed>,
    on_dead: Option<Reaper>,
}

impl ChannelSink {
    pub fn new(output: Channel<InvokeResponseBody>, closed: Channel<SessionClosed>) -> Self {
        Self {
            output,
            closed,
            on_dead: None,
        }
    }

    /// Closes the session through `reaper` when a frame cannot be delivered
    /// (the webview side of the channel is gone): a backstop for sessions
    /// orphaned by a reload, besides the UI's `close_all_sessions` on load.
    pub fn on_dead(mut self, reaper: Reaper) -> Self {
        self.on_dead = Some(reaper);
        self
    }
}

impl OutputSink for ChannelSink {
    fn frame(&self, bytes: Vec<u8>) {
        if let Err(e) = self.output.send(InvokeResponseBody::Raw(bytes)) {
            tracing::debug!("dropping output frame: {e}");
            if let Some(reaper) = &self.on_dead {
                reaper.reap();
            }
        }
    }

    fn closed(&self, reason: Option<String>, exit_status: Option<u32>) {
        if let Err(e) = self.closed.send(SessionClosed {
            reason,
            exit_status,
        }) {
            tracing::debug!("dropping closed notification: {e}");
        }
    }
}

/// Forwards connect steps to the webview on a session's `onProgress`
/// channel. A failed send is ignored: a gone webview ends the session
/// through the output channel, and progress is only for display.
pub struct ChannelProgress(pub Channel<ConnectStep>);

impl ConnectProgress for ChannelProgress {
    fn step(&self, step: ConnectStep) {
        if let Err(e) = self.0.send(step) {
            tracing::debug!("dropping connect step: {e}");
        }
    }
}

impl ChannelProgress {
    pub fn shared(channel: &Channel<ConnectStep>) -> Arc<dyn ConnectProgress> {
        Arc::new(Self(channel.clone()))
    }
}
