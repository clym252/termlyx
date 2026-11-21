//! Prompts: questions for the user go out as `prompt` events, answers come
//! back through `prompt_respond`, and a prompt that ends unanswered (timeout,
//! or its asker gave up) is announced with `prompt-withdrawn` so the UI can
//! close a stale dialog.

use std::sync::Arc;
use std::time::Duration;

use core_api::{PROMPT_TIMEOUT, Prompt, PromptBridge, PromptResponse};
use serde::Serialize;
use tauri::State;

use crate::state::{AppState, CmdError, EventSink, events};

/// Payload of the `prompt` event: answer it with `prompt_respond`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptEvent {
    pub id: String,
    pub prompt: Prompt,
}

/// Payload of the `prompt-withdrawn` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptWithdrawn {
    pub id: String,
}

/// The app's prompt bridge: prompts and withdrawals go to `events`.
pub fn prompt_bridge(events: Arc<dyn EventSink>) -> PromptBridge {
    prompt_bridge_with_timeout(events, PROMPT_TIMEOUT)
}

/// [`prompt_bridge`] with another timeout (tests).
pub fn prompt_bridge_with_timeout(events: Arc<dyn EventSink>, timeout: Duration) -> PromptBridge {
    let withdraw_events = events.clone();
    PromptBridge::with_timeout(
        Arc::new(move |id, prompt| {
            let payload =
                serde_json::to_value(PromptEvent { id, prompt }).map_err(|e| e.to_string())?;
            // A failed emit can never be answered: the bridge rejects at once.
            events.emit(events::PROMPT, payload)
        }),
        timeout,
    )
    .on_withdrawn(Arc::new(move |id| {
        let payload = serde_json::json!(PromptWithdrawn { id });
        if let Err(e) = withdraw_events.emit(events::PROMPT_WITHDRAWN, payload) {
            tracing::debug!(error = %e, "prompt-withdrawn not delivered");
        }
    }))
}

impl AppState {
    /// Answers prompt `id`; unknown or expired ids are ignored.
    pub fn prompt_respond(&self, id: &str, response: PromptResponse) {
        self.prompts().respond(id, response);
    }
}

/// Answers a `prompt` event. Secrets in `response` deserialize straight
/// into `SecretString`.
#[tauri::command]
pub async fn prompt_respond(
    state: State<'_, AppState>,
    id: String,
    response: PromptResponse,
) -> Result<(), CmdError> {
    state.prompt_respond(&id, response);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn prompt_event_serializes_camel_case() {
        let ev = PromptEvent {
            id: "p1".into(),
            prompt: Prompt::Password {
                user: "u".into(),
                host: "h".into(),
                save_to: None,
            },
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::json!({"id": "p1", "prompt": {"kind": "password", "user": "u", "host": "h", "saveTo": null}})
        );
    }

    #[test]
    fn prompt_respond_args_deserialize_into_secret_string() {
        #[derive(Deserialize)]
        struct Args {
            id: String,
            response: PromptResponse,
        }
        let args: Args =
            serde_json::from_str(r#"{"id":"p1","response":{"secret":"hunter2"}}"#).unwrap();
        assert_eq!(args.id, "p1");
        assert!(!format!("{:?}", args.response).contains("hunter2"));
        let PromptResponse::Secret(s) = args.response else {
            panic!("expected a secret");
        };
        use secrecy::ExposeSecret;
        assert_eq!(s.expose_secret(), "hunter2");
    }
}
