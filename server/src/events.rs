//! `GET /v1/events`: the change-notification WebSocket.
//!
//! Authentication happens before the upgrade, with the access token the client
//! offers as the subprotocol `bearer.<token>` (browsers and some WebSocket
//! clients cannot set an `Authorization` header). The server answers with
//! `tether.v1` and never echoes the bearer subprotocol. A missing or bad token
//! is refused with an ordinary error response (`401 unauthorized`, `401
//! revoked`, `403 forbidden`) instead of an upgrade.
//!
//! Each text frame is one [`core_proto::Event`] as JSON. The server pings every
//! [`crate::state::Limits::ws_ping`] (30 s), drops a peer that has been silent
//! for two intervals, and on every tick re-checks that the device is still
//! valid, so a socket opened before a revoke closes even if the `revoked`
//! message was missed.

use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::http::header::SEC_WEBSOCKET_PROTOCOL;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use core_proto::{ErrorCode, Event, WS_PROTOCOL, parse_ws_bearer_protocol};
use tokio::time::{Instant, MissedTickBehavior, interval_at};

use crate::auth::{AuthDevice, authenticate_token};
use crate::error::ApiError;
use crate::state::{AppState, Subscription};

/// Largest message a peer may send. Clients have nothing to say on this
/// socket, so anything beyond a ping payload is already too much.
const MAX_INCOMING: usize = 4096;

/// The subprotocols a client offered, across all `Sec-WebSocket-Protocol`
/// headers.
fn offered(headers: &HeaderMap) -> Vec<&str> {
    headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

async fn events(
    State(st): State<AppState>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let protocols = offered(&headers);
    let Some(token) = protocols.iter().find_map(|p| parse_ws_bearer_protocol(p)) else {
        return ApiError::of(ErrorCode::Unauthorized).into_response();
    };
    let auth = match authenticate_token(&st, token).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let upgrade = match upgrade {
        Ok(u) => u,
        Err(rejection) => return rejection.into_response(),
    };
    if !protocols.contains(&WS_PROTOCOL) {
        return ApiError::bad_request("offer the tether.v1 WebSocket subprotocol").into_response();
    }
    // Subscribe before the upgrade completes, so nothing published in between
    // is missed.
    let sub = st.events.subscribe(&auth.vault_id, &auth.device_id);
    upgrade
        .protocols([WS_PROTOCOL])
        .max_message_size(MAX_INCOMING)
        .max_frame_size(MAX_INCOMING)
        .on_upgrade(move |socket| run(socket, st, auth, sub))
}

/// Whether a device may still listen.
enum Standing {
    Valid,
    /// Revoked: it is told, then dropped.
    Revoked,
    /// Deleted, or its account was disabled: just dropped.
    Gone,
}

async fn standing(st: &AppState, device_id: &str) -> Standing {
    let row: Result<Option<(i64, i64)>, _> = sqlx::query_as(
        "SELECT d.revoked, a.disabled FROM devices d JOIN accounts a ON a.id = d.account_id \
         WHERE d.id = ?",
    )
    .bind(device_id)
    .fetch_optional(&st.db)
    .await;
    match row {
        Ok(Some((0, 0))) => Standing::Valid,
        Ok(Some((revoked, _))) if revoked != 0 => Standing::Revoked,
        Ok(_) => Standing::Gone,
        Err(e) => {
            // A hiccup must not drop a healthy socket; the next tick retries.
            tracing::warn!(error = %e, "could not re-check a device for its socket");
            Standing::Valid
        }
    }
}

async fn send_event(socket: &mut WebSocket, event: &Event) -> bool {
    match serde_json::to_string(event) {
        Ok(text) => socket.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

async fn run(mut socket: WebSocket, st: AppState, auth: AuthDevice, mut sub: Subscription) {
    tracing::info!(account = %auth.account_id, device = %auth.device_id, "events socket opened");
    let every = st.limits.ws_ping;
    let mut tick = interval_at(Instant::now() + every, every);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Set when a ping goes out, cleared by any frame from the peer.
    let mut awaiting = false;

    // The device may have been revoked between authentication and now.
    let mut revoked = matches!(standing(&st, &auth.device_id).await, Standing::Revoked);
    let mut open = !matches!(standing(&st, &auth.device_id).await, Standing::Gone) || revoked;

    if revoked {
        send_event(&mut socket, &Event::Revoked).await;
    }
    while open && !revoked {
        tokio::select! {
            event = sub.recv() => match event {
                Some(event) => {
                    open = send_event(&mut socket, &event).await;
                    revoked = event == Event::Revoked;
                }
                // Missed events: have the client reconnect and sync.
                None => open = false,
            },
            frame = socket.recv() => match frame {
                None | Some(Err(_) | Ok(Message::Close(_))) => open = false,
                // Anything else, a pong included, proves the peer is alive.
                // Ping frames are answered by the library.
                Some(Ok(_)) => awaiting = false,
            },
            _ = tick.tick() => {
                if awaiting {
                    tracing::info!(device = %auth.device_id, "events socket dropped: peer silent");
                    open = false;
                    continue;
                }
                match standing(&st, &auth.device_id).await {
                    Standing::Valid => {
                        awaiting = true;
                        open = socket.send(Message::Ping(Vec::new().into())).await.is_ok();
                    }
                    Standing::Revoked => {
                        send_event(&mut socket, &Event::Revoked).await;
                        revoked = true;
                    }
                    Standing::Gone => open = false,
                }
            }
        }
    }
    let _ = socket.send(Message::Close(None)).await;
    tracing::info!(account = %auth.account_id, device = %auth.device_id, "events socket closed");
}

/// Routes of this module.
pub fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/v1/events", get(events))
}
